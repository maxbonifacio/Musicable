//! macOS and Linux, through cpal's default host: CoreAudio and ALSA. It
//! builds and plays, but isn't tuned.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, ErrorKind, FromSample, SizedSample, SupportedBufferSize};
use musicable_engine::Engine;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::input::{self, InputDevice, StreamInput};
use crate::stats::Counters;
use crate::{
    Api, Capabilities, Device, DriverType, Error, Feeds, Mode, SampleFormat, StartError, Stopped,
    StreamInfo, StreamRequest, StreamStats, frames_to_duration,
};

/// The block size when the system picks the period and won't say what it
/// is. Callbacks of any length still work: the engine splits longer ones.
const FALLBACK_PERIOD: u32 = 512;

pub(crate) fn output_devices() -> Result<Vec<Device>, Error> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.id().ok());
    let devices = host.output_devices().map_err(error)?;
    // Skip devices that went away while listing.
    Ok(devices
        .filter_map(|device| describe(&device, default.as_ref()).ok())
        .collect())
}

pub(crate) fn input_devices() -> Result<Vec<InputDevice>, Error> {
    Ok(Vec::new())
}

pub(crate) fn capabilities(id: Option<&str>) -> Result<Capabilities, Error> {
    let device = find(id)?;
    let config = device.default_output_config().map_err(error)?;
    let min_period = match config.buffer_size() {
        SupportedBufferSize::Range { min, .. } => Some(*min),
        SupportedBufferSize::Unknown => None,
    };
    Ok(Capabilities {
        sample_rate: config.sample_rate(),
        channels: config.channels(),
        min_period,
        default_period: None,
        exclusive: None,
        asio: None,
    })
}

fn describe(device: &cpal::Device, default: Option<&cpal::DeviceId>) -> Result<Device, Error> {
    let id = device.id().map_err(error)?;
    let name = match device.description() {
        Ok(description) => description.name().to_owned(),
        Err(_) => device.to_string(),
    };
    Ok(Device {
        is_default: default == Some(&id),
        id: id.to_string(),
        name,
        driver_type: DriverType::System,
    })
}

fn find(id: Option<&str>) -> Result<cpal::Device, Error> {
    let host = cpal::default_host();
    match id {
        Some(id) => {
            let id: cpal::DeviceId = id.parse().map_err(|_| Error::DeviceNotFound)?;
            host.device_by_id(&id).ok_or(Error::DeviceNotFound)
        }
        None => host.default_output_device().ok_or(Error::DeviceNotFound),
    }
}

/// The audio thread's state, inside cpal's data callback.
struct Callback {
    /// Where the engine arrives when the stream starts.
    incoming: Consumer<Engine>,
    engine: Option<Engine>,
    /// Where the engine goes back when the callback is dropped, which is
    /// when the stream is.
    back: SyncSender<Engine>,
    shared: Arc<Shared>,
    channels: usize,
    rate: u32,
    /// The engine renders here, then the samples are converted.
    scratch: Vec<f32>,
    last: Option<Instant>,
}

impl Callback {
    fn render<T: SizedSample + FromSample<f32>>(
        &mut self,
        data: &mut [T],
        info: &cpal::OutputCallbackInfo,
    ) {
        let woke = Instant::now();
        if self.engine.is_none() {
            self.engine = self.incoming.pop().ok();
        }
        let frames = data.len() / self.channels;
        let counters = &self.shared.counters;
        if let Some(last) = self.last.replace(woke) {
            let interval = woke - last;
            counters.interval(interval);
            if interval * 2 > frames_to_duration(frames as u64, self.rate) * 3 {
                counters.late_wakeup();
            }
        }
        match &mut self.engine {
            Some(engine) => {
                for _ in 0..self.shared.xruns.swap(0, Relaxed) {
                    engine.record_xrun();
                }
                for chunk in data.chunks_mut(self.scratch.len()) {
                    let scratch = &mut self.scratch[..chunk.len()];
                    engine.process_interleaved(scratch, self.channels);
                    for (out, sample) in chunk.iter_mut().zip(scratch.iter()) {
                        *out = T::from_sample(*sample);
                    }
                }
            }
            None => data.iter_mut().for_each(|out| *out = T::from_sample(0.0)),
        }
        let timestamp = info.timestamp();
        if let Some(ahead) = timestamp
            .playback
            .checked_duration_since(timestamp.callback)
        {
            let frames_ahead = ahead.as_nanos() * u128::from(self.rate) / 1_000_000_000;
            counters.queued(u64::try_from(frames_ahead).unwrap_or(u64::MAX));
        }
        counters.wrote(u32::try_from(frames).unwrap_or(u32::MAX));
        counters.wakeup();
        counters.busy(woke.elapsed());
    }
}

impl Drop for Callback {
    fn drop(&mut self) {
        // An engine that never got taken is still in the queue.
        if let Some(engine) = self.engine.take().or_else(|| self.incoming.pop().ok()) {
            let _ = self.back.send(engine);
        }
    }
}

#[derive(Default)]
struct Shared {
    counters: Counters,
    /// Xruns reported by cpal and not yet passed on to the engine.
    xruns: AtomicU64,
    failure: AtomicU8,
}

const LOST: u8 = 1;
const FAILED: u8 = 2;

impl Shared {
    fn report(&self, error: &cpal::Error) {
        match error.kind() {
            ErrorKind::Xrun => {
                self.xruns.fetch_add(1, Relaxed);
                self.counters.underrun();
            }
            // Rerouted, or merely not at real-time priority: still playing.
            ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => {}
            ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated => {
                self.failure.store(LOST, Relaxed);
                self.counters.end();
            }
            _ => {
                self.failure.store(FAILED, Relaxed);
                self.counters.end();
            }
        }
    }

    fn error(&self) -> Option<Error> {
        match self.failure.load(Relaxed) {
            LOST => Some(Error::DeviceLost),
            FAILED => Some(Error::Os {
                call: "the cpal stream",
                message: "the stream failed".into(),
            }),
            _ => None,
        }
    }
}

/// Opens the device. Its inputs and MIDI inputs don't open, so it has no
/// use for the `_feeds` that would line them up.
pub(crate) fn open(
    request: &StreamRequest,
    _feeds: Feeds,
) -> Result<(StreamInfo, Vec<StreamInput>, OpenStream), Error> {
    if request.mode == Mode::Exclusive {
        return Err(Error::ExclusiveNotAllowed);
    }
    let device = find(request.device.as_deref())?;
    let host_default = cpal::default_host()
        .default_output_device()
        .and_then(|d| d.id().ok());
    let described = describe(&device, host_default.as_ref())?;
    let (config, format) = pick_config(&device, request)?;

    let (engines, incoming) = RingBuffer::new(1);
    let (back, returned) = mpsc::sync_channel(1);
    let shared = Arc::new(Shared::default());
    let channels = usize::from(config.channels);
    let longest = match config.buffer_size {
        BufferSize::Fixed(frames) => frames.max(FALLBACK_PERIOD),
        BufferSize::Default => 4 * FALLBACK_PERIOD,
    };
    let callback = Callback {
        incoming,
        engine: None,
        back,
        shared: shared.clone(),
        channels,
        rate: config.sample_rate,
        scratch: vec![0.0; longest as usize * channels],
        last: None,
    };
    let (stream, sample_format) = match format {
        cpal::SampleFormat::F32 => (
            build::<f32>(&device, &config, callback, &shared)?,
            SampleFormat::F32,
        ),
        cpal::SampleFormat::I32 => (
            build::<i32>(&device, &config, callback, &shared)?,
            SampleFormat::I32,
        ),
        _ => (
            build::<i16>(&device, &config, callback, &shared)?,
            SampleFormat::I16,
        ),
    };
    let period = stream
        .buffer_size()
        .ok()
        .filter(|&n| n > 0)
        .unwrap_or(match config.buffer_size {
            BufferSize::Fixed(frames) => frames,
            BufferSize::Default => FALLBACK_PERIOD,
        });
    let info = StreamInfo {
        device: described,
        api: Api::Cpal,
        sample_rate: config.sample_rate,
        channels: config.channels,
        format: sample_format,
        period,
        buffer: period,
        latency: period,
        // CoreAudio runs its I/O threads at real-time priority; cpal's ALSA
        // thread doesn't ask for it.
        realtime: cfg!(target_os = "macos"),
        inputs: Vec::new(),
        midi_inputs: Vec::new(),
    };
    let opened = request
        .inputs
        .iter()
        .map(|_| Err("only Windows records so far".to_owned()))
        .collect();
    let (infos, inputs, _) = input::assemble(&request.inputs, opened);
    let info = StreamInfo {
        inputs: infos,
        ..info
    };
    Ok((
        info,
        inputs,
        OpenStream {
            stream,
            engines,
            returned,
            shared,
        },
    ))
}

fn pick_config(
    device: &cpal::Device,
    request: &StreamRequest,
) -> Result<(cpal::StreamConfig, cpal::SampleFormat), Error> {
    let default = device.default_output_config().map_err(error)?;
    let rate = request.sample_rate.unwrap_or(default.sample_rate());
    let rank = |format: cpal::SampleFormat| match format {
        cpal::SampleFormat::F32 => Some(0),
        cpal::SampleFormat::I32 => Some(1),
        cpal::SampleFormat::I16 => Some(2),
        _ => None,
    };
    let mut ranges: Vec<_> = device
        .supported_output_configs()
        .map_err(error)?
        .filter(|range| range.contains_rate(rate) && rank(range.sample_format()).is_some())
        .collect();
    // The default channel count first, then the best sample format.
    ranges.sort_by_key(|range| {
        (
            range.channels() != default.channels(),
            rank(range.sample_format()),
        )
    });
    let Some(range) = ranges.first() else {
        return Err(Error::SampleRate {
            requested: rate,
            device: default.sample_rate(),
        });
    };
    let buffer_size = match (request.period, range.buffer_size()) {
        (None, _) => BufferSize::Default,
        (Some(frames), SupportedBufferSize::Range { min, max }) => {
            BufferSize::Fixed(frames.clamp(*min, *max))
        }
        (Some(frames), SupportedBufferSize::Unknown) => BufferSize::Fixed(frames),
    };
    let config = cpal::StreamConfig {
        channels: range.channels(),
        sample_rate: rate,
        buffer_size,
    };
    Ok((config, range.sample_format()))
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut callback: Callback,
    shared: &Arc<Shared>,
) -> Result<cpal::Stream, Error> {
    let shared = shared.clone();
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data, info| callback.render(data, info),
            move |error| shared.report(&error),
            None,
        )
        .map_err(error)
}

/// An error opening or starting a stream. Errors while it runs go through
/// [`Shared::report`].
fn error(error: cpal::Error) -> Error {
    match error.kind() {
        ErrorKind::DeviceNotAvailable => Error::DeviceNotFound,
        ErrorKind::DeviceBusy => Error::DeviceBusy,
        ErrorKind::UnsupportedConfig => Error::UnsupportedFormat,
        _ => Error::Os {
            call: "cpal",
            message: error.to_string(),
        },
    }
}

pub(crate) struct OpenStream {
    stream: cpal::Stream,
    engines: Producer<Engine>,
    returned: Receiver<Engine>,
    shared: Arc<Shared>,
}

impl OpenStream {
    pub fn start(mut self, engine: Engine) -> Result<Stream, StartError> {
        if let Err(rtrb::PushError::Full(engine)) = self.engines.push(engine) {
            // Only one engine is ever sent.
            return Err(StartError {
                engine: Box::new(engine),
                error: Error::Os {
                    call: "starting the stream",
                    message: "the stream already has an engine".into(),
                },
            });
        }
        match self.stream.play() {
            Ok(()) => Ok(Stream {
                stream: Some(self.stream),
                returned: self.returned,
                shared: self.shared,
            }),
            Err(e) => {
                // Dropping the stream drops the callback, which sends the
                // engine back.
                drop(self.stream);
                let engine = self
                    .returned
                    .recv()
                    .expect("the callback returns the engine");
                Err(StartError {
                    engine: Box::new(engine),
                    error: error(e),
                })
            }
        }
    }
}

pub(crate) struct Stream {
    stream: Option<cpal::Stream>,
    returned: Receiver<Engine>,
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
        drop(self.stream.take());
        let engine = self
            .returned
            .recv()
            .expect("the callback returns the engine");
        Stopped {
            engine,
            error: self.shared.error(),
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // Drops the engine here, not on the audio thread.
        drop(self.stream.take());
        let _ = self.returned.try_recv();
    }
}
