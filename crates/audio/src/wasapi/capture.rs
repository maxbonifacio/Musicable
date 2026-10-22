//! Inputs: each one captures on a thread of its own, taking packets as
//! WASAPI hands them over and lining them up with the output as they come.
//! `docs/audio.md` describes how.

use std::ptr;
use std::slice;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use musicable_engine::rt;
use windows::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
    IAudioClient, IAudioRenderClient, IMMDevice,
};
use windows::Win32::System::Threading::WaitForMultipleObjects;
use windows::core::GUID;

use super::client::{self, Format};
use super::com::{Call, Com, Event, Failed, Mmcss, os};
use super::device;
use crate::input::{
    self, Conformer, InputBuffer, InputClock, InputCounters, OpenedInput, Packet, Playback,
};
use crate::period::hns_to_frames;
use crate::{Error, SampleFormat};

/// How long a capture thread waits for WASAPI to say a packet is ready
/// before it looks anyway, in milliseconds. Some systems never signal
/// loopback streams.
const WAIT_MS: u32 = 10;

/// The capture buffer, in 100 ns units: how long a capture thread can be
/// held up before the device loses audio. It adds no latency, since
/// packets are taken as soon as they're ready.
const BUFFER_HNS: i64 = 2_000_000;

/// The container id Windows gives every device built into the computer,
/// which says nothing about the clocks they run on.
const BUILT_IN: GUID = GUID::from_u128(0x0000_0000_0000_0000_ffff_ffff_ffff_ffff);

/// The output, as its inputs line up with it.
#[derive(Clone)]
pub(crate) struct Output {
    /// The output endpoint's id.
    pub id: String,
    /// The physical device it belongs to, if Windows says.
    pub container: Option<GUID>,
    pub rate: u32,
    pub exclusive: bool,
    /// When the output plays each engine frame.
    pub clock: Arc<Playback>,
}

/// An input capturing on a thread of its own, until dropped.
pub(crate) struct Capture {
    pub input: OpenedInput,
    stop: Arc<Event>,
    thread: Option<JoinHandle<()>>,
}

/// A capture thread opening its device.
struct Pending {
    opened: Receiver<Result<Opened, String>>,
    counters: Arc<InputCounters>,
    stop: Arc<Event>,
    thread: JoinHandle<()>,
}

/// What a capture thread reports once its device opened.
struct Opened {
    name: String,
    channels: u16,
    rate: u32,
    clock: InputClock,
    buffer: InputBuffer,
    /// Frames per packet, in engine frames.
    packet: u64,
}

/// Opens the inputs `ids`, side by side, and starts them capturing, lined
/// up with `output`. Each one that doesn't open says why.
pub(crate) fn open(ids: &[String], output: &Output) -> Vec<Result<Capture, String>> {
    let pending: Vec<_> = ids.iter().map(|id| Pending::spawn(id, output)).collect();
    pending
        .into_iter()
        .map(|pending| pending.and_then(Pending::wait))
        .collect()
}

impl Pending {
    fn spawn(id: &str, output: &Output) -> Result<Pending, String> {
        let stop = Arc::new(Event::manual_reset().map_err(|e| e.to_string())?);
        let counters = Arc::new(InputCounters::default());
        let (opened_tx, opened) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("musicable-input".into())
            .spawn({
                let (id, output) = (id.to_owned(), output.clone());
                let (stop, counters) = (stop.clone(), counters.clone());
                move || capture_thread(&id, &output, &stop, &counters, &opened_tx)
            })
            .map_err(|e| format!("couldn't start the input's thread: {e}"))?;
        Ok(Pending {
            opened,
            counters,
            stop,
            thread,
        })
    }

    fn wait(self) -> Result<Capture, String> {
        let opened = match self.opened.recv() {
            Ok(Ok(opened)) => opened,
            Ok(Err(error)) => {
                let _ = self.thread.join();
                return Err(error);
            }
            Err(_) => {
                // Only a panic ends the thread without a word.
                let _ = self.thread.join();
                return Err("the input's thread failed".into());
            }
        };
        Ok(Capture {
            input: OpenedInput {
                name: opened.name,
                channels: opened.channels,
                rate: opened.rate,
                clock: opened.clock,
                buffer: opened.buffer,
                counters: self.counters,
                // Packets come a period at a time, so the newest frame falls
                // up to a period further behind between them: room for that,
                // and half as much again for jitter.
                room: opened.packet * 3 / 2,
            },
            stop: self.stop,
            thread: Some(self.thread),
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture_thread(
    id: &str,
    output: &Output,
    stop: &Event,
    counters: &Arc<InputCounters>,
    opened: &SyncSender<Result<Opened, String>>,
) {
    let _com = match Com::init() {
        Ok(com) => com,
        Err(error) => {
            let _ = opened.send(Err(message(&error)));
            return;
        }
    };
    // Ahead of everything but the output, so it keeps up with the device.
    let _mmcss = Mmcss::join();
    let mut input = match Input::open(id, output) {
        Ok(input) => input,
        Err(error) => {
            let _ = opened.send(Err(error));
            return;
        }
    };
    let format = input.format;
    let locked = input.same_clock && format.rate == output.rate;
    let (buffer, writer) = InputBuffer::new(
        usize::from(format.channels),
        input::buffer_frames(output.rate),
    );
    let mut conformer = Conformer::new(
        writer,
        format.rate,
        output.rate,
        input.same_clock,
        output.clock.clone(),
        counters.clone(),
    );
    let packet = u64::from(input.period) * u64::from(output.rate) / u64::from(format.rate);
    let report = Opened {
        name: input.name.clone(),
        channels: format.channels,
        rate: format.rate,
        clock: if locked {
            InputClock::Output
        } else {
            InputClock::Resampled
        },
        buffer,
        packet,
    };
    if opened.send(Ok(report)).is_err() {
        return;
    }
    if let Err(failed) = input.run(&mut conformer, stop, counters) {
        counters.fail(message(&failed.into()));
    }
    input.stop();
}

/// What to tell the user about an input that failed.
fn message(error: &Error) -> String {
    match error {
        Error::DeviceNotFound => "no such input device".into(),
        Error::DeviceLost => "the input device was unplugged, disabled or reconfigured".into(),
        error => error.to_string(),
    }
}

/// An input's capture client, started.
struct Input {
    audio: IAudioClient,
    capture: IAudioCaptureClient,
    /// Set by WASAPI when a packet is ready.
    event: Event,
    format: Format,
    name: String,
    /// Whether the input runs on the output's clock.
    same_clock: bool,
    /// Frames per packet: the audio engine's period.
    period: u32,
    /// Where packets are turned into floats: a buffer's worth.
    scratch: Vec<f32>,
    /// For a loopback of an output other than ours: silence played there,
    /// so the system keeps mixing for it.
    keep_alive: Option<KeepAlive>,
    /// The device position just after the last packet.
    next: Option<u64>,
}

impl Input {
    fn open(id: &str, output: &Output) -> Result<Input, String> {
        let (device, loopback) = device::enumerator()
            .and_then(|enumerator| device::find_input(&enumerator, id))
            .map_err(|e| message(&e))?;
        // A loopback of the output it plays with.
        let ours = loopback && device::id_of(&device).is_ok_and(|found| found == output.id);
        if ours && output.exclusive {
            return Err(
                "the output plays in exclusive mode, which bypasses the system's \
                        mix: a loopback of it records nothing"
                    .into(),
            );
        }
        let name = device::friendly_name(&device).unwrap_or_else(|| id.to_owned());
        // A device's inputs and outputs share its clock, and a loopback
        // runs on its output's.
        let container = device::container_id(&device).filter(|&found| found != BUILT_IN);
        let same_clock = ours || (container.is_some() && container == output.container);
        Input::start(&device, loopback, ours, name, same_clock).map_err(|e| message(&e))
    }

    fn start(
        device: &IMMDevice,
        loopback: bool,
        ours: bool,
        name: String,
        same_clock: bool,
    ) -> Result<Input, Error> {
        let audio: IAudioClient = client::activate(device)?;
        let mix = client::mix_format(&audio)?;
        // SAFETY: WASAPI's own format.
        let format = unsafe { Format::read(mix.0) }.ok_or(Error::UnsupportedFormat)?;
        let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        if loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        // SAFETY: a fresh client and WASAPI's own format. All WASAPI calls
        // here are on this thread, in the MTA.
        unsafe { audio.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, mix.0, None) }
            .map_err(|e| os("IAudioClient::Initialize", e))?;
        let event = Event::auto_reset()?;
        let (buffer, capture, period) = unsafe {
            audio
                .SetEventHandle(event.0)
                .map_err(|e| os("IAudioClient::SetEventHandle", e))?;
            let buffer = audio
                .GetBufferSize()
                .map_err(|e| os("IAudioClient::GetBufferSize", e))?;
            let capture: IAudioCaptureClient = audio
                .GetService()
                .map_err(|e| os("IAudioClient::GetService(IAudioCaptureClient)", e))?;
            let mut period = 0;
            audio
                .GetDevicePeriod(Some(&mut period), None)
                .map_err(|e| os("IAudioClient::GetDevicePeriod", e))?;
            (buffer, capture, period)
        };
        let period = hns_to_frames(period.max(0) as u64, format.rate).clamp(1, u64::from(buffer));
        let keep_alive = if loopback && !ours {
            Some(KeepAlive::start(device)?)
        } else {
            None
        };
        // SAFETY: an initialized client.
        unsafe { audio.Start() }.map_err(|e| os("IAudioClient::Start", e))?;
        Ok(Input {
            audio,
            capture,
            event,
            format,
            name,
            same_clock,
            period: period as u32,
            scratch: vec![0.0; buffer as usize * usize::from(format.channels)],
            keep_alive,
            next: None,
        })
    }

    /// Takes packets until the stop event is set or the device fails.
    fn run(
        &mut self,
        conformer: &mut Conformer,
        stop: &Event,
        counters: &InputCounters,
    ) -> Result<(), Failed> {
        let events = [stop.0, self.event.0];
        loop {
            // SAFETY: both events stay open for the whole loop.
            let woke = unsafe { WaitForMultipleObjects(&events, false, WAIT_MS) };
            if woke == WAIT_OBJECT_0 {
                return Ok(());
            }
            if woke.0 != WAIT_OBJECT_0.0 + 1 && woke != WAIT_TIMEOUT {
                return Err(Failed {
                    call: "WaitForMultipleObjects",
                    error: windows::core::Error::from_thread(),
                });
            }
            rt::scope(|| self.take_packets(conformer, counters))?;
        }
    }

    /// Takes every packet that's ready. Real-time safe.
    fn take_packets(
        &mut self,
        conformer: &mut Conformer,
        counters: &InputCounters,
    ) -> Result<(), Failed> {
        if let Some(keep_alive) = &self.keep_alive {
            keep_alive.top_up()?;
        }
        loop {
            // SAFETY: an initialized, started client.
            let ready = unsafe { self.capture.GetNextPacketSize() }
                .call("IAudioCaptureClient::GetNextPacketSize")?;
            if ready == 0 {
                return Ok(());
            }
            let (mut data, mut frames, mut flags) = (ptr::null_mut(), 0, 0);
            let (mut position, mut qpc) = (0, 0);
            // SAFETY: as above; the packet is released below.
            unsafe {
                self.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut position),
                    Some(&mut qpc),
                )
            }
            .call("IAudioCaptureClient::GetBuffer")?;
            if frames == 0 {
                return Ok(());
            }
            self.take(data, frames, flags, position, qpc, conformer, counters);
            // SAFETY: releases the packet just taken, whole.
            unsafe { self.capture.ReleaseBuffer(frames) }
                .call("IAudioCaptureClient::ReleaseBuffer")?;
        }
    }

    /// Passes one packet on, as floats.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &mut self,
        data: *mut u8,
        frames: u32,
        flags: u32,
        position: u64,
        qpc: u64,
        conformer: &mut Conformer,
        counters: &InputCounters,
    ) {
        let flag = |f: i32| flags & f as u32 != 0;
        if flag(AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0) && self.next.is_some() {
            counters.discontinuity();
        }
        // Positions never go back; a device that says they do is taken to
        // follow on from the packet before.
        let position = self.next.map_or(position, |next| position.max(next));
        self.next = Some(position + u64::from(frames));
        // QPC times come in 100 ns units.
        let time = (!flag(AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0)).then(|| qpc.saturating_mul(100));
        let format = self.format;
        let count = frames as usize * usize::from(format.channels);
        let samples = if flag(AUDCLNT_BUFFERFLAGS_SILENT.0) {
            None
        } else if format.sample == SampleFormat::F32 && data.cast::<f32>().is_aligned() {
            // SAFETY: WASAPI handed over `frames` frames of f32 samples,
            // aligned as checked, until `ReleaseBuffer`.
            Some(unsafe { slice::from_raw_parts(data.cast::<f32>().cast_const(), count) })
        } else {
            // SAFETY: as above, in the stream's sample format.
            let bytes =
                unsafe { slice::from_raw_parts(data.cast_const(), count * format.sample.bytes()) };
            let out = &mut self.scratch[..count];
            format.sample.read(bytes, out);
            Some(&*out)
        };
        conformer.push(Packet {
            position,
            time,
            frames: frames as usize,
            samples,
        });
    }

    fn stop(&self) {
        // SAFETY: an initialized client. Stopping it twice is harmless.
        let _ = unsafe { self.audio.Stop() };
    }
}

/// Silence played to an output, so the system keeps mixing for it, and a
/// loopback of it keeps capturing, whether anything else plays there or not.
struct KeepAlive {
    audio: IAudioClient,
    render: IAudioRenderClient,
    buffer: u32,
}

impl KeepAlive {
    fn start(device: &IMMDevice) -> Result<KeepAlive, Error> {
        let audio: IAudioClient = client::activate(device)?;
        let mix = client::mix_format(&audio)?;
        // Topped up whenever the capture thread looks for packets: a buffer
        // of many of those.
        // SAFETY: a fresh client and WASAPI's own format.
        unsafe { audio.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, BUFFER_HNS, 0, mix.0, None) }
            .map_err(|e| os("IAudioClient::Initialize", e))?;
        // SAFETY: an initialized client.
        let (buffer, render) = unsafe {
            let buffer = audio
                .GetBufferSize()
                .map_err(|e| os("IAudioClient::GetBufferSize", e))?;
            let render: IAudioRenderClient = audio
                .GetService()
                .map_err(|e| os("IAudioClient::GetService(IAudioRenderClient)", e))?;
            (buffer, render)
        };
        let keep_alive = KeepAlive {
            audio,
            render,
            buffer,
        };
        keep_alive.top_up()?;
        // SAFETY: an initialized client with its buffer written.
        unsafe { keep_alive.audio.Start() }.map_err(|e| os("IAudioClient::Start", e))?;
        Ok(keep_alive)
    }

    /// Fills the buffer with silence. Real-time safe.
    fn top_up(&self) -> Result<(), Failed> {
        // SAFETY: an initialized client; the buffer is released at once.
        unsafe {
            let padding = self
                .audio
                .GetCurrentPadding()
                .call("IAudioClient::GetCurrentPadding")?;
            let free = self.buffer.saturating_sub(padding);
            if free > 0 {
                self.render
                    .GetBuffer(free)
                    .call("IAudioRenderClient::GetBuffer")?;
                self.render
                    .ReleaseBuffer(free, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                    .call("IAudioRenderClient::ReleaseBuffer")?;
            }
        }
        Ok(())
    }
}

impl Drop for KeepAlive {
    fn drop(&mut self) {
        // SAFETY: an initialized client. Stopping it twice is harmless.
        let _ = unsafe { self.audio.Stop() };
    }
}
