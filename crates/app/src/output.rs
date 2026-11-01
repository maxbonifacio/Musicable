//! The audio device the engine plays through: opened from the settings,
//! opened again when it comes back, and switched when the settings change,
//! with the audio and MIDI inputs the project's tracks record and monitor.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use musicable_audio::{
    self as audio, Api, DriverType, InputBuffer, InputKind, InputStats, MidiBuffer, OpenStream,
    SampleFormat, StartError, Stream, StreamInfo, StreamRequest, StreamStats,
};
use musicable_engine::{
    AHEAD_BLOCK, Engine, EngineConfig, EngineHandle, InputLayout, Lead, ParallelMonitor,
    ParallelStats, Stats,
};
use musicable_ui::LeadGauge;

use crate::Session;

/// What the engine renders for while no device has opened. It changes once
/// one does, if the device runs at another rate or asks for longer blocks.
const NO_DEVICE: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: AHEAD_BLOCK,
};

/// How long to wait before trying a device again after it failed.
const RETRY: Duration = Duration::from_secs(2);

/// How often the CPU meter and the lead gauge update.
const METER_WINDOW: Duration = Duration::from_millis(250);

/// How often the MIDI inputs are listed again while tracks want any, to
/// open those plugged in since.
const MIDI_LISTING: Duration = Duration::from_secs(2);

/// The audio device, playing what the engine renders.
///
/// The engine is built for the device: for its sample rate, with blocks as
/// long as the period the device asks for, or as the engine renders ahead
/// in ([`AHEAD_BLOCK`]), whichever is longer ([`engine_config`]). When a
/// device needs another, such as after a change of sample rate, the output
/// builds a new engine and hands it to the [`Session`], which sets it up
/// where the old one left off.
///
/// When the device fails or goes away, the engine waits, and the output tries
/// the device again every two seconds. An ASIO driver that asks to be reset,
/// after a change in its own settings, opens again at once.
///
/// The stream opens the audio and MIDI inputs it's asked to
/// ([`want_inputs`](Self::want_inputs)) as well as the output, and keeps
/// them open, the audio inputs until the device changes and the MIDI
/// inputs while they're plugged in, so arming tracks one after another
/// opens the stream again only for an input it hasn't open yet.
pub struct Output {
    /// The stream to keep open, without its inputs.
    request: StreamRequest,
    /// The inputs to open with it.
    inputs: Vec<String>,
    /// The MIDI inputs to open with it.
    midi_inputs: Vec<String>,
    /// The MIDI inputs plugged in, as last listed, and when.
    midi_listed: Option<(Instant, Vec<String>)>,
    /// The input among them that tracks taking the default input take,
    /// once one wants it: `Some(None)` if there's none.
    default_input: Option<Option<String>>,
    /// Counts the streams that have started.
    started: u64,
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
    lead: LeadMeter,
    /// The stream's dropouts when the user last reset the count.
    dropouts_from: u64,
    /// The engine's short blocks when the stream started.
    short_from: u64,
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
            .map_or(NO_DEVICE, |open| engine_config(open.info()));
        let (engine, handle, monitor) = crate::engine(config);
        let mut output = Self {
            request,
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
            midi_listed: None,
            default_input: None,
            started: 0,
            stream: None,
            idle: Some(engine),
            monitor,
            problem: None,
            retry_at: None,
            reset: false,
            meter: LoadMeter::default(),
            lead: LeadMeter::default(),
            dropouts_from: 0,
            short_from: 0,
        };
        if let Err(error) = opened.and_then(|open| output.play(open)) {
            output.failed(&error);
            notices.push(format!("The audio device didn't open: {error}."));
        }
        (output, handle)
    }

    /// An output that never opens a device, for the app's tests and for
    /// headless Musicable started without audio: the engine waits for one.
    pub(crate) fn none() -> (Self, EngineHandle) {
        let (engine, handle, monitor) = crate::engine(NO_DEVICE);
        let output = Self {
            request: StreamRequest::default(),
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
            midi_listed: None,
            default_input: None,
            started: 0,
            stream: None,
            idle: Some(engine),
            monitor,
            problem: Some("Musicable plays through no audio device".to_owned()),
            retry_at: None,
            reset: false,
            meter: LoadMeter::default(),
            lead: LeadMeter::default(),
            dropouts_from: 0,
            short_from: 0,
        };
        (output, handle)
    }

    /// Renders `frames` frames of the engine while no stream plays it, as a
    /// device would, and returns them, interleaved in stereo. For the app's
    /// tests.
    #[cfg(test)]
    pub(crate) fn play_idle(&mut self, frames: usize) -> Vec<f32> {
        let engine = self.idle.as_mut().expect("the engine waits for a stream");
        let mut played = vec![0.0; frames * 2];
        engine.process_interleaved(&mut played, 2);
        played
    }

    /// Call once per frame. Notices a stream that ended by itself, and tries
    /// the device again when it's time. `notices` says what happened.
    /// [`read_meters`](Self::read_meters) updates the CPU meter and the lead
    /// gauge.
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
    }

    /// Updates the CPU meter and the lead gauge from the stream and the
    /// engine as they are now: once a frame, and before an agent's read.
    pub fn read_meters(&mut self, session: &Session) {
        if let Some(stream) = &self.stream {
            let (now, busy) = (Instant::now(), stream.stats().busy);
            self.meter.update(now, busy, self.monitor.stats());
            self.lead.update(now, &session.stats(), session.lead());
        }
    }

    /// Plays through the stream `request` asks for from now on. If it doesn't
    /// open, the output goes back to the stream it had, and says why the new
    /// one failed.
    ///
    /// The inputs open go with the device: the audio inputs `wanted`, as
    /// [`want_inputs`](Self::want_inputs) takes them, open on the new one
    /// with it. So the stream opens once, and a take that's recording, while
    /// which the app asks for no inputs, carries on with them.
    pub fn apply(
        &mut self,
        request: StreamRequest,
        wanted: &BTreeSet<String>,
        session: &mut Session,
    ) -> Result<(), audio::Error> {
        let previous = std::mem::replace(&mut self.request, request);
        let default_input = self.default_input.take();
        let wanted = self.with_wanted(Vec::new(), wanted);
        let inputs = std::mem::replace(&mut self.inputs, wanted);
        let request = self.request.clone();
        if let Err(error) = self.start(&request, session) {
            self.request = previous;
            self.inputs = inputs;
            self.default_input = default_input;
            let previous = self.request.clone();
            if let Err(error) = self.start(&previous, session) {
                self.failed(&error);
            }
            return Err(error);
        }
        Ok(())
    }

    /// The stream to keep open, without its inputs.
    pub fn request(&self) -> &StreamRequest {
        &self.request
    }

    /// Opens the audio inputs `wanted` with the stream, by their ids, with
    /// `""` for the default input: the ASIO driver's own while one plays,
    /// or else the system's. Opens the MIDI inputs `midi` names too, with
    /// `""` for every one, while they're plugged in. The inputs open
    /// already stay open. If that takes opening the stream again and it
    /// doesn't open, the output tries again later, as when the device
    /// fails.
    pub fn want_inputs(
        &mut self,
        wanted: &BTreeSet<String>,
        midi: &BTreeSet<String>,
        session: &mut Session,
    ) {
        let inputs = self.with_wanted(self.inputs.clone(), wanted);
        let mut midi_inputs = self.midi_inputs.clone();
        if !midi.is_empty() {
            // One unplugged since it opened is forgotten, without opening
            // the stream again, so that it opens again once it's back.
            let listed = self.midi_devices().to_vec();
            self.midi_inputs.retain(|id| listed.contains(id));
            midi_inputs = midi_inputs_to_open(&self.midi_inputs, midi, &listed);
        }
        if (&inputs, &midi_inputs) == (&self.inputs, &self.midi_inputs) {
            return;
        }
        self.inputs = inputs;
        self.midi_inputs = midi_inputs;
        let request = self.request.clone();
        if let Err(error) = self.start(&request, session) {
            self.failed(&error);
        }
    }

    /// `inputs` and then the audio inputs `wanted` they haven't got, by
    /// their ids, with `""` for the default input.
    fn with_wanted(&mut self, mut inputs: Vec<String>, wanted: &BTreeSet<String>) -> Vec<String> {
        if wanted.contains("") && self.default_input.is_none() {
            self.default_input = Some(self.find_default_input());
        }
        for id in wanted {
            let id = match id.as_str() {
                "" => self.default_input.clone().flatten(),
                id => Some(id.to_owned()),
            };
            if let Some(id) = id
                && !inputs.contains(&id)
            {
                inputs.push(id);
            }
        }
        inputs
    }

    /// The ids of the MIDI inputs plugged in, listed again if the last list
    /// is [`MIDI_LISTING`] old.
    fn midi_devices(&mut self) -> &[String] {
        let now = Instant::now();
        if self
            .midi_listed
            .as_ref()
            .is_none_or(|(at, _)| now.duration_since(*at) >= MIDI_LISTING)
        {
            let devices = audio::midi_input_devices().unwrap_or_default();
            let ids = devices.into_iter().map(|device| device.id).collect();
            self.midi_listed = Some((now, ids));
        }
        self.midi_listed.as_ref().map_or(&[], |(_, ids)| ids)
    }

    /// The input tracks taking the default input record from.
    fn find_default_input(&self) -> Option<String> {
        if let Some(device) = &self.request.device
            && DriverType::of(device) == DriverType::Asio
        {
            return Some(device.clone());
        }
        let devices = audio::input_devices().ok()?;
        let default = devices
            .into_iter()
            .find(|device| device.is_default && device.kind == InputKind::Device)?;
        Some(default.id)
    }

    /// Counts the streams that have started: each has inputs of its own,
    /// for the [`Session`].
    pub fn started(&self) -> u64 {
        self.started
    }

    /// Where the playing stream's inputs are among the engine's device
    /// inputs, by their ids, and which takes the default input's place,
    /// and its MIDI inputs' order.
    pub fn input_layout(&self) -> InputLayout {
        let mut layout = InputLayout::new();
        for input in self.info().map_or(&[][..], |info| &info.inputs) {
            layout.add(
                input.id.clone(),
                input.first_channel,
                usize::from(input.channels),
            );
        }
        for input in self.info().map_or(&[][..], |info| &info.midi_inputs) {
            layout.add_midi(input.id.clone());
        }
        if let Some(Some(default)) = &self.default_input {
            layout.set_default(default.clone());
        }
        layout
    }

    /// The buffers the playing stream's inputs capture into, in the
    /// layout's order.
    pub fn input_buffers(&self) -> Vec<Option<InputBuffer>> {
        self.stream
            .as_ref()
            .map_or_else(Vec::new, Stream::input_buffers)
    }

    /// The buffers the playing stream's MIDI inputs bring what they play
    /// into, in the layout's order.
    pub fn midi_buffers(&self) -> Vec<Option<MidiBuffer>> {
        self.stream
            .as_ref()
            .map_or_else(Vec::new, Stream::midi_buffers)
    }

    /// The counters of the playing stream's inputs, in the layout's order.
    pub fn input_stats(&self) -> Vec<InputStats> {
        self.stream
            .as_ref()
            .map_or_else(Vec::new, Stream::input_stats)
    }

    /// The playing stream, as the device set it up.
    pub fn info(&self) -> Option<&StreamInfo> {
        self.stream.as_ref().map(Stream::info)
    }

    /// Why the playing stream's inputs that didn't open didn't, for the
    /// status bar: tracks taking them don't monitor or record them.
    pub fn input_problems(&self) -> Vec<String> {
        let Some(info) = self.info() else {
            return Vec::new();
        };
        let audio = (info.inputs.iter())
            .filter_map(|input| Some(("input", &input.name, input.error.as_ref()?)));
        let midi = (info.midi_inputs.iter())
            .filter_map(|input| Some(("MIDI input", &input.name, input.error.as_ref()?)));
        audio
            .chain(midi)
            .map(|(what, name, error)| format!("The {what} {name} didn't open: {error}."))
            .collect()
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

    /// How far ahead of the device the engine rendered lately, for the
    /// gauge under the CPU meter: none while Render Ahead is off or no
    /// stream plays.
    pub fn lead_gauge(&self) -> Option<LeadGauge> {
        self.stream.as_ref().and(self.lead.gauge())
    }

    /// Render Ahead, and what the engine's lead came to lately: none while
    /// no stream plays, or until a quarter of a second has gone by.
    pub(crate) fn lead_lately(&self) -> Option<(Lead, Lately)> {
        let lately = self.stream.as_ref().and(self.lead.lately)?;
        Some((self.lead.setting, lately))
    }

    /// The blocks the audio thread rendered itself since the stream
    /// started, while the engine rendered ahead, as the lead ran out.
    pub fn short_blocks(&self) -> u64 {
        self.lead.short_blocks.saturating_sub(self.short_from)
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

    /// How many frames late the device plays what the engine renders: the
    /// output latency [`summary`](Self::summary) gives, in frames. None
    /// without a device.
    pub fn latency(&self) -> u64 {
        self.stream
            .as_ref()
            .map_or(0, |stream| match stream.stats().queued {
                0 => u64::from(stream.info().latency),
                queued => queued,
            })
    }

    /// The stream, the load, rendering ahead and the dropouts in more words,
    /// for a tooltip.
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
        let lead = self.lead.describe(info.sample_rate, self.short_blocks());
        let _ = write!(text, "\n\n{lead}");
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
        let request = StreamRequest {
            inputs: self.inputs.clone(),
            midi_inputs: self.midi_inputs.clone(),
            ..request.clone()
        };
        let open = audio::open(&request)?;
        let config = engine_config(open.info());
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
        let short_from = engine.stats().short_blocks;
        match open.start(engine) {
            Ok(stream) => {
                self.stream = Some(stream);
                self.started += 1;
                self.problem = None;
                self.retry_at = None;
                self.meter = LoadMeter::default();
                self.lead = LeadMeter::default();
                self.dropouts_from = 0;
                self.short_from = short_from;
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

/// What to build the engine for `info`'s stream with: its sample rate, and
/// blocks as long as its period or as the engine renders ahead in,
/// whichever is longer. So plugins are activated for both, and the engine
/// needn't be built again as Render Ahead changes or a buffer size under
/// [`AHEAD_BLOCK`] does.
pub(crate) fn engine_config(info: &StreamInfo) -> EngineConfig {
    let config = info.engine_config();
    EngineConfig {
        max_block: config.max_block.max(AHEAD_BLOCK),
        ..config
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

/// The lead gauge: how far ahead of the device the engine rendered, over
/// the last quarter of a second or so, against the lead it aimed for, and
/// whether the audio thread had to render blocks itself as the lead ran
/// out.
#[derive(Debug, Default)]
struct LeadMeter {
    /// Render Ahead, as last seen.
    setting: Lead,
    /// When the window started, with the engine's short blocks then.
    since: Option<(Instant, u64)>,
    /// The window's readings so far while the engine rendered ahead: the
    /// leads and the leads aimed for, in frames, added up, and how many
    /// there were.
    leads: u64,
    targets: u64,
    readings: u64,
    /// The engine's short blocks, as last seen.
    short_blocks: u64,
    /// As of the last window.
    lately: Option<Lately>,
}

/// What the engine's lead came to over a quarter of a second or so.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Lately {
    /// The lead on average, in frames, while the engine rendered ahead, and
    /// the lead it aimed for: both none if it didn't, or doesn't now.
    pub(crate) lead: u64,
    pub(crate) target: u64,
    /// Whether it rendered ahead at the end, and whether the transport
    /// played then.
    pub(crate) ahead: bool,
    pub(crate) playing: bool,
    /// Whether the audio thread rendered blocks itself.
    pub(crate) short: bool,
}

impl LeadMeter {
    /// Takes the engine's counters at `now`, with Render Ahead at
    /// `setting`. A new setting starts the window again, so nothing from
    /// before it shows.
    fn update(&mut self, now: Instant, stats: &Stats, setting: Lead) {
        if setting != self.setting {
            *self = Self {
                setting,
                ..Self::default()
            };
        }
        self.short_blocks = stats.short_blocks;
        let (then, short_then) = *self.since.get_or_insert((now, stats.short_blocks));
        if stats.lead_target > 0 {
            self.leads += stats.lead.min(stats.lead_target);
            self.targets += stats.lead_target;
            self.readings += 1;
        }
        if now.saturating_duration_since(then) < METER_WINDOW {
            return;
        }
        // Rendering live at the end, whatever rendered ahead before then,
        // there's no lead to show.
        let ahead = stats.lead_target > 0;
        let readings = self.readings.max(1);
        let average = |sum: u64| if ahead { sum / readings } else { 0 };
        self.lately = Some(Lately {
            lead: average(self.leads),
            target: average(self.targets),
            ahead,
            playing: stats.playing,
            short: stats.short_blocks > short_then,
        });
        (self.leads, self.targets, self.readings) = (0, 0, 0);
        self.since = Some((now, stats.short_blocks));
    }

    /// The gauge, once a window has gone by, while Render Ahead is on.
    fn gauge(&self) -> Option<LeadGauge> {
        let lately = self.lately.filter(|_| self.setting != Lead::Off)?;
        Some(LeadGauge {
            filled: match lately.target {
                0 => 0.0,
                target => (lately.lead as f64 / target as f64) as f32,
            },
            ahead: lately.ahead,
            short: lately.short,
        })
    }

    /// What Render Ahead did lately, in a few words, at `sample_rate`, with
    /// `short_blocks` rendered by the audio thread since the stream
    /// started.
    fn describe(&self, sample_rate: u32, short_blocks: u64) -> String {
        let name = match self.setting {
            Lead::Off => {
                return "Render Ahead is off: each block renders as the device asks for it."
                    .to_owned();
            }
            Lead::Short => "Short",
            Lead::Normal => "Normal",
            Lead::Long => "Long",
        };
        let mut text = format!("Render Ahead is {name}");
        let time = |frames| millis(audio_duration(frames, sample_rate));
        match self.lately {
            Some(lately) if lately.ahead => {
                let _ = write!(
                    text,
                    ": the engine was {} ahead of the device lately, of the {} it aims for{}.",
                    time(lately.lead),
                    time(lately.target),
                    match lately.playing {
                        true => "",
                        false => " while the transport stands still",
                    }
                );
            }
            Some(_) => text.push_str(
                ", and nothing renders ahead now, as for a moment when a track arms: each block \
                 renders as the device asks for it.",
            ),
            None => text.push('.'),
        }
        if short_blocks > 0 {
            let plural = if short_blocks == 1 { "" } else { "s" };
            let _ = write!(
                text,
                " Since the device started, the audio thread rendered {short_blocks} block{plural} \
                 itself, as the lead ran out: a bigger buffer or a longer Render Ahead gives it \
                 more room."
            );
        }
        text
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

/// The MIDI inputs for a stream to open: those it has `open` that are
/// still `listed`, plugged in, then those listed that `wanted` names, with
/// `""` for every one.
fn midi_inputs_to_open(
    open: &[String],
    wanted: &BTreeSet<String>,
    listed: &[String],
) -> Vec<String> {
    let mut inputs: Vec<String> = (open.iter())
        .filter(|id| listed.contains(id))
        .cloned()
        .collect();
    for id in listed {
        if (wanted.contains("") || wanted.contains(id)) && !inputs.contains(id) {
            inputs.push(id.clone());
        }
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_inputs_open_while_they_are_plugged_in() {
        let ids = |ids: &[&str]| -> Vec<String> { ids.iter().map(|&id| id.to_owned()).collect() };
        let wanted =
            |ids: &[&str]| -> BTreeSet<String> { ids.iter().map(|&id| id.to_owned()).collect() };
        let listed = ids(&["Keystep", "Pads", "Launchkey"]);
        // Those wanted that are plugged in, after those open already.
        let open = ids(&["Pads"]);
        assert_eq!(
            midi_inputs_to_open(&open, &wanted(&["Keystep", "Arturia"]), &listed),
            ids(&["Pads", "Keystep"])
        );
        // Every one, for a track that takes them all.
        assert_eq!(midi_inputs_to_open(&[], &wanted(&[""]), &listed), listed);
        // One unplugged goes, and comes back with the next listing that
        // has it.
        let open = ids(&["Pads", "Keystep"]);
        let unplugged = ids(&["Keystep"]);
        let left = midi_inputs_to_open(&open, &wanted(&["Pads"]), &unplugged);
        assert_eq!(left, ids(&["Keystep"]));
        assert_eq!(
            midi_inputs_to_open(&left, &wanted(&["Pads"]), &listed),
            ids(&["Keystep", "Pads"])
        );
    }

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
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
        };
        let mut stats = StreamStats::default();
        assert_eq!(millis(latency(&info, &stats)), "2.7 ms");
        stats.queued = 542;
        assert_eq!(millis(latency(&info, &stats)), "11.3 ms");
        // The engine renders blocks of up to 1024 frames, as it does ahead
        // of the device, or the device's, if they're longer.
        let config = engine_config(&info);
        assert_eq!((config.sample_rate, config.max_block), (48_000, 1024));
        let long = StreamInfo {
            period: 2048,
            ..info
        };
        assert_eq!(engine_config(&long).max_block, 2048);
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

    #[test]
    fn the_lead_gauge_shows_how_much_of_the_lead_the_engine_kept() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let stats = |lead, lead_target, short_blocks| Stats {
            lead,
            lead_target,
            short_blocks,
            playing: true,
            ..Stats::default()
        };
        let gauge = |filled, ahead, short| {
            Some(LeadGauge {
                filled,
                ahead,
                short,
            })
        };
        let mut meter = LeadMeter::default();
        assert_eq!(
            meter.describe(48_000, 0),
            "Render Ahead is off: each block renders as the device asks for it."
        );
        meter.update(start, &stats(2400, 2400, 0), Lead::Normal);
        assert_eq!(meter.gauge(), None);
        assert_eq!(meter.describe(48_000, 0), "Render Ahead is Normal.");
        // Half the lead on average, over a quarter of a second.
        meter.update(at(100), &stats(1200, 2400, 0), Lead::Normal);
        meter.update(at(250), &stats(0, 2400, 0), Lead::Normal);
        assert_eq!(meter.gauge(), gauge(0.5, true, false));
        assert_eq!(
            meter.describe(48_000, 0),
            "Render Ahead is Normal: the engine was 25.0 ms ahead of the device lately, of the \
             50.0 ms it aims for."
        );
        // The audio thread had to render two blocks itself.
        meter.update(at(400), &stats(600, 2400, 2), Lead::Normal);
        meter.update(at(500), &stats(600, 2400, 2), Lead::Normal);
        assert_eq!(meter.gauge(), gauge(0.25, true, true));
        assert!(meter.describe(48_000, 2).ends_with(
            "Since the device started, the audio thread rendered 2 blocks itself, as the lead \
             ran out: a bigger buffer or a longer Render Ahead gives it more room."
        ));
        // Nothing renders ahead, as for a moment when a track arms: what
        // rendered ahead earlier in the window doesn't fill the gauge.
        meter.update(at(600), &stats(2400, 2400, 2), Lead::Normal);
        meter.update(at(750), &stats(0, 0, 2), Lead::Normal);
        assert_eq!(meter.gauge(), gauge(0.0, false, false));
        let lately = meter.lately.unwrap();
        assert_eq!((lately.lead, lately.target), (0, 0));
        assert_eq!(
            meter.describe(48_000, 0),
            "Render Ahead is Normal, and nothing renders ahead now, as for a moment when a track \
             arms: each block renders as the device asks for it."
        );
        // The transport stops, and a lead longer than the one aimed for
        // counts as full.
        let stopped = Stats {
            playing: false,
            ..stats(600, 512, 2)
        };
        meter.update(at(1000), &stopped, Lead::Normal);
        assert_eq!(meter.gauge(), gauge(1.0, true, false));
        assert_eq!(
            meter.describe(48_000, 0),
            "Render Ahead is Normal: the engine was 10.7 ms ahead of the device lately, of the \
             10.7 ms it aims for while the transport stands still."
        );
        // Another setting starts the window again: nothing from before it
        // shows.
        meter.update(at(1100), &stopped, Lead::Long);
        assert_eq!(meter.gauge(), None);
        assert_eq!(meter.describe(48_000, 0), "Render Ahead is Long.");
        meter.update(at(1350), &stopped, Lead::Long);
        assert_eq!(meter.gauge(), gauge(1.0, true, false));
        // No gauge with Render Ahead off.
        meter.update(at(1600), &stopped, Lead::Off);
        assert_eq!(meter.gauge(), None);
    }
}
