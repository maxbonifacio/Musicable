//! Audio device output for the engine.
//!
//! On Windows this is our own WASAPI backend. Shared mode runs at the
//! smallest period the driver offers through IAudioClient3, or at the audio
//! engine's default period (usually 10 ms) when it offers nothing smaller.
//! Exclusive mode is an option. On macOS and Linux it goes through
//! [cpal](https://docs.rs/cpal): it builds and plays, but isn't tuned.
//!
//! Opening a stream takes two steps, because the engine has to be built for
//! what the device actually does, which isn't always what was asked for:
//!
//! 1. [`open`] opens the device and reports what it got in [`StreamInfo`]:
//!    the sample rate, the period the driver really delivers, the latency.
//! 2. Create an [`Engine`] with [`StreamInfo::engine_config`], swap in a
//!    graph, and [`OpenStream::start`] the stream with it.
//!
//! The stream counts dropouts ([`StreamStats::underruns`]) from the device's
//! own clock and reports each one to the engine with
//! [`Engine::record_xrun`]. When the device goes away the stream ends by
//! itself, and [`Stream::stop`] hands back the engine and says why.
//!
//! `docs/audio.md` describes how it works.

use std::fmt;
use std::time::Duration;

use musicable_engine::{Engine, EngineConfig};

#[cfg(any(windows, test))]
mod format;
#[cfg(any(windows, test))]
mod period;
mod stats;
#[cfg(any(windows, test))]
mod xrun;

#[cfg(windows)]
mod wasapi;
#[cfg(windows)]
use wasapi as backend;

#[cfg(not(windows))]
mod cpal_host;
#[cfg(not(windows))]
use cpal_host as backend;

pub use stats::StreamStats;

#[cfg(test)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

/// An audio output device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Identifies the device across runs: save it to open the same device
    /// next time.
    pub id: String,
    pub name: String,
    /// Whether it is the system's default output.
    pub is_default: bool,
}

/// The output devices that are plugged in and enabled.
pub fn output_devices() -> Result<Vec<Device>, Error> {
    backend::output_devices()
}

/// What a device supports, for offering choices before opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The rate the device runs at now. Shared-mode streams run at this rate.
    pub sample_rate: u32,
    pub channels: u16,
    /// The smallest period a shared-mode stream can have, in frames. `None`
    /// when the system only decides once the stream opens.
    pub min_period: Option<u32>,
    /// The period a shared-mode stream gets by default, in frames.
    pub default_period: Option<u32>,
    /// Exclusive mode, if the device supports it.
    pub exclusive: Option<ExclusiveCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusiveCapabilities {
    /// The common rates the device takes a format at that we can write.
    pub sample_rates: Vec<u32>,
    /// The smallest period the driver reports, in frames at
    /// [`Capabilities::sample_rate`]. Some drivers report less than they
    /// deliver; a stream says what it really got.
    pub min_period: u32,
}

/// What `device` supports, or the default output's if it's `None`.
pub fn capabilities(device: Option<&str>) -> Result<Capabilities, Error> {
    backend::capabilities(device)
}

/// How a stream shares the device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Mixed with other apps' audio by the system.
    #[default]
    Shared,
    /// The device to ourselves, with the lowest latency. Other apps go
    /// silent. WASAPI only.
    Exclusive,
}

/// The stream to open. The defaults open the default output in shared mode
/// at the smallest period it supports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamRequest {
    /// A [`Device::id`], or `None` for the default output.
    pub device: Option<String>,
    pub mode: Mode,
    /// `None` for the rate the device runs at. A shared-mode stream can only
    /// run at that rate.
    pub sample_rate: Option<u32>,
    /// Frames per period, the "buffer size", or `None` for the smallest the
    /// device supports. Drivers round it, and some ignore it:
    /// [`StreamInfo::period`] says what the stream got.
    pub period: Option<u32>,
}

/// What drives a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Api {
    /// WASAPI shared mode at the audio engine's default period, for drivers
    /// that offer nothing smaller.
    WasapiShared,
    /// WASAPI shared mode at a smaller period, set through IAudioClient3.
    WasapiLowLatency,
    WasapiExclusive,
    /// cpal's default host: CoreAudio on macOS, ALSA on Linux.
    Cpal,
}

impl fmt::Display for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WasapiShared => "WASAPI shared",
            Self::WasapiLowLatency => "WASAPI shared, low latency",
            Self::WasapiExclusive => "WASAPI exclusive",
            Self::Cpal => "cpal",
        })
    }
}

/// How the device stores samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    F32,
    /// 32-bit integers. Some devices only use the top 24 bits.
    I32,
    /// Packed 24-bit integers, 3 bytes each.
    I24,
    I16,
}

/// An open stream, as the device set it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub device: Device,
    pub api: Api,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    /// Frames the device asks for at each wake-up: the period it really
    /// delivers, which can differ from the one requested.
    pub period: u32,
    /// The device buffer, in frames. In shared mode the most the stream can
    /// queue; in exclusive mode each of the two halves the device alternates
    /// between.
    pub buffer: u32,
    /// Output latency in frames, estimated from the buffer sizes: how far
    /// ahead of the device's play position a punctual wake-up starts
    /// writing, plus the delay the system reports beyond that. It leaves out
    /// converter delay. [`StreamStats::queued`] measures it while the stream
    /// runs, as the device's clock sees it.
    pub latency: u32,
    /// Whether the audio thread runs at real-time priority: MMCSS "Pro
    /// Audio" on Windows.
    pub realtime: bool,
}

impl StreamInfo {
    /// What to create the engine with: the stream's sample rate, and its
    /// period as the block size.
    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            sample_rate: self.sample_rate,
            max_block: self.period as usize,
        }
    }

    pub fn period_duration(&self) -> Duration {
        frames_to_duration(u64::from(self.period), self.sample_rate)
    }

    pub fn latency_duration(&self) -> Duration {
        frames_to_duration(u64::from(self.latency), self.sample_rate)
    }
}

pub(crate) fn frames_to_duration(frames: u64, sample_rate: u32) -> Duration {
    let nanos = u128::from(frames) * 1_000_000_000 / u128::from(sample_rate.max(1));
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

/// Opens a device for output. It doesn't play until it's started with an
/// engine.
pub fn open(request: &StreamRequest) -> Result<OpenStream, Error> {
    let (info, inner) = backend::open(request)?;
    Ok(OpenStream { info, inner })
}

/// A device opened for output, not playing yet. Dropping it closes the
/// device.
pub struct OpenStream {
    info: StreamInfo,
    inner: backend::OpenStream,
}

impl OpenStream {
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Starts playing what `engine` renders. The engine must run at the
    /// stream's sample rate; [`StreamInfo::engine_config`] has the block size
    /// to create it with.
    pub fn start(self, engine: Engine) -> Result<Stream, StartError> {
        let rate = engine.config().sample_rate;
        if rate != self.info.sample_rate {
            return Err(StartError {
                engine: Box::new(engine),
                error: Error::EngineRate {
                    engine: rate,
                    stream: self.info.sample_rate,
                },
            });
        }
        let inner = self.inner.start(engine)?;
        Ok(Stream {
            info: self.info,
            inner,
        })
    }
}

/// Why a stream didn't start, with the engine it was given.
pub struct StartError {
    pub engine: Box<Engine>,
    pub error: Error,
}

impl fmt::Debug for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for StartError {}

/// A playing stream. [`stop`](Stream::stop) it to get the engine back.
/// Dropping it stops it too, and drops the engine on the dropping thread.
pub struct Stream {
    info: StreamInfo,
    inner: backend::Stream,
}

impl Stream {
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    pub fn stats(&self) -> StreamStats {
        self.inner.stats()
    }

    /// Whether the stream ended by itself, because the device went away or
    /// failed. [`stop`](Stream::stop) says why.
    pub fn has_ended(&self) -> bool {
        self.inner.has_ended()
    }

    /// Stops the stream and hands back the engine, off the audio thread.
    pub fn stop(self) -> Stopped {
        self.inner.stop()
    }
}

/// A stopped stream's engine, and the error that ended the stream if it
/// ended by itself.
pub struct Stopped {
    pub engine: Engine,
    pub error: Option<Error>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// No device has the requested id, or there is no output device.
    DeviceNotFound,
    /// The device was unplugged or disabled, its format was changed in the
    /// system settings, or the audio service restarted. Open it again, or
    /// open another.
    DeviceLost,
    /// Another app has the device in exclusive mode.
    DeviceBusy,
    /// Exclusive mode is turned off for the device in the Windows sound
    /// settings, or the backend has no exclusive mode.
    ExclusiveNotAllowed,
    /// The device doesn't run at `requested` in this mode. It runs at
    /// `device` now.
    SampleRate { requested: u32, device: u32 },
    /// The device takes no sample format we can write.
    UnsupportedFormat,
    /// The driver delivers a different period from the one it set up, even
    /// after reopening at the period it delivers.
    PeriodMismatch { period: u32, delivered: u32 },
    /// The engine runs at a different sample rate from the stream.
    EngineRate { engine: u32, stream: u32 },
    /// The device stopped asking for audio, without reporting an error.
    Timeout,
    /// Anything else the system reported: the call that failed, and why.
    Os { call: &'static str, message: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceNotFound => f.write_str("no such output device"),
            Self::DeviceLost => f.write_str("the device was unplugged, disabled or reconfigured"),
            Self::DeviceBusy => f.write_str("another app is using the device in exclusive mode"),
            Self::ExclusiveNotAllowed => {
                f.write_str("exclusive mode isn't allowed for this device")
            }
            Self::SampleRate { requested, device } => write!(
                f,
                "the device doesn't run at {requested} Hz in this mode (it runs at {device} Hz)"
            ),
            Self::UnsupportedFormat => {
                f.write_str("the device takes no sample format we can write")
            }
            Self::PeriodMismatch { period, delivered } => write!(
                f,
                "the driver set up a {period}-frame period but delivers {delivered} frames"
            ),
            Self::EngineRate { engine, stream } => write!(
                f,
                "the engine runs at {engine} Hz but the stream at {stream} Hz"
            ),
            Self::Timeout => f.write_str("the device stopped asking for audio"),
            Self::Os { call, message } => write!(f, "{call} failed: {message}"),
        }
    }
}

impl std::error::Error for Error {}
