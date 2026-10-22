//! The audio thread: opens the device, then renders at each wake-up.

use std::slice;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use musicable_engine::{Engine, rt};
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::AUDCLNT_E_BUFFER_TOO_LARGE;
use windows::Win32::System::Threading::WaitForMultipleObjects;

use super::capture::{self, Output};
use super::client::{self, Client};
use super::com::{Call, Com, Event, Failed, Mmcss, Qpc, device_gone, os};
use super::device;
use crate::input::{self, Bus, Playback, StreamInput};
use crate::midi::MidiBus;
use crate::period::frames_to_hns;
use crate::stats::Counters;
use crate::xrun::{Detector, Medians, Reading};
use crate::{
    Error, Feeds, SampleFormat, StartError, Stopped, StreamInfo, StreamRequest, StreamStats,
};

/// How long the audio thread waits for a wake-up before checking that the
/// device is still there, in milliseconds.
const WAIT_MS: u32 = 250;
/// How long the device can go without a wake-up or an error before the
/// stream gives up on it.
const GIVE_UP_MS: u32 = 2000;

struct Shared {
    counters: Counters,
    /// Set to stop the audio thread.
    stop: Event,
}

/// What the audio thread hands back when it ends.
#[derive(Default)]
struct Ended {
    engine: Option<Engine>,
    error: Option<Error>,
}

/// What the audio thread reports once the device and the inputs opened.
type Opened = (StreamInfo, Vec<StreamInput>);

pub(crate) fn open(
    request: &StreamRequest,
    feeds: Feeds,
) -> Result<(StreamInfo, Vec<StreamInput>, OpenStream), Error> {
    let shared = Arc::new(Shared {
        counters: Counters::default(),
        stop: Event::manual_reset()?,
    });
    let (opened_tx, opened) = mpsc::sync_channel(1);
    let (start, start_rx) = mpsc::sync_channel(1);
    let (started_tx, started) = mpsc::sync_channel(1);
    let request = request.clone();
    let thread_shared = shared.clone();
    let thread = thread::Builder::new()
        .name("musicable-audio".into())
        .spawn(move || {
            audio_thread(
                &request,
                feeds,
                &thread_shared,
                &opened_tx,
                &start_rx,
                &started_tx,
            )
        })
        .map_err(|e| Error::Os {
            call: "spawning the audio thread",
            message: e.to_string(),
        })?;
    match opened.recv() {
        Ok(Ok((info, inputs))) => Ok((
            info,
            inputs,
            OpenStream {
                thread: Some(thread),
                start: Some(start),
                started,
                shared,
            },
        )),
        Ok(Err(error)) => {
            join(thread);
            Err(error)
        }
        Err(_) => {
            // Only a panic ends the thread without a word, and joining
            // passes the panic on.
            join(thread);
            unreachable!("the audio thread reports whether the device opened")
        }
    }
}

fn audio_thread(
    request: &StreamRequest,
    feeds: Feeds,
    shared: &Shared,
    opened: &SyncSender<Result<Opened, Error>>,
    start: &Receiver<Engine>,
    started: &SyncSender<Result<(), Error>>,
) -> Ended {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = opened.send(Err(error));
            return Ended::default();
        }
    };
    // First, so the period check runs at the priority the stream will.
    let mmcss = Mmcss::join();
    let (client, mut info, container) = match open_device(request, mmcss.is_some()) {
        Ok(opened) => opened,
        Err(error) => {
            let _ = opened.send(Err(error));
            return Ended::default();
        }
    };
    // The inputs start capturing now, and line up with the output once it
    // plays. They stop when this thread ends, as `captures` drops.
    let Feeds { playback, midi } = feeds;
    let output = Output {
        id: info.device.id.clone(),
        container,
        rate: info.sample_rate,
        exclusive: client.exclusive(),
        clock: playback.clone(),
    };
    let captures = capture::open(&request.inputs, &output);
    let opened_inputs = captures
        .iter()
        .map(|capture| {
            capture
                .as_ref()
                .map(|c| c.input.clone())
                .map_err(Clone::clone)
        })
        .collect();
    let (infos, stream_inputs, bus_inputs) = input::assemble(&request.inputs, opened_inputs);
    let bus = Bus::new(
        bus_inputs,
        client.buffer as usize,
        input::max_lag(info.sample_rate),
    );
    info.inputs = infos;
    if opened.send(Ok((info, stream_inputs))).is_err() {
        return Ended::default();
    }
    // Wait for the engine. Closing the stream without starting it drops the
    // sender instead.
    let Ok(mut engine) = start.recv() else {
        return Ended::default();
    };
    let mut runner = Runner::new(&client, &shared.counters, bus, midi, &playback);
    if let Err(error) = runner.start(&mut engine) {
        let _ = started.send(Err(error));
        return Ended {
            engine: Some(engine),
            error: None,
        };
    }
    let _ = started.send(Ok(()));
    let error = runner.run(&mut engine, &shared.stop).err();
    // SAFETY: an initialized client. Stopping it twice is harmless.
    let _ = unsafe { client.audio.Stop() };
    shared.counters.end();
    Ended {
        engine: Some(engine),
        error,
    }
}

/// Opens the output, and says which physical device it belongs to.
fn open_device(
    request: &StreamRequest,
    realtime: bool,
) -> Result<(Client, StreamInfo, Option<windows::core::GUID>), Error> {
    let enumerator = device::enumerator()?;
    let endpoint = device::find(&enumerator, request.device.as_deref())?;
    let described = device::describe(&endpoint, device::default_id(&enumerator).as_deref())?;
    let client = client::open(&endpoint, request)?;
    let info = client.info(described, realtime);
    Ok((client, info, device::container_id(&endpoint)))
}

/// The stream, as the audio thread runs it.
struct Runner<'a> {
    client: &'a Client,
    counters: &'a Counters,
    /// The inputs, for the engine.
    bus: Bus,
    /// The MIDI inputs, for the engine.
    midi: MidiBus,
    /// Where the inputs learn when each engine frame plays.
    playback: &'a Playback,
    /// The engine frame the stream started with: the one the device's
    /// position 0 plays.
    first_frame: u64,
    detector: Detector,
    /// The frames queued ahead of the play position, for their median.
    latency: Medians,
    qpc: Qpc,
    /// Where the engine renders when it can't write to the device buffer
    /// directly: for integer formats, or an unaligned buffer.
    scratch: Vec<f32>,
    /// Frames written since the stream started.
    written: u64,
    last_wakeup: Option<u64>,
    /// A period in 100 ns units.
    period_hns: u64,
}

impl<'a> Runner<'a> {
    fn new(
        client: &'a Client,
        counters: &'a Counters,
        bus: Bus,
        midi: MidiBus,
        playback: &'a Playback,
    ) -> Self {
        let format = client.format;
        Self {
            client,
            counters,
            bus,
            midi,
            playback,
            first_frame: 0,
            detector: Detector::new(client.exclusive(), client.period, format.rate),
            latency: Medians::default(),
            qpc: Qpc::new(),
            scratch: vec![0.0; client.buffer as usize * usize::from(format.channels)],
            written: 0,
            last_wakeup: None,
            period_hns: frames_to_hns(u64::from(client.period), format.rate),
        }
    }

    /// Fills the buffer, so the first period plays audio, and starts.
    fn start(&mut self, engine: &mut Engine) -> Result<(), Error> {
        self.first_frame = engine.time();
        let target = self.client.target;
        rt::scope(|| self.write(engine, target))?;
        // SAFETY: an initialized client with its buffer written.
        unsafe { self.client.audio.Start() }.call("IAudioClient::Start")?;
        Ok(())
    }

    /// Serves wake-ups until the stop event is set or the device fails.
    fn run(&mut self, engine: &mut Engine, stop: &Event) -> Result<(), Error> {
        let events = [stop.0, self.client.event.0];
        let mut waited = 0;
        loop {
            // SAFETY: both events stay open for the whole loop.
            let woke = unsafe { WaitForMultipleObjects(&events, false, WAIT_MS) };
            if woke == WAIT_OBJECT_0 {
                return Ok(());
            } else if woke.0 == WAIT_OBJECT_0.0 + 1 {
                waited = 0;
                rt::scope(|| self.serve(engine))?;
            } else if woke == WAIT_TIMEOUT {
                // If the device went away, WASAPI says so now.
                // SAFETY: an initialized client.
                unsafe { self.client.audio.GetCurrentPadding() }
                    .call("IAudioClient::GetCurrentPadding")?;
                waited += WAIT_MS;
                if waited >= GIVE_UP_MS {
                    return Err(Error::Timeout);
                }
            } else {
                let error = windows::core::Error::from_thread();
                return Err(os("WaitForMultipleObjects", error));
            }
        }
    }

    /// One wake-up: look for a dropout on the device clock, then render and
    /// write what the device has room for. Real-time safe.
    fn serve(&mut self, engine: &mut Engine) -> Result<(), Failed> {
        let woke = self.qpc.now();
        if let Some(last) = self.last_wakeup.replace(woke) {
            let interval = woke.saturating_sub(last);
            self.counters.interval(hns_duration(interval));
            if interval * 2 > self.period_hns * 3 {
                self.counters.late_wakeup();
            }
        }
        let client = self.client;
        let padding = if client.exclusive() {
            0
        } else {
            // SAFETY: an initialized client.
            unsafe { client.audio.GetCurrentPadding() }.call("IAudioClient::GetCurrentPadding")?
        };
        self.check_clock(engine, padding)?;
        let frames = if client.exclusive() {
            client.buffer
        } else {
            client.target.saturating_sub(padding)
        };
        if frames > 0 {
            match self.write(engine, frames) {
                Ok(Some(queued)) => self.detector.landed(u64::from(queued)),
                Ok(None) => {}
                // Exclusive mode, woken with no half free: nothing to write.
                Err(failed)
                    if client.exclusive() && failed.code() == AUDCLNT_E_BUFFER_TOO_LARGE => {}
                Err(failed) => return Err(failed),
            }
        }
        self.counters.wakeup();
        self.counters
            .busy(hns_duration(self.qpc.now().saturating_sub(woke)));
        Ok(())
    }

    fn check_clock(&mut self, engine: &Engine, padding: u32) -> Result<(), Failed> {
        let (mut position, mut qpc) = (0, 0);
        // SAFETY: an initialized client.
        match unsafe { self.client.clock.GetPosition(&mut position, Some(&mut qpc)) } {
            Ok(()) => {
                let rate = u128::from(self.client.format.rate);
                let played = u128::from(position) * rate / u128::from(self.client.clock_rate);
                let played = played as u64;
                let outcome = self.detector.observe(Reading {
                    played,
                    qpc,
                    written: self.written,
                    padding: u64::from(padding),
                });
                if outcome.underrun {
                    engine.record_xrun();
                    self.counters.underrun();
                    if outcome.system {
                        self.counters.system_dropout();
                    }
                }
                self.counters.lost(outcome.lost);
                if let Some(queued) = self.latency.push(self.written as i64 - played as i64) {
                    self.counters.queued(queued.max(0) as u64);
                }
                // The clock stands at zero until the device starts playing.
                if played > 0 {
                    let frame = (self.first_frame + played).saturating_sub(self.detector.shift());
                    // QPC times come in 100 ns units.
                    let rate = self.client.format.rate;
                    self.playback.publish(frame, qpc.saturating_mul(100), rate);
                }
                Ok(())
            }
            Err(error) if device_gone(error.code()) => Err(Failed {
                call: "IAudioClock::GetPosition",
                error,
            }),
            // No reading this time: compare afresh from the next.
            Err(_) => {
                self.detector.reset();
                Ok(())
            }
        }
    }

    /// Renders `frames` frames into the device buffer. In shared mode,
    /// returns how many frames were still queued as the write landed: a
    /// render that took too long drains the buffer even after a punctual
    /// wake-up, and this is where it shows.
    fn write(&mut self, engine: &mut Engine, frames: u32) -> Result<Option<u32>, Failed> {
        let format = self.client.format;
        let channels = usize::from(format.channels);
        let samples = frames as usize * channels;
        // SAFETY: an initialized client, and `frames` fits in the free space.
        let data = unsafe { self.client.render.GetBuffer(frames) }
            .call("IAudioRenderClient::GetBuffer")?;
        let time = engine.time();
        let input = self.bus.fill(time, frames as usize);
        // QPC times come in 100 ns units.
        let now = self.qpc.now().saturating_mul(100);
        let heard = self.playback.reading().map(|r| r.frame_at(now));
        let midi = self.midi.fill(time, frames as usize, heard, format.rate);
        if format.sample == SampleFormat::F32 && data.cast::<f32>().is_aligned() {
            // SAFETY: WASAPI handed over `frames` frames of `channels` f32
            // samples, aligned as checked, until `ReleaseBuffer`.
            let out = unsafe { slice::from_raw_parts_mut(data.cast::<f32>(), samples) };
            engine.process_interleaved_with_input(input, midi, out, channels);
        } else {
            let scratch = &mut self.scratch[..samples];
            engine.process_interleaved_with_input(input, midi, scratch, channels);
            // SAFETY: as above, in the stream's sample format.
            let out = unsafe { slice::from_raw_parts_mut(data, samples * format.sample.bytes()) };
            format.sample.write(scratch, out);
        }
        let queued = if self.client.exclusive() {
            None
        } else {
            // SAFETY: an initialized client.
            let padding = unsafe { self.client.audio.GetCurrentPadding() }
                .call("IAudioClient::GetCurrentPadding")?;
            Some(padding)
        };
        // SAFETY: releases all that `GetBuffer` handed over, written.
        unsafe { self.client.render.ReleaseBuffer(frames, 0) }
            .call("IAudioRenderClient::ReleaseBuffer")?;
        self.written += u64::from(frames);
        self.counters.wrote(frames);
        self.counters.inputs(self.bus.delay(), self.bus.late());
        self.counters.midi(self.midi.delay(), self.midi.late());
        Ok(queued)
    }
}

fn hns_duration(hns: u64) -> Duration {
    Duration::from_nanos(hns.saturating_mul(100))
}

fn join(thread: JoinHandle<Ended>) -> Ended {
    thread
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

pub(crate) struct OpenStream {
    thread: Option<JoinHandle<Ended>>,
    start: Option<SyncSender<Engine>>,
    started: Receiver<Result<(), Error>>,
    shared: Arc<Shared>,
}

impl OpenStream {
    pub fn start(mut self, engine: Engine) -> Result<Stream, StartError> {
        let thread = self.thread.take().expect("an open stream has its thread");
        let start = self.start.take().expect("an open stream hasn't started");
        if let Err(mpsc::SendError(engine)) = start.send(engine) {
            // The thread is gone, which only a panic does: joining passes it
            // on.
            join(thread);
            return Err(StartError {
                engine: Box::new(engine),
                error: Error::Os {
                    call: "starting the stream",
                    message: "the audio thread ended".into(),
                },
            });
        }
        match self.started.recv() {
            Ok(Ok(())) => Ok(Stream {
                thread: Some(thread),
                shared: self.shared.clone(),
            }),
            Ok(Err(error)) => {
                let engine = join(thread)
                    .engine
                    .expect("the audio thread returns the engine");
                Err(StartError {
                    engine: Box::new(engine),
                    error,
                })
            }
            Err(_) => {
                join(thread);
                unreachable!("the audio thread reports whether the stream started")
            }
        }
    }
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        // Dropping the sender tells the audio thread to close the device.
        drop(self.start.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct Stream {
    thread: Option<JoinHandle<Ended>>,
    shared: Arc<Shared>,
}

impl Stream {
    pub fn stats(&self) -> StreamStats {
        self.shared.counters.snapshot()
    }

    pub fn has_ended(&self) -> bool {
        self.shared.counters.ended()
    }

    pub fn stop(mut self) -> Stopped {
        self.shared.stop.set();
        let ended = join(
            self.thread
                .take()
                .expect("a stream has its thread until stopped"),
        );
        Stopped {
            engine: ended.engine.expect("the audio thread returns the engine"),
            error: ended.error,
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.shared.stop.set();
            // The engine comes back with the thread and is dropped here.
            let _ = thread.join();
        }
    }
}
