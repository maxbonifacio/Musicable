//! The audio device the engine plays through: opened from the settings,
//! opened again when it comes back, and switched when the settings change.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use musicable_audio::{
    self as audio, Api, DriverType, OpenStream, SampleFormat, StartError, Stream, StreamInfo,
    StreamRequest, StreamStats,
};
use musicable_engine::{Engine, EngineConfig, EngineHandle, ParallelMonitor, ParallelStats};

use crate::Session;

/// What the engine renders for while no device has opened. It changes once
/// one does.
const NO_DEVICE: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 512,
};

/// How long to wait before trying a device again after it failed.
const RETRY: Duration = Duration::from_secs(2);

/// How often the CPU meter updates.
const METER_WINDOW: Duration = Duration::from_millis(250);

/// The audio device, playing what the engine renders.
///
/// The engine is built for the device: for its sample rate, with the period
/// the device asks for as its block size. When a device needs another, such
/// as after a change of buffer size, the output builds a new engine and hands
/// it to the [`Session`], which sets it up where the old one left off.
///
/// When the device fails or goes away, the engine waits, and the output tries
/// the device again every two seconds. An ASIO driver that asks to be reset,
/// after a change in its own settings, opens again at once.
pub struct Output {
    /// The stream to keep open.
    request: StreamRequest,
    /// The stream playing the engine, if any.
    stream: Option<Stream>,
    /// The engine, while no stream plays it.
    idle: Option<Engine>,
    /// The parallel scheduler's counters, for the engine.
    monitor: ParallelMonitor,
    /// Why no stream plays.
    problem: Option<String>,
    /// When to try the device again, while no stream plays.
    retry_at: Option<Instant>,
    /// Whether the stream ended because an ASIO driver asked to be reset.
    reset: bool,
    meter: LoadMeter,
    /// The stream's dropouts when the user last reset the count.
    dropouts_from: u64,
}

impl Output {
    /// Opens the stream `request` asks for, with an engine built for it, and
    /// hands back the engine's handle for the [`Session`].
    ///
    /// When the requested device isn't plugged in, it plays through the
    /// default output instead. When no device opens, the engine waits for
    /// one. `notices` says what happened.
    pub fn open(request: StreamRequest, notices: &mut Vec<String>) -> (Self, EngineHandle) {
        let mut request = request;
        let mut opened = audio::open(&request);
        if matches!(opened, Err(audio::Error::DeviceNotFound))
            && let Some(device) = &request.device
        {
            let asio = DriverType::of(device) == DriverType::Asio;
            notices.push(
                if asio && !audio::driver_types().contains(&DriverType::Asio) {
                    "The audio settings name an ASIO driver, and this build of Musicable has no \
                     ASIO, so it plays through the default output."
                } else if asio {
                    "The ASIO driver in the audio settings isn't installed, so Musicable plays \
                     through the default output."
                } else {
                    "The audio device in the settings isn't plugged in, so Musicable plays \
                     through the default output."
                }
                .to_owned(),
            );
            request = StreamRequest::default();
            opened = audio::open(&request);
        }
        let config = opened
            .as_ref()
            .map_or(NO_DEVICE, |open| open.info().engine_config());
        let (engine, handle, monitor) = crate::engine(config);
        let mut output = Self {
            request,
            stream: None,
            idle: Some(engine),
            monitor,
            problem: None,
            retry_at: None,
            reset: false,
            meter: LoadMeter::default(),
            dropouts_from: 0,
        };
        if let Err(error) = opened.and_then(|open| output.play(open)) {
            output.failed(&error);
            notices.push(format!("The audio device didn't open: {error}."));
        }
        (output, handle)
    }

    /// An output that never opens a device, for the app's tests: the engine
    /// waits for one.
    #[cfg(test)]
    pub(crate) fn none() -> (Self, EngineHandle) {
        let (engine, handle, monitor) = crate::engine(NO_DEVICE);
        let output = Self {
            request: StreamRequest::default(),
            stream: None,
            idle: Some(engine),
            monitor,
            problem: Some("the tests play through no device".to_owned()),
            retry_at: None,
            reset: false,
            meter: LoadMeter::default(),
            dropouts_from: 0,
        };
        (output, handle)
    }

    /// Call once per frame. Notices a stream that ended by itself, tries the
    /// device again when it's time, and updates the CPU meter. `notices`
    /// says what happened.
    pub fn tick(&mut self, session: &mut Session, notices: &mut Vec<String>) {
        if self.stream.as_ref().is_some_and(Stream::has_ended) {
            let error = self.stop();
            // An ASIO driver whose own settings changed asks to be opened
            // again with them, which is no news until it plays.
            self.reset = matches!(error, Some(audio::Error::DriverReset));
            let reason = match error {
                Some(error) => error.to_string(),
                None => "it stopped".to_owned(),
            };
            if !self.reset {
                notices.push(format!("The audio device stopped playing: {reason}."));
            }
            self.problem = Some(reason);
            // It may well open again at once, as after a change of format in
            // the system's sound settings.
            self.retry_at = Some(Instant::now());
        }
        if self.retry_at.is_some_and(|at| Instant::now() >= at) {
            let request = self.request.clone();
            match self.start(&request, session) {
                Ok(()) if std::mem::take(&mut self.reset) => notices.push(format!(
                    "{} changed its settings, and plays with them now: {}.",
                    self.device_name(),
                    self.summary()
                )),
                Ok(()) => notices.push(format!("Playing through {}.", self.device_name())),
                Err(error) => {
                    if std::mem::take(&mut self.reset) {
                        notices.push(format!(
                            "The ASIO driver changed its settings, and didn't open again with \
                             them: {error}."
                        ));
                    }
                    self.failed(&error);
                }
            }
        }
        if let Some(stream) = &self.stream {
            let busy = stream.stats().busy;
            self.meter
                .update(Instant::now(), busy, self.monitor.stats());
        }
    }

    /// Plays through the stream `request` asks for from now on. If it doesn't
    /// open, the output goes back to the stream it had, and says why the new
    /// one failed.
    pub fn apply(
        &mut self,
        request: StreamRequest,
        session: &mut Session,
    ) -> Result<(), audio::Error> {
        if let Err(error) = self.start(&request, session) {
            let previous = self.request.clone();
            if let Err(error) = self.start(&previous, session) {
                self.failed(&error);
            }
            return Err(error);
        }
        self.request = request;
        Ok(())
    }

    /// The stream to keep open.
    pub fn request(&self) -> &StreamRequest {
        &self.request
    }

    /// The playing stream, as the device set it up.
    pub fn info(&self) -> Option<&StreamInfo> {
        self.stream.as_ref().map(Stream::info)
    }

    /// Why no stream plays.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The share of the time the audio thread was busy lately.
    pub fn load(&self) -> Option<f32> {
        self.stream.as_ref().and(self.meter.load)
    }

    /// Dropouts since the stream started or the count was reset.
    pub fn dropouts(&self) -> u64 {
        self.stream.as_ref().map_or(0, |stream| {
            stream.stats().underruns.saturating_sub(self.dropouts_from)
        })
    }

    /// Counts dropouts from zero again.
    pub fn reset_dropouts(&mut self) {
        if let Some(stream) = &self.stream {
            self.dropouts_from = stream.stats().underruns;
        }
    }

    /// The stream in a few words, such as "48 kHz · 128 samples · 5.3 ms",
    /// or why there is none.
    pub fn summary(&self) -> String {
        match &self.stream {
            Some(stream) => {
                let info = stream.info();
                format!(
                    "{} · {} samples · {}",
                    kilohertz(info.sample_rate),
                    info.period,
                    millis(latency(info, &stream.stats()))
                )
            }
            None => "No audio device".to_owned(),
        }
    }

    /// The stream, the load and the dropouts in more words, for a tooltip.
    pub fn details(&self) -> String {
        let Some(stream) = &self.stream else {
            let problem = self.problem.as_deref().unwrap_or("no device is open");
            return format!(
                "Nothing plays: {problem}. Musicable tries the device again every few seconds."
            );
        };
        let info = stream.info();
        let stats = stream.stats();
        let mut text = describe(info, latency(info, &stats));
        if let Some(load) = self.meter.load {
            let _ = write!(
                text,
                "\n\nThe audio thread was busy {:.0}% of the time lately. Its longest wake-up \
                 took {}.",
                load * 100.0,
                millis(stats.max_busy)
            );
        }
        if let Some(cores) = self.meter.cores {
            let threads = self.monitor.stats().threads;
            let _ = write!(
                text,
                "\nThe parallel scheduler has {threads} worker threads. While it shared blocks, \
                 {cores:.1} threads ran nodes at once on average."
            );
        }
        let lost = audio_duration(stats.lost_frames, info.sample_rate);
        let _ = write!(
            text,
            "\n\nDropouts since the device started: {} ({} of audio lost)",
            stats.underruns,
            millis(lost),
        );
        if stats.system_dropouts > 0 {
            let _ = write!(
                text,
                ", {} of them in the system's audio engine or driver",
                stats.system_dropouts
            );
        }
        let _ = write!(text, ". Late wake-ups: {}.", stats.late_wakeups);
        text
    }

    /// Opens the playing ASIO driver's own settings. Returns `false` when no
    /// ASIO driver plays.
    pub fn show_driver_settings(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(Stream::show_driver_settings)
    }

    /// The device's name, as the system calls it.
    pub fn device_name(&self) -> String {
        self.info().map_or("the audio device".to_owned(), |info| {
            info.device.name.clone()
        })
    }

    /// Stops the stream, if one plays, then opens the stream `request` asks
    /// for and plays the engine on it. If the stream needs an engine built
    /// for another config, a new engine takes over in the session first.
    fn start(
        &mut self,
        request: &StreamRequest,
        session: &mut Session,
    ) -> Result<(), audio::Error> {
        self.stop();
        let open = audio::open(request)?;
        let config = open.info().engine_config();
        if self.engine().config() != config {
            // The old engine's workers stop before the new engine's start.
            // Windows gives a process 32 real-time (MMCSS) threads, so on a
            // many-core machine two sets of workers at once would leave some
            // of the new ones at normal priority for good.
            drop(self.idle.take());
            let (engine, handle, monitor) = crate::engine(config);
            drop(session.replace_engine(handle));
            self.idle = Some(engine);
            self.monitor = monitor;
        }
        self.play(open)
    }

    /// Plays the engine on `open`, a stream it was built for.
    fn play(&mut self, open: OpenStream) -> Result<(), audio::Error> {
        let engine = self.idle.take().expect("the engine waits for a stream");
        match open.start(engine) {
            Ok(stream) => {
                self.stream = Some(stream);
                self.problem = None;
                self.retry_at = None;
                self.meter = LoadMeter::default();
                self.dropouts_from = 0;
                Ok(())
            }
            Err(StartError { engine, error }) => {
                self.idle = Some(*engine);
                Err(error)
            }
        }
    }

    /// The engine, while no stream plays it.
    fn engine(&self) -> &Engine {
        self.idle.as_ref().expect("the engine waits for a stream")
    }

    /// Stops the stream, if one plays, and returns the error that ended it
    /// by itself, if one did. The engine waits for the next stream.
    fn stop(&mut self) -> Option<audio::Error> {
        let stopped = self.stream.take()?.stop();
        self.idle = Some(stopped.engine);
        stopped.error
    }

    /// Notes that the device didn't open, to try it again later.
    fn failed(&mut self, error: &audio::Error) {
        self.problem = Some(error.to_string());
        self.retry_at = Some(Instant::now() + RETRY);
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // Stop the device first, so the engine is dropped here and not on the
        // audio thread.
        self.stop();
    }
}

/// The output latency: measured by the device's clock once the stream has
/// run for a moment, and estimated from the buffer sizes until then. In
/// Windows shared mode the estimate can be a few periods short.
fn latency(info: &StreamInfo, stats: &StreamStats) -> Duration {
    match stats.queued {
        0 => info.latency_duration(),
        queued => audio_duration(queued, info.sample_rate),
    }
}

/// The stream as the device set it up, in a few lines, with its `latency`.
fn describe(info: &StreamInfo, latency: Duration) -> String {
    let format = match info.format {
        SampleFormat::F32 => "32-bit float",
        SampleFormat::F64 => "64-bit float",
        SampleFormat::I32 => "32-bit",
        SampleFormat::I24 => "24-bit",
        SampleFormat::I16 => "16-bit",
    };
    let priority = if info.realtime {
        "at real-time priority"
    } else {
        "at normal priority"
    };
    let api = match info.api {
        Api::WasapiExclusive => "WASAPI exclusive mode, with the device to itself".to_owned(),
        api => api.to_string(),
    };
    format!(
        "{} through {api}.\n{}, {} channels, {format} samples. The device asks for {} samples \
         ({}) at a time, and plays them {} after they're written. The audio thread runs \
         {priority}.",
        info.device.name,
        kilohertz(info.sample_rate),
        info.channels,
        info.period,
        millis(info.period_duration()),
        millis(latency),
    )
}

/// The CPU meter: the share of the time the audio thread was busy, over the
/// last quarter of a second or so.
#[derive(Debug, Default)]
struct LoadMeter {
    /// When the window started, with the counters then.
    since: Option<(Instant, Duration, ParallelStats)>,
    load: Option<f32>,
    /// How many threads ran nodes at once, on average, while the parallel
    /// scheduler shared blocks.
    cores: Option<f64>,
}

impl LoadMeter {
    /// Takes the stream's busy time and the scheduler's counters at `now`.
    fn update(&mut self, now: Instant, busy: Duration, parallel: ParallelStats) {
        let Some((then, busy_then, parallel_then)) = self.since else {
            self.since = Some((now, busy, parallel));
            return;
        };
        let elapsed = now.saturating_duration_since(then);
        if elapsed < METER_WINDOW {
            return;
        }
        let rendering = busy.saturating_sub(busy_then);
        self.load = Some((rendering.as_secs_f64() / elapsed.as_secs_f64()) as f32);
        let wall = parallel.wall.saturating_sub(parallel_then.wall);
        let work = parallel.busy.saturating_sub(parallel_then.busy);
        self.cores = (!wall.is_zero()).then(|| work.as_secs_f64() / wall.as_secs_f64());
        self.since = Some((now, busy, parallel));
    }
}

/// A sample rate as people say it: "48 kHz", "44.1 kHz".
pub(crate) fn kilohertz(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if rate.is_multiple_of(1000) {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

/// A short time in milliseconds: "5.3 ms".
pub(crate) fn millis(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

fn audio_duration(frames: u64, sample_rate: u32) -> Duration {
    Duration::from_secs_f64(frames as f64 / f64::from(sample_rate.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_times_read_as_people_say_them() {
        assert_eq!(kilohertz(48_000), "48 kHz");
        assert_eq!(kilohertz(44_100), "44.1 kHz");
        assert_eq!(kilohertz(192_000), "192 kHz");
        assert_eq!(millis(Duration::from_micros(2667)), "2.7 ms");
        assert_eq!(audio_duration(480, 48_000), Duration::from_millis(10));
    }

    #[test]
    fn the_latency_is_measured_once_the_stream_runs() {
        let info = StreamInfo {
            device: audio::Device {
                id: "speakers".to_owned(),
                name: "Speakers".to_owned(),
                is_default: true,
                driver_type: DriverType::System,
            },
            api: Api::WasapiLowLatency,
            sample_rate: 48_000,
            channels: 2,
            format: SampleFormat::F32,
            period: 128,
            buffer: 280,
            latency: 128,
            realtime: true,
        };
        let mut stats = StreamStats::default();
        assert_eq!(millis(latency(&info, &stats)), "2.7 ms");
        stats.queued = 542;
        assert_eq!(millis(latency(&info, &stats)), "11.3 ms");
    }

    #[test]
    fn the_meter_shows_the_share_of_the_time_spent_rendering() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let busy = Duration::from_millis;
        let parallel = |busy, wall| ParallelStats {
            busy: Duration::from_millis(busy),
            wall: Duration::from_millis(wall),
            ..ParallelStats::default()
        };
        let mut meter = LoadMeter::default();
        meter.update(start, busy(100), parallel(0, 0));
        assert_eq!(meter.load, None);
        // Too soon to say.
        meter.update(at(100), busy(150), parallel(0, 0));
        assert_eq!(meter.load, None);
        // 75 ms of rendering in 300 ms, with two threads running nodes at
        // once while blocks were shared.
        meter.update(at(300), busy(175), parallel(40, 20));
        assert_eq!(meter.load, Some(0.25));
        assert_eq!(meter.cores, Some(2.0));
        // Then 250 ms flat out, without shared blocks.
        meter.update(at(550), busy(425), parallel(40, 20));
        assert_eq!(meter.load, Some(1.0));
        assert_eq!(meter.cores, None);
    }
}
