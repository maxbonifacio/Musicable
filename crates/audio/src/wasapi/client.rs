//! Setting up a WASAPI render client: format, period and mode.

use std::mem::size_of;
use std::ptr;

use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_BUFFER_TOO_LARGE,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_ENGINE_PERIODICITY_LOCKED,
    AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioClient3, IAudioClock,
    IAudioRenderClient, IMMDevice, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
    WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::{
    KSDATAFORMAT_SUBTYPE_PCM, SPEAKER_FRONT_CENTER, SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT,
    WAVE_FORMAT_EXTENSIBLE,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::{HRESULT, Interface};

use super::com::{Event, Qpc, TaskMem, os};
use crate::period::{self, EnginePeriods, frames_to_hns, hns_to_frames};
use crate::{
    Api, Capabilities, Device, Error, ExclusiveCapabilities, Mode, SampleFormat, StreamInfo,
    StreamRequest,
};

/// The rates [`Capabilities`] checks exclusive mode at.
const COMMON_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Exclusive-mode formats, best first: what most interfaces take natively,
/// then the rest.
const EXCLUSIVE_FORMATS: [(SampleFormat, u16); 5] = [
    (SampleFormat::I32, 24),
    (SampleFormat::F32, 32),
    (SampleFormat::I32, 32),
    (SampleFormat::I24, 24),
    (SampleFormat::I16, 16),
];

/// Wake-ups timed when checking an exclusive-mode period.
const CHECKED_WAKEUPS: usize = 24;

/// A stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Format {
    pub rate: u32,
    pub channels: u16,
    pub sample: SampleFormat,
    pub valid_bits: u16,
    /// Which speaker each channel is for.
    pub mask: u32,
}

impl Format {
    /// Reads a format WASAPI handed over. `None` if we can't write it.
    ///
    /// # Safety
    ///
    /// `wave` points to a valid `WAVEFORMATEX`, followed by the rest of a
    /// `WAVEFORMATEXTENSIBLE` if its tag says so.
    pub unsafe fn read(wave: *const WAVEFORMATEX) -> Option<Format> {
        // SAFETY: as the caller promises. The structs are packed.
        let base = unsafe { wave.read_unaligned() };
        let (tag, channels, rate, bits) = (
            u32::from(base.wFormatTag),
            base.nChannels,
            base.nSamplesPerSec,
            base.wBitsPerSample,
        );
        let extensible = tag == WAVE_FORMAT_EXTENSIBLE
            && usize::from(base.cbSize)
                >= size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();
        let (float, valid_bits, mask) = if extensible {
            // SAFETY: the tag and size say the rest is there.
            let ext = unsafe { wave.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            let samples = ext.Samples;
            // SAFETY: every member of the union is a u16.
            let valid_bits = unsafe { samples.wValidBitsPerSample };
            let sub = ext.SubFormat;
            let float = if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                true
            } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
                false
            } else {
                return None;
            };
            (float, valid_bits, ext.dwChannelMask)
        } else if tag == WAVE_FORMAT_IEEE_FLOAT {
            (true, bits, speakers(channels))
        } else if tag == WAVE_FORMAT_PCM {
            (false, bits, speakers(channels))
        } else {
            return None;
        };
        let sample = match (float, bits) {
            (true, 32) => SampleFormat::F32,
            (false, 32) => SampleFormat::I32,
            (false, 24) => SampleFormat::I24,
            (false, 16) => SampleFormat::I16,
            _ => return None,
        };
        Some(Format {
            rate,
            channels,
            sample,
            valid_bits: if valid_bits == 0 { bits } else { valid_bits },
            mask,
        })
    }

    fn wave(&self) -> WAVEFORMATEXTENSIBLE {
        let bytes = self.sample.bytes() as u16;
        let block = self.channels * bytes;
        WAVEFORMATEXTENSIBLE {
            Format: WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
                nChannels: self.channels,
                nSamplesPerSec: self.rate,
                nAvgBytesPerSec: self.rate * u32::from(block),
                nBlockAlign: block,
                wBitsPerSample: bytes * 8,
                cbSize: (size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()) as u16,
            },
            Samples: WAVEFORMATEXTENSIBLE_0 {
                wValidBitsPerSample: self.valid_bits,
            },
            dwChannelMask: self.mask,
            SubFormat: if self.sample == SampleFormat::F32 {
                KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
            } else {
                KSDATAFORMAT_SUBTYPE_PCM
            },
        }
    }
}

/// The usual speakers for a channel count, where there is one.
fn speakers(channels: u16) -> u32 {
    match channels {
        1 => SPEAKER_FRONT_CENTER,
        2 => SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        _ => 0,
    }
}

/// The rate, channels and speakers of a format, written by us or not.
///
/// # Safety
///
/// As for [`Format::read`].
unsafe fn basics(wave: *const WAVEFORMATEX) -> (u32, u16, u32) {
    // SAFETY: as the caller promises.
    let base = unsafe { wave.read_unaligned() };
    let (tag, rate, channels) = (
        u32::from(base.wFormatTag),
        base.nSamplesPerSec,
        base.nChannels,
    );
    let mask = if tag == WAVE_FORMAT_EXTENSIBLE {
        // SAFETY: as the caller promises.
        unsafe { wave.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() }.dwChannelMask
    } else {
        speakers(channels)
    };
    (rate, channels, mask)
}

/// An initialized render client, not started.
pub(super) struct Client {
    pub audio: IAudioClient,
    pub render: IAudioRenderClient,
    pub clock: IAudioClock,
    /// Set by WASAPI when the device wants audio.
    pub event: Event,
    pub api: Api,
    pub format: Format,
    /// Frames per wake-up: the engine period in shared mode, the buffer in
    /// exclusive mode.
    pub period: u32,
    /// `GetBufferSize`: the whole buffer in shared mode, each of the two
    /// halves in exclusive mode.
    pub buffer: u32,
    /// Frames to keep queued in shared mode; the buffer in exclusive mode.
    pub target: u32,
    /// `IAudioClock::GetFrequency`: what its positions count per second.
    pub clock_rate: u64,
    /// `GetStreamLatency`, in frames.
    pub stream_latency: u32,
}

impl Client {
    pub fn exclusive(&self) -> bool {
        self.api == Api::WasapiExclusive
    }

    pub fn info(&self, device: Device, realtime: bool) -> StreamInfo {
        let latency = if self.exclusive() {
            // The other half plays while this one is written.
            self.buffer
        } else {
            // A punctual wake-up finds a period gone from the target.
            self.target.saturating_sub(self.period) + self.stream_latency
        };
        StreamInfo {
            device,
            api: self.api,
            sample_rate: self.format.rate,
            channels: self.format.channels,
            format: self.format.sample,
            period: self.period,
            buffer: self.buffer,
            latency,
            realtime,
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
        }
    }

    /// Plays a buffer's worth of silence. For checking the period, before
    /// the engine plays.
    fn write_silence(&self) -> windows::core::Result<()> {
        // SAFETY: an initialized client; the buffer is released at once.
        unsafe {
            self.render.GetBuffer(self.buffer)?;
            self.render
                .ReleaseBuffer(self.buffer, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
        }
    }

    /// Plays silence for a moment and times the wake-ups. Returns the period
    /// the driver really runs at, if it's well above the buffer it set up.
    /// Leaves the client stopped and reset.
    fn delivered_period(&self) -> Result<Option<u32>, Error> {
        let delivered = self.time_wakeups();
        // SAFETY: an initialized client. Stopping a stopped one is harmless.
        unsafe {
            let _ = self.audio.Stop();
            let _ = self.audio.Reset();
            // A wake-up may be left signalled.
            WaitForSingleObject(self.event.0, 0);
        }
        delivered
    }

    fn time_wakeups(&self) -> Result<Option<u32>, Error> {
        let qpc = Qpc::new();
        let mut intervals = [0u64; CHECKED_WAKEUPS];
        self.write_silence()
            .map_err(|e| os("IAudioRenderClient::GetBuffer", e))?;
        // SAFETY: an initialized client with a buffer written.
        unsafe { self.audio.Start() }.map_err(|e| os("IAudioClient::Start", e))?;
        let mut last = None;
        let mut timed = 0;
        while timed < CHECKED_WAKEUPS {
            // SAFETY: the event lives as long as the client.
            if unsafe { WaitForSingleObject(self.event.0, 1000) } != WAIT_OBJECT_0 {
                return Err(Error::Timeout);
            }
            let now = qpc.now();
            match self.write_silence() {
                Ok(()) => {}
                // Woken with no half free: nothing to time.
                Err(e) if e.code() == AUDCLNT_E_BUFFER_TOO_LARGE => continue,
                Err(e) => return Err(os("IAudioRenderClient::GetBuffer", e)),
            }
            if let Some(last) = last {
                intervals[timed] = now - last;
                timed += 1;
            }
            last = Some(now);
        }
        Ok(period::delivered_period(
            &mut intervals,
            self.buffer,
            self.format.rate,
        ))
    }
}

/// Opens a render client on `device` for `request`.
pub(super) fn open(device: &IMMDevice, request: &StreamRequest) -> Result<Client, Error> {
    match request.mode {
        Mode::Shared => open_shared(device, request),
        Mode::Exclusive => open_exclusive(device, request),
    }
}

pub(super) fn activate<T: Interface>(device: &IMMDevice) -> Result<T, Error> {
    // SAFETY: on an MTA thread, like every WASAPI call in this backend.
    unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| os("IMMDevice::Activate", e))
}

pub(super) fn mix_format(audio: &IAudioClient) -> Result<TaskMem<WAVEFORMATEX>, Error> {
    // SAFETY: an activated client. The format is ours to free.
    unsafe { audio.GetMixFormat() }
        .map(TaskMem)
        .map_err(|e| os("IAudioClient::GetMixFormat", e))
}

fn engine_periods(audio: &IAudioClient3, format: *const WAVEFORMATEX) -> Option<EnginePeriods> {
    let (mut default, mut fundamental, mut min, mut max) = (0, 0, 0, 0);
    // SAFETY: an activated client and a valid format.
    unsafe {
        audio.GetSharedModeEnginePeriod(format, &mut default, &mut fundamental, &mut min, &mut max)
    }
    .ok()?;
    Some(EnginePeriods {
        default,
        fundamental,
        min,
        max,
    })
}

fn open_shared(device: &IMMDevice, request: &StreamRequest) -> Result<Client, Error> {
    let audio: IAudioClient = activate(device)?;
    let mix = mix_format(&audio)?;
    // SAFETY: WASAPI's own format.
    let format = unsafe { Format::read(mix.0) }.ok_or(Error::UnsupportedFormat)?;
    if let Some(requested) = request.sample_rate.filter(|&rate| rate != format.rate) {
        return Err(Error::SampleRate {
            requested,
            device: format.rate,
        });
    }

    // IAudioClient3 runs the audio engine at a smaller period, where the
    // driver supports one. Each attempt needs a fresh client.
    let periods = audio
        .cast::<IAudioClient3>()
        .ok()
        .and_then(|audio| engine_periods(&audio, mix.0));
    drop(audio);
    if let Some(periods) = periods.filter(EnginePeriods::low_latency) {
        let mut period = periods.pick(request.period);
        for _ in 0..2 {
            let audio: IAudioClient3 = activate(device)?;
            // SAFETY: a fresh client and WASAPI's own format.
            let init = unsafe {
                audio.InitializeSharedAudioStream(
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    period,
                    mix.0,
                    None,
                )
            };
            match init {
                Ok(()) => {
                    let api = if period < periods.default {
                        Api::WasapiLowLatency
                    } else {
                        Api::WasapiShared
                    };
                    let audio = audio
                        .cast()
                        .map_err(|e| os("IAudioClient3::QueryInterface", e))?;
                    return finish(audio, api, format, period);
                }
                // Another app has the engine at a period already: join in
                // at that one.
                Err(e) if e.code() == AUDCLNT_E_ENGINE_PERIODICITY_LOCKED => {
                    let mut current_format = ptr::null_mut();
                    // SAFETY: an activated client; the format is ours to free.
                    unsafe {
                        audio.GetCurrentSharedModeEnginePeriod(&mut current_format, &mut period)
                    }
                    .map_err(|e| os("IAudioClient3::GetCurrentSharedModeEnginePeriod", e))?;
                    drop(TaskMem(current_format));
                }
                Err(e) if super::com::device_gone(e.code()) => return Err(Error::DeviceLost),
                // Fall back to the default period.
                Err(_) => break,
            }
        }
    }

    let audio: IAudioClient = activate(device)?;
    // A buffer duration of zero gets the smallest buffer the engine allows.
    // SAFETY: a fresh client and WASAPI's own format.
    unsafe {
        audio.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            0,
            0,
            mix.0,
            None,
        )
    }
    .map_err(|e| os("IAudioClient::Initialize", e))?;
    let mut default = 0;
    // SAFETY: an initialized client.
    unsafe { audio.GetDevicePeriod(Some(&mut default), None) }
        .map_err(|e| os("IAudioClient::GetDevicePeriod", e))?;
    let period = hns_to_frames(default.max(0) as u64, format.rate) as u32;
    finish(audio, Api::WasapiShared, format, period)
}

fn open_exclusive(device: &IMMDevice, request: &StreamRequest) -> Result<Client, Error> {
    let probe: IAudioClient = activate(device)?;
    let mix = mix_format(&probe)?;
    // SAFETY: WASAPI's own format.
    let (mix_rate, channels, mask) = unsafe { basics(mix.0) };
    let rate = request.sample_rate.unwrap_or(mix_rate);
    let format = exclusive_format(&probe, rate, channels, mask).map_err(|error| match error {
        Error::UnsupportedFormat if rate != mix_rate => Error::SampleRate {
            requested: rate,
            device: mix_rate,
        },
        error => error,
    })?;
    let mut min = 0;
    // SAFETY: an activated client.
    unsafe { probe.GetDevicePeriod(None, Some(&mut min)) }
        .map_err(|e| os("IAudioClient::GetDevicePeriod", e))?;
    drop(probe);

    let period = period::exclusive_period(request.period, rate, min.max(0) as u64);
    let client = init_exclusive(device, format, period)?;
    // Some drivers keep to their own buffer setting whatever they set up.
    // Writing less than they play breaks the audio up, so check, and reopen
    // at the period they deliver.
    let Some(delivered) = client.delivered_period()? else {
        return Ok(client);
    };
    drop(client);
    let client = init_exclusive(device, format, frames_to_hns(u64::from(delivered), rate))?;
    match client.delivered_period()? {
        None => Ok(client),
        Some(delivered) => Err(Error::PeriodMismatch {
            period: client.buffer,
            delivered,
        }),
    }
}

/// The first exclusive-mode format the device takes at `rate`, trying the
/// device's own channel count, then stereo.
fn exclusive_format(
    audio: &IAudioClient,
    rate: u32,
    channels: u16,
    mask: u32,
) -> Result<Format, Error> {
    let mut layouts = vec![(channels, mask)];
    if channels != 2 {
        layouts.push((2, speakers(2)));
    }
    for (channels, mask) in layouts {
        for (sample, valid_bits) in EXCLUSIVE_FORMATS {
            let format = Format {
                rate,
                channels,
                sample,
                valid_bits,
                mask,
            };
            let wave = format.wave();
            // SAFETY: an activated client and a complete format.
            let result: HRESULT = unsafe {
                audio.IsFormatSupported(
                    AUDCLNT_SHAREMODE_EXCLUSIVE,
                    ptr::from_ref(&wave).cast(),
                    None,
                )
            };
            if result.0 == 0 {
                return Ok(format);
            }
            if result == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
                return Err(Error::ExclusiveNotAllowed);
            }
            if result == AUDCLNT_E_DEVICE_INVALIDATED {
                return Err(Error::DeviceLost);
            }
        }
    }
    Err(Error::UnsupportedFormat)
}

/// An exclusive-mode client with a period as close to `period` (100 ns
/// units) as the driver allows.
fn init_exclusive(device: &IMMDevice, format: Format, period: u64) -> Result<Client, Error> {
    let wave = format.wave();
    let wave: *const WAVEFORMATEX = ptr::from_ref(&wave).cast();
    let audio: IAudioClient = activate(device)?;
    let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    let hns = period as i64;
    // In event-driven exclusive mode the buffer is one period.
    // SAFETY: a fresh client and a complete format.
    match unsafe { audio.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, hns, hns, wave, None) } {
        Ok(()) => finish(audio, Api::WasapiExclusive, format, 0),
        Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // The driver wants a whole number of its own blocks. The client
            // says how many frames that comes to, and a new one tries again.
            // SAFETY: the failed client still reports the aligned size.
            let aligned = unsafe { audio.GetBufferSize() }
                .map_err(|e| os("IAudioClient::GetBufferSize", e))?;
            let hns = frames_to_hns(u64::from(aligned), format.rate) as i64;
            let audio: IAudioClient = activate(device)?;
            // SAFETY: a fresh client and a complete format.
            unsafe { audio.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, flags, hns, hns, wave, None) }
                .map_err(|e| os("IAudioClient::Initialize", e))?;
            finish(audio, Api::WasapiExclusive, format, 0)
        }
        Err(e) => Err(os("IAudioClient::Initialize", e)),
    }
}

/// The rest of the setup, once a client is initialized. `period` is ignored
/// in exclusive mode, where it's the buffer.
fn finish(audio: IAudioClient, api: Api, format: Format, period: u32) -> Result<Client, Error> {
    // SAFETY: an initialized client.
    unsafe {
        let buffer = audio
            .GetBufferSize()
            .map_err(|e| os("IAudioClient::GetBufferSize", e))?;
        let event = Event::auto_reset()?;
        audio
            .SetEventHandle(event.0)
            .map_err(|e| os("IAudioClient::SetEventHandle", e))?;
        let render: IAudioRenderClient = audio
            .GetService()
            .map_err(|e| os("IAudioClient::GetService(IAudioRenderClient)", e))?;
        let clock: IAudioClock = audio
            .GetService()
            .map_err(|e| os("IAudioClient::GetService(IAudioClock)", e))?;
        let clock_rate = clock
            .GetFrequency()
            .map_err(|e| os("IAudioClock::GetFrequency", e))?;
        if clock_rate == 0 {
            return Err(Error::Os {
                call: "IAudioClock::GetFrequency",
                message: "the device clock has no frequency".into(),
            });
        }
        let latency = audio
            .GetStreamLatency()
            .map_err(|e| os("IAudioClient::GetStreamLatency", e))?;
        let exclusive = api == Api::WasapiExclusive;
        let period = if exclusive {
            buffer
        } else {
            period.clamp(1, buffer)
        };
        Ok(Client {
            audio,
            render,
            clock,
            event,
            api,
            format,
            period,
            buffer,
            target: if exclusive {
                buffer
            } else {
                period::fill_target(period, buffer)
            },
            clock_rate,
            stream_latency: hns_to_frames(latency.max(0) as u64, format.rate) as u32,
        })
    }
}

/// How many channels an endpoint's shared-mode format has.
pub(super) fn channels(device: &IMMDevice) -> Result<u16, Error> {
    let audio: IAudioClient = activate(device)?;
    let mix = mix_format(&audio)?;
    // SAFETY: WASAPI's own format.
    let (_, channels, _) = unsafe { basics(mix.0) };
    Ok(channels)
}

pub(super) fn capabilities(device: &IMMDevice) -> Result<Capabilities, Error> {
    let audio: IAudioClient = activate(device)?;
    let mix = mix_format(&audio)?;
    // SAFETY: WASAPI's own format.
    let (rate, channels, mask) = unsafe { basics(mix.0) };
    let (mut default, mut min) = (0, 0);
    // SAFETY: an activated client.
    unsafe { audio.GetDevicePeriod(Some(&mut default), Some(&mut min)) }
        .map_err(|e| os("IAudioClient::GetDevicePeriod", e))?;
    let default = hns_to_frames(default.max(0) as u64, rate) as u32;
    let shared = audio
        .cast::<IAudioClient3>()
        .ok()
        .and_then(|audio| engine_periods(&audio, mix.0));
    let (min_period, default_period) = match shared {
        Some(periods) => (periods.min, periods.default),
        None => (default, default),
    };
    let sample_rates: Vec<u32> = COMMON_RATES
        .into_iter()
        .filter(|&rate| exclusive_format(&audio, rate, channels, mask).is_ok())
        .collect();
    let exclusive = (!sample_rates.is_empty()).then(|| ExclusiveCapabilities {
        sample_rates,
        min_period: hns_to_frames(min.max(0) as u64, rate) as u32,
    });
    Ok(Capabilities {
        sample_rate: rate,
        channels,
        min_period: Some(min_period),
        default_period: Some(default_period),
        exclusive,
        asio: None,
    })
}
