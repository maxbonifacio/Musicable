//! The open project, wired to the engine.

use std::cell::Cell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use musicable_audio::{InputBuffer, MidiBuffer};
use musicable_commands::{
    Args, BatchError, CommandError, CommandId, Context, Document, Edit, Editor, KeyChord, Keymap,
    Registry, Request, Steps, Transport, UnknownCommand, Value, auto_warp, mixer, take, transport,
    warp,
};
use musicable_engine::compile::keys;
use musicable_engine::media::Media;
use musicable_engine::modulation;
use musicable_engine::nodes::{Gain, InputMonitor, Metronome};
use musicable_engine::switch::Switch;
use musicable_engine::{
    Compiled, Compiler, Devices, EngineConfig, EngineHandle, Event, Graph, InputLayout, Lead,
    MAX_WRITERS, Meters, NodeKey, NodeLoads, ParamId, PreparedGraph, Preview, PreviewAt, QueueFull,
    Stats, SwapError, TapWriter, TransportCommand, compile_with_inputs, compile_with_stems,
};
use musicable_library::Busy;
use musicable_model::clip::ClipContent;
use musicable_model::modulation::Routing;
use musicable_model::param::ParamAddress;
use musicable_model::plugin::PluginKind;
use musicable_model::routing::Tap;
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::WarpMode;
use musicable_model::{DeviceId, Input, Monitor, PluginFormat, Project, TrackId, TrackKind};
use musicable_plugins::{Instance, PluginEvent, PluginInfo, Waker};

use crate::command_log::{CommandLog, Ran};
use crate::library::{Library, LibraryLink, ToEngine};
use crate::media::{MediaPool, follows_pitch};
use crate::output_recorder::OutputRecorder;
use crate::plugins::{Change, Plugins};
use crate::recorder::{
    self, Armed, Buffers, Place, Recorder, Recording, Report, TapInput, TapNotes, Taps,
};

/// Builds the graph for a project, with its meters, its audio files, its
/// plugins' nodes and the audio inputs it monitors, on the compiler's
/// thread.
type Build = Arc<dyn Fn(&Project, &Meters, &Media, &Devices, &InputLayout) -> Graph + Send + Sync>;

/// The folder recordings go in, in the folder of the project's file.
const RECORDINGS: &str = "Recordings";

/// The open project, the commands that change it, and the engine that plays
/// it.
///
/// Key presses and menus run commands. Commands change the project through
/// transactions, and after each command the session sends the engine what
/// changed, so the engine plays the project as it is. Graphs compile on a
/// thread of their own, so a slow one never holds up the window.
/// `docs/commands.md` describes the flow.
///
/// Audio clips play files from the folder the project's file is in, which
/// the session is told when it opens a project. They load as graphs build:
/// short files whole, and longer ones streaming from disk.
///
/// Plugin devices play through instances of their plugins, which the
/// session loads from the plugins it's told are installed, and keeps in
/// step with the project. What changes in a plugin's own window comes back
/// into the project, so undo covers it.
///
/// Armed audio tracks record from the audio inputs the session is told the
/// stream opened ([`set_inputs`](Self::set_inputs)) while the transport
/// records, into files in a `Recordings` folder by the project's file, or
/// for a project that isn't saved, in the user's music folder. Those that
/// take their input from another track's tap, or resample the master,
/// record what the engine writes of the tap for the recorder. Armed MIDI
/// tracks record the notes their MIDI inputs play. When recording stops,
/// what each track recorded becomes takes, in one step of the undo
/// history.
///
/// Capture does the same with what armed tracks, and those monitoring In,
/// played over the transport's last run without recording, from the last
/// few minutes of their inputs, which the recorder holds once
/// [`set_capture_seconds`](Self::set_capture_seconds) says how long.
///
/// Audio takes make up for a device's latency that its driver doesn't
/// report once [`set_recording_offset`](Self::set_recording_offset) says
/// how much.
pub struct Session {
    document: Document,
    commands: Registry,
    keymap: Keymap,
    engine: EngineLink,
    plugins: Plugins,
    recorder: Recorder,
    /// Where recordings of a project that isn't saved go.
    unsaved_recordings: Option<PathBuf>,
    /// How long Capture holds the inputs of the tracks it takes, in
    /// seconds: 0 holds nothing.
    capture_seconds: u64,
    /// How many frames late audio inputs' buffers have what they captured,
    /// beyond the latency their drivers report.
    recording_offset: i64,
    /// Counts the projects opened, to tell a recording of an earlier one.
    opened: u64,
    /// The buffers the engine writes the taps that armed tracks take into,
    /// and with Capture on, those that held tracks take, for the recorder.
    taps: Taps,
    /// What the plugins' windows asked for, oldest first.
    window_events: Vec<(DeviceId, PluginEvent)>,
    /// What went wrong with plugins as they played.
    problems: Vec<String>,
    /// What commands asked of the app, oldest first.
    requests: Vec<Request>,
    /// Counts the project's changes.
    revision: u64,
    /// The browser's library, which previews through the engine.
    library: Library,
    /// How loud previews play: 1.0 as they are.
    preview_gain: f32,
    /// How far ahead of the device the engine renders.
    lead: Lead,
    /// The commands run this session.
    command_log: CommandLog,
    /// A recording of what the audio device plays, while one is on.
    output_recorder: Option<OutputRecorder>,
}

impl Session {
    /// Opens `project`, whose file is in `folder`, on the engine `handle`
    /// drives: compiles the project's graph and sends its tempo map and
    /// loop. Key presses go through `keymap`. A project that isn't saved has
    /// no folder.
    ///
    /// It returns before the graph is ready. Until the graph reaches the
    /// engine, what's sent to the engine waits for it: the engine plays
    /// silence, and doesn't start playing.
    pub fn new(
        project: Project,
        folder: Option<&Path>,
        keymap: Keymap,
        handle: EngineHandle,
    ) -> Self {
        let media = MediaPool::new(false);
        Self::building(
            project,
            folder,
            keymap,
            handle,
            media,
            Arc::new(compile_with_inputs),
            false,
        )
    }

    /// Like [`new`](Self::new), for an engine that renders offline, faster
    /// or slower than real time, such as the null test's: audio clips that
    /// stream wait for the disk, where a session playing on a device would
    /// play silence until it caught up.
    pub fn offline(
        project: Project,
        folder: Option<&Path>,
        keymap: Keymap,
        handle: EngineHandle,
    ) -> Self {
        let media = MediaPool::new(true);
        Self::building(
            project,
            folder,
            keymap,
            handle,
            media,
            Arc::new(compile_with_inputs),
            false,
        )
    }

    /// Like [`offline`](Self::offline), for an export: the plugins of
    /// `project` load from `installed` before it plays, and the graphs tap
    /// the tracks in `stems`, in that order, for the engine's
    /// [`process_with_taps`](musicable_engine::Engine::process_with_taps)
    /// (see [`compile_with_stems`]). Key presses do nothing. The engine
    /// renders nothing ahead, so the graphs are whole
    /// ([`Graph::prepare_whole`]): what would render live renders alongside
    /// the rest, and pipelined with it.
    pub fn exporting(
        project: Project,
        folder: Option<&Path>,
        handle: EngineHandle,
        installed: Vec<PluginInfo>,
        stems: Vec<TrackId>,
    ) -> Self {
        let build = move |project: &Project,
                          meters: &Meters,
                          media: &Media,
                          devices: &Devices,
                          inputs: &InputLayout| {
            compile_with_stems(project, meters, media, devices, inputs, &stems)
        };
        let media = MediaPool::new(true);
        let mut session = Self::building(
            project,
            folder,
            Keymap::new(),
            handle,
            media,
            Arc::new(build),
            true,
        );
        session.set_installed_plugins(installed);
        session
    }

    /// Like [`new`](Self::new), for a project without a folder, with `build`
    /// building the project's graphs in place of [`compile_with_inputs`]:
    /// for tests, such as one with a node that's slow to prepare. The
    /// session's meters read nothing, and audio clips, plugins and inputs
    /// are silent.
    pub fn with_compiler(
        project: Project,
        keymap: Keymap,
        handle: EngineHandle,
        build: impl Fn(&Project) -> Graph + Send + Sync + 'static,
    ) -> Self {
        let build =
            move |project: &Project, _: &Meters, _: &Media, _: &Devices, _: &InputLayout| {
                build(project)
            };
        let media = MediaPool::new(false);
        Self::building(project, None, keymap, handle, media, Arc::new(build), false)
    }

    fn building(
        project: Project,
        folder: Option<&Path>,
        keymap: Keymap,
        mut handle: EngineHandle,
        media: MediaPool,
        build: Build,
        whole: bool,
    ) -> Self {
        let plugins = Plugins::new(handle.config());
        let recorder = Recorder::spawn();
        if let Some(timeline) = handle.take_timeline() {
            recorder.set_timeline(timeline);
        }
        let mut engine = EngineLink {
            playing: handle.stats().playing,
            recording: false,
            located: Cell::new(None),
            tempo: project.tempo.clone(),
            handle,
            compiler: Compiler::spawn(),
            build,
            whole,
            media: Arc::new(media),
            folder: folder.map(Path::to_owned),
            devices: Devices::new(),
            inputs: InputLayout::new(),
            asked: 0,
            received: 0,
            floor: 0,
            outbox: VecDeque::new(),
            meters: Meters::new(),
            gates: BTreeMap::new(),
            loads: None,
        };
        engine.set_up(&project);
        engine.flush();
        let mut document = Document::new(project);
        document.editor_mut().sample_rate = engine.handle.config().sample_rate;
        let mut session = Self {
            document,
            commands: crate::commands(),
            keymap,
            engine,
            plugins,
            recorder,
            unsaved_recordings: recorder::unsaved_folder(),
            capture_seconds: 0,
            recording_offset: 0,
            opened: 0,
            taps: Taps::default(),
            window_events: Vec::new(),
            problems: Vec::new(),
            requests: Vec::new(),
            revision: 0,
            library: Library::none(),
            preview_gain: 1.0,
            lead: Lead::Off,
            command_log: CommandLog::default(),
            output_recorder: None,
        };
        session.arm();
        session
    }

    /// Opens `project`, whose file is in `folder`, in place of the open one.
    /// The transport stops, and the playhead goes to the start of the song.
    /// What was waiting for the engine for the old project is dropped, and
    /// the undo history starts again.
    pub fn open(&mut self, project: Project, folder: Option<&Path>) {
        // What's waiting is for the old project, and so are the values the
        // engine would send again: the new project's nodes can have the same
        // keys.
        self.engine.outbox.clear();
        self.engine.handle.forget_params();
        self.engine.gates.clear();
        self.engine.meters = Meters::new();
        self.engine.folder = folder.map(Path::to_owned);
        self.engine.media.forget();
        self.engine.stop();
        self.engine.locate(Beats::ZERO);
        self.plugins.clear();
        self.window_events.clear();
        self.plugins.sync(&project);
        self.engine.devices = self.plugins.devices();
        self.engine.set_up(&project);
        self.engine.flush();
        // How editing behaves is the user's, not the project's.
        let behaviour = self.document.editor().behaviour;
        self.document = Document::new(project);
        let editor = self.document.editor_mut();
        editor.behaviour = behaviour;
        editor.sample_rate = self.engine.handle.config().sample_rate;
        self.revision += 1;
        self.opened += 1;
        // The new project's taps are other tracks', even with the same ids.
        self.restart_taps();
        self.arm();
    }

    /// Plays the project on the engine `handle` drives from now on, such as
    /// one built for another audio device or buffer size, and hands back the
    /// old engine's handle.
    ///
    /// The new engine takes over where the old one was: its playhead goes to
    /// the same place in bars and beats, and it plays if the old one did,
    /// once the project's graph for it is ready. What the old engine hadn't
    /// received yet, it never will. The meters and the modulators' readings
    /// start again, but for the meters' highest readings.
    pub fn replace_engine(&mut self, mut handle: EngineHandle) -> EngineHandle {
        let at = self.playhead();
        let playing = self.engine.playing;
        // A recording stops where the old engine got to.
        if let Some(timeline) = handle.take_timeline() {
            self.recorder.set_timeline(timeline);
        }
        let old = std::mem::replace(&mut self.engine.handle, handle);
        self.engine.outbox.clear();
        self.engine.gates.clear();
        // The new engine's time starts again from 0, and its graph measures
        // into the same meters.
        self.engine.meters.restart();
        self.engine.loads = None;
        self.engine.playing = false;
        self.engine.recording = false;
        self.plugins.set_config(self.engine.handle.config());
        self.engine.set_up(self.document.project());
        self.engine.locate(at);
        if playing {
            self.engine.play();
        }
        self.engine.flush();
        self.document.editor_mut().sample_rate = self.engine.handle.config().sample_rate;
        self.engine.handle.set_preview_gain(self.preview_gain);
        self.engine.handle.set_lead(self.lead);
        // The new engine writes no taps yet.
        self.restart_taps();
        self.arm();
        old
    }

    /// The audio inputs the stream opened are laid out among the engine's
    /// device inputs as `layout` says, and capture into `buffers`, in the
    /// same order, and its MIDI inputs, in the layout's order too, bring
    /// what they play into `midi`: tracks monitor and record them from now
    /// on. Call it whenever the stream opens. Until it's first called,
    /// tracks have no inputs.
    pub fn set_inputs(
        &mut self,
        layout: InputLayout,
        buffers: Vec<Option<InputBuffer>>,
        midi: Vec<Option<MidiBuffer>>,
    ) {
        let buffers = Buffers {
            audio: buffers,
            midi,
        };
        self.recorder.set_inputs(layout.clone(), buffers);
        if self.engine.inputs != layout {
            self.engine.inputs = layout;
            self.rebuild();
            self.engine.flush();
        }
    }

    /// The audio inputs the project's tracks want open: those of armed
    /// audio tracks, and of those that always monitor. `""` is the default
    /// input.
    pub fn wanted_inputs(&self) -> BTreeSet<String> {
        self.document
            .project()
            .tracks
            .iter()
            .filter(|track| track.armed || track.monitor == Monitor::In)
            .filter_map(|track| match &track.input {
                Some(Input::Audio(input)) if track.kind == TrackKind::Audio => {
                    Some(input.device.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The MIDI inputs the project's tracks want open: those of armed MIDI
    /// tracks, and of those that always monitor. `""` is every MIDI input.
    pub fn wanted_midi_inputs(&self) -> BTreeSet<String> {
        self.document
            .project()
            .tracks
            .iter()
            .filter(|track| track.armed || track.monitor == Monitor::In)
            .filter_map(|track| match &track.input {
                Some(Input::Midi(input)) if track.kind == TrackKind::Midi => {
                    Some(input.device.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Whether recording is on: whether playing records the armed tracks,
    /// counting what hasn't reached the engine yet.
    pub fn is_recording(&self) -> bool {
        self.engine.recording
    }

    /// Whether the transport plays, counting what hasn't reached the engine
    /// yet: from the moment Play is pressed, where [`stats`](Self::stats)
    /// says so once the device plays that far, which while the engine
    /// renders ahead can be the lead and a block later.
    pub fn is_playing(&self) -> bool {
        self.engine.playing
    }

    /// What the armed tracks have recorded so far, while they record, for
    /// showing it: song positions in frames at the engine's rate.
    pub fn recordings(&self) -> Vec<Recording> {
        self.recorder.live()
    }

    /// Where recordings of projects that aren't saved go from now on, such
    /// as a folder of a test's own. By default, a `Musicable` folder in the
    /// user's music folder.
    pub fn set_unsaved_recordings(&mut self, folder: Option<PathBuf>) {
        self.unsaved_recordings = folder;
        self.arm();
    }

    /// Holds the last `seconds` of the inputs of armed tracks, and of those
    /// monitoring In, at least, for [`capture`](Self::capture), from now
    /// on. With 0, the default, it holds nothing.
    pub fn set_capture_seconds(&mut self, seconds: u64) {
        self.capture_seconds = seconds;
        self.arm();
    }

    /// Makes up for `frames` of the audio inputs' latency that their
    /// drivers don't report, from the next recording or capture on: each
    /// audio take starts that much later in what its input captured. What a
    /// cable from an output to an input measures, as the device's recording
    /// offset. 0 by default. MIDI takes are left as they are.
    pub fn set_recording_offset(&mut self, frames: i64) {
        if frames != self.recording_offset {
            self.recording_offset = frames;
            self.arm();
        }
    }

    /// The recording offset, in frames.
    pub fn recording_offset(&self) -> i64 {
        self.recording_offset
    }

    /// Makes what the armed tracks, and those monitoring In, played over
    /// the transport's last run, where it didn't record, into takes,
    /// as if they had recorded it then: one step of the undo history, named
    /// Capture, once the recorder has written it. If there's nothing to
    /// take, the problems say why.
    pub fn capture(&mut self) {
        self.recorder.capture(self.engine.handle.stats().time);
    }

    /// Tells the recorder which tracks to record from the next recording
    /// on, and where, and which inputs to hold for Capture, and has the
    /// engine write the taps they take for it.
    fn arm(&mut self) {
        let project = self.document.project();
        let audio_offset = self.recording_offset;
        let tracks = |held: bool| -> Vec<Armed> {
            (project.tracks.iter())
                .filter(move |track| track.armed || (held && track.monitor == Monitor::In))
                .filter_map(move |track| {
                    let input = track.input.as_ref()?;
                    // Sound for an audio track, notes for a MIDI track.
                    let takes = match (input, track.kind) {
                        (Input::Audio(_), TrackKind::Audio) | (Input::Midi(_), TrackKind::Midi) => {
                            true
                        }
                        (Input::Tap(tap), TrackKind::Audio) => project.carries_sound(*tap),
                        (Input::Tap(tap), TrackKind::Midi) => project.carries_notes(*tap),
                        _ => false,
                    };
                    takes.then_some((track, input))
                })
                .map(|(track, input)| Armed {
                    track: track.id,
                    name: track.name.clone(),
                    kind: track.kind,
                    input: input.clone(),
                    offset: match input {
                        Input::Audio(_) => audio_offset,
                        Input::Midi(_) | Input::Tap(_) => 0,
                    },
                })
                .collect()
        };
        let armed = tracks(false);
        let held = tracks(true);
        // Capture holds the tracks monitoring In too, but only when it's on.
        let taking = if self.capture_seconds > 0 {
            &held
        } else {
            &armed
        };
        let (mut sound, mut notes) = (BTreeSet::new(), BTreeSet::new());
        for track in taking {
            match (track.notes_tap(), track.input.tap()) {
                (Some(tap), _) => notes.insert(tap),
                (None, Some(tap)) => sound.insert(tap),
                (None, None) => false,
            };
        }
        self.write_taps(sound, notes);
        let place = match &self.engine.folder {
            Some(folder) => Some(Place {
                folder: folder.join(RECORDINGS),
                project: Some(folder.clone()),
            }),
            None => self.unsaved_recordings.clone().map(|folder| Place {
                folder,
                project: None,
            }),
        };
        let rate = self.engine.handle.config().sample_rate;
        // The taps first, so the tracks find them.
        self.recorder.set_taps(self.taps.clone());
        self.recorder.set_tracks(rate, armed, place, self.opened);
        self.recorder.set_held(held, self.capture_seconds);
    }

    /// Has the engine write the sound of the taps in `sound`, and the notes
    /// of those in `notes`, into buffers for the recorder, those it doesn't
    /// yet into new ones, and stop writing the others. As many as it can
    /// write, sound first: past that, tracks taking the others record
    /// nothing.
    fn write_taps(&mut self, sound: BTreeSet<Tap>, notes: BTreeSet<Tap>) {
        let sound: BTreeSet<Tap> = sound.into_iter().take(MAX_WRITERS).collect();
        let room = MAX_WRITERS - sound.len();
        let notes: BTreeSet<Tap> = notes.into_iter().take(room).collect();
        let engine = &mut self.engine;
        self.taps.sound.retain(|tap, _| {
            let kept = sound.contains(tap);
            if !kept {
                engine.push(Message::Writer(keys::record(*tap), None));
            }
            kept
        });
        self.taps.notes.retain(|tap, _| {
            let kept = notes.contains(tap);
            if !kept {
                engine.push(Message::Writer(keys::record_notes(*tap), None));
            }
            kept
        });
        let rate = self.engine.handle.config().sample_rate;
        for tap in sound {
            if let Entry::Vacant(entry) = self.taps.sound.entry(tap) {
                let (buffer, writer) = TapInput::new(rate);
                entry.insert(buffer);
                let writer: Box<dyn TapWriter> = Box::new(writer);
                (self.engine).push(Message::Writer(keys::record(tap), Some(writer)));
            }
        }
        for tap in notes {
            if let Entry::Vacant(entry) = self.taps.notes.entry(tap) {
                let (buffer, writer) = TapNotes::new();
                entry.insert(buffer);
                let writer: Box<dyn TapWriter> = Box::new(writer);
                (self.engine).push(Message::Writer(keys::record_notes(tap), Some(writer)));
            }
        }
    }

    /// Starts the taps the engine writes for the recorder over, as for a
    /// new project, whose taps are other tracks', or a new engine: what
    /// waited for the engine is gone, the engine stops writing the taps it
    /// has, and arming gives those wanted new buffers.
    fn restart_taps(&mut self) {
        for key in self.engine.handle.writers().to_vec() {
            self.engine.push(Message::Writer(key, None));
        }
        self.taps = Taps::default();
    }

    /// Starts recording what the audio device plays to `file`, a WAV file,
    /// with a timeline beside it once it stops: see [`OutputRecorder`].
    /// Says where it records, or why it can't.
    pub fn record_output(&mut self, file: &Path) -> Result<serde_json::Value, String> {
        if let Some(recording) = &self.output_recorder {
            return Err(format!(
                "Musicable is recording its output to {} already: stop that first.",
                recording.file().display()
            ));
        }
        let recorder = OutputRecorder::start(&mut self.engine.handle, file)?;
        self.output_recorder = Some(recorder);
        Ok(serde_json::json!({
            "file": file,
            "timeline": crate::output_recorder::timeline_path(file),
        }))
    }

    /// Stops recording what the audio device plays, and writes the
    /// timeline: what it wrote, or why it couldn't.
    pub fn stop_recording_output(&mut self) -> Result<serde_json::Value, String> {
        let recorder = self
            .output_recorder
            .take()
            .ok_or("Musicable isn't recording its output.")?;
        let project = self.document.project();
        recorder.stop(&mut self.engine.handle, project)
    }

    /// The file the output records to, while it does.
    pub fn recording_output(&self) -> Option<&Path> {
        self.output_recorder.as_ref().map(OutputRecorder::file)
    }

    pub fn project(&self) -> &Project {
        self.document.project()
    }

    /// The arrangement's editing state: the selection, the start marker,
    /// the grid and the zoom.
    pub fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// The same, to change, such as when the app reads an audio file's
    /// header into [`Editor::files`]. Changing it changes nothing the
    /// engine plays.
    pub fn editor_mut(&mut self) -> &mut Editor {
        self.document.editor_mut()
    }

    /// Goes up with every change to the project, undone and redone ones
    /// too, and when another project opens: for what follows the project,
    /// such as the files its clips play.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Gives the warped clips that haven't taken their transients yet those
    /// analysis found in their files, where the editor has the analyses,
    /// then sends the engine what changed: a change the project follows,
    /// not a step in the undo history. Returns whether any did. See
    /// [`warp::take_transients`].
    pub fn take_transients(&mut self) -> bool {
        let took = warp::take_transients(&mut self.document);
        if took {
            self.sync();
        }
        took
    }

    /// Gives the graphs the pitch marks analysis found in the project's
    /// audio files, where the editor has the analyses, for clips in
    /// Granular to fit their grains to with Follow pitch on, and compiles a
    /// new graph where such a clip plays a file whose marks are new.
    /// Returns whether it did.
    pub fn take_pitch_marks(&mut self) -> bool {
        let mut new = BTreeSet::new();
        for (file, analysis) in &self.document.editor().analyses {
            if self.engine.media.keep_marks(file, &analysis.pitch) {
                new.insert(file.as_str());
            }
        }
        let mut clips = self.document.project().tracks.iter();
        let rebuild = clips.any(|track| {
            track.clips.iter().any(|clip| match &clip.content {
                ClipContent::Audio(audio) => follows_pitch(audio) && new.contains(&*audio.file),
                ClipContent::Midi(_) => false,
            })
        });
        if rebuild {
            self.rebuild();
            self.engine.flush();
        }
        rebuild
    }

    /// Auto-warps the audio files dropped before they'd been analysed whose
    /// analyses are all in, which `waits_for` says of each file, then sends
    /// the engine what changed. Returns whether anything did. See
    /// [`auto_warp::apply_pending`].
    pub fn auto_warp(&mut self, waits_for: impl Fn(&str) -> bool) -> bool {
        if self.document.editor().auto_warp.is_empty() {
            return false;
        }
        let warped = auto_warp::apply_pending(&mut self.document, waits_for);
        if warped {
            self.sync();
        }
        warped
    }

    /// Whether the project changed since it was opened or last saved.
    pub fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.document.mark_saved();
    }

    /// The steps of the undo history, by name, for the History panel and
    /// the Edit menu.
    pub fn history(&self) -> Steps<'_> {
        self.document.steps()
    }

    /// The name of the change Undo would take back.
    pub fn undo_name(&self) -> Option<&str> {
        self.document.undo_name()
    }

    /// The name of the change Redo would make again.
    pub fn redo_name(&self) -> Option<&str> {
        self.document.redo_name()
    }

    /// Starts a gesture, such as dragging the tempo field: until
    /// [`end_gesture`](Self::end_gesture), its changes are one step in the
    /// undo history. See [`Document::begin_gesture`].
    pub fn begin_gesture(&mut self) {
        self.document.begin_gesture();
    }

    pub fn end_gesture(&mut self) {
        self.document.end_gesture();
    }

    pub fn commands(&self) -> &Registry {
        &self.commands
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Key presses go through `keymap` from now on.
    pub fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = keymap;
    }

    /// What the engine renders for: its sample rate and block size.
    pub fn engine_config(&self) -> EngineConfig {
        self.engine.handle.config()
    }

    /// The engine's counters, as of its last block: the playhead, whether it
    /// plays, and what went wrong.
    pub fn stats(&self) -> Stats {
        self.engine.handle.stats()
    }

    /// Where the playhead is in the song, as of the engine's last block.
    pub fn playhead(&self) -> Beats {
        self.engine.playhead()
    }

    /// Where the playhead is in what's heard, in frames from the start of
    /// the song, for showing it: while the transport plays, what plays now
    /// left the clips the running graph's latency, and the audio device's
    /// `output_latency` frames, before the engine's position. See
    /// `docs/engine.md`, on delay compensation.
    ///
    /// Until a locate reaches the engine, it's where the locate puts the
    /// playhead, so the playhead doesn't stay where it was for the frames
    /// that takes.
    pub fn heard_position(&self, output_latency: u64) -> u64 {
        let stats = self.engine.handle.stats();
        if let Some((_, frame)) = self.engine.located(&stats) {
            return frame;
        }
        match stats.playing {
            true => stats
                .position
                .saturating_sub(stats.latency + output_latency),
            false => stats.position,
        }
    }

    /// Where the engine's frame `position` is in the song, by the project's
    /// tempo map.
    pub fn beats_at(&self, position: u64) -> Beats {
        self.engine.beats_at(position)
    }

    /// Whether the keymap binds `chord` to a command that applies now, so a
    /// press of it runs one. Other keys can go elsewhere.
    pub fn takes_key(&self, chord: KeyChord) -> bool {
        self.keymap
            .lookup_focused(chord, self.document.editor().focus)
            .is_some_and(|id| self.commands.applies(id, &self.document))
    }

    /// Runs the command the keymap binds to `chord` where the focus is: the
    /// focused panel's own, or else the one everywhere. Returns `false` if
    /// there is none, so the key can go elsewhere.
    pub fn key_pressed(&mut self, chord: KeyChord) -> bool {
        let focus = self.document.editor().focus;
        let Some(id) = self.keymap.lookup_focused(chord, focus).cloned() else {
            return false;
        };
        // A keymap can bind a command this version doesn't have.
        self.run(&id).is_ok()
    }

    /// Runs the command `id` without arguments, as a menu or a key does,
    /// then sends the engine what it changed. A command that needs
    /// arguments, or doesn't apply now, does nothing.
    pub fn run(&mut self, id: &CommandId) -> Result<(), UnknownCommand> {
        let mut library = LibraryLink {
            library: &mut self.library,
            plugins: &mut self.plugins,
            folder: self.engine.folder.clone(),
        };
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.engine,
            requests: &mut self.requests,
            library: &mut library,
        };
        let ran = self.commands.run_with(id, &Args::new(), &mut context);
        if let Err(CommandError::Unknown(_)) = ran {
            return Err(UnknownCommand(id.clone()));
        }
        let error = ran.err().map(|error| error.to_string());
        self.command_log.add(Ran::One(id, &Args::new()), error);
        self.sync();
        Ok(())
    }

    /// Runs the command `id` with `args`, then sends the engine what it
    /// changed, and returns the command's result. See
    /// [`Registry::run_with`].
    pub fn run_with(&mut self, id: &CommandId, args: &Args) -> Result<Value, CommandError> {
        let mut library = LibraryLink {
            library: &mut self.library,
            plugins: &mut self.plugins,
            folder: self.engine.folder.clone(),
        };
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.engine,
            requests: &mut self.requests,
            library: &mut library,
        };
        let result = self.commands.run_with(id, args, &mut context);
        let error = result.as_ref().err().map(ToString::to_string);
        self.command_log.add(Ran::One(id, args), error);
        self.sync();
        result
    }

    /// Runs `commands` in order as one step in the undo history called
    /// `name`, all or none, then sends the engine what they changed, and
    /// returns their results. See [`Registry::run_batch`].
    pub fn run_batch(
        &mut self,
        name: &str,
        commands: &[(CommandId, Args)],
    ) -> Result<Vec<Value>, BatchError> {
        let mut library = LibraryLink {
            library: &mut self.library,
            plugins: &mut self.plugins,
            folder: self.engine.folder.clone(),
        };
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.engine,
            requests: &mut self.requests,
            library: &mut library,
        };
        let result = self.commands.run_batch(name, commands, &mut context);
        let error = result.as_ref().err().map(ToString::to_string);
        self.command_log.add(Ran::Batch(name, commands), error);
        self.sync();
        result
    }

    /// The commands run this session.
    pub(crate) fn command_log(&self) -> &CommandLog {
        &self.command_log
    }

    pub(crate) fn command_log_mut(&mut self) -> &mut CommandLog {
        &mut self.command_log
    }

    /// What the commands run so far asked the app to do, such as opening a
    /// file, oldest first. The app carries them out after each command.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Sets the tempo at the playhead to `bpm`, moving every other tempo in
    /// the song by as much, then sends the engine the new tempo map. For a
    /// tempo field: see [`transport::SET_TEMPO`].
    pub fn set_tempo(&mut self, bpm: f64) {
        let args = Args::new().with("bpm", bpm);
        // Only a tempo that isn't a number fails, and it changes nothing.
        let _ = self.run_with(&transport::SET_TEMPO, &args);
    }

    /// Makes a change from the mixer's controls, through the command it
    /// names, then sends the engine what it changed. See
    /// [`mixer::Change::command`]. The mixer's controls only name tracks
    /// the project has, so it fails only for an output or a send that
    /// would close a loop through a wire.
    pub fn change_mixer(&mut self, change: mixer::Change) -> Result<(), CommandError> {
        let (id, args) = change.command();
        self.run_with(&id, &args).map(|_| ())
    }

    /// The level meters of the project's tracks and of the master, as the
    /// engine last measured them.
    pub fn meters(&self) -> &Meters {
        &self.engine.meters
    }

    /// How long each node of the graph last sent to the engine takes, once
    /// one has been sent. They're whole once the engine has installed it
    /// ([`NodeLoads::installed`]).
    pub fn loads(&self) -> Option<&Arc<NodeLoads>> {
        self.engine.loads.as_ref()
    }

    /// What went wrong reading the project's audio files and loading its
    /// plugins since the last call, such as a file or a plugin that's
    /// missing, for the app to show. Each file's problem comes once.
    pub fn take_problems(&mut self) -> Vec<String> {
        let mut problems = self.engine.media.take_problems();
        problems.extend(self.plugins.take_problems());
        problems.extend(self.library.take_problems());
        problems.append(&mut self.problems);
        problems
    }

    /// Finds the project's plugins among `installed` from now on, such as
    /// when a scan finds new ones, and loads the plugins of the devices that
    /// have none yet. Until it's first called, plugin devices play nothing.
    pub fn set_installed_plugins(&mut self, installed: Vec<PluginInfo>) {
        self.plugins.set_installed(installed);
        if self.plugins.sync(self.document.project()) {
            self.rebuild();
        }
        self.engine.flush();
    }

    /// The plugins the session finds the project's plugins among.
    pub fn installed_plugins(&self) -> &[PluginInfo] {
        self.plugins.installed()
    }

    /// What `plugin` does with notes: what the user marked it as
    /// ([`browser::SET_PLUGIN_KIND`]), or what the scan found.
    ///
    /// [`browser::SET_PLUGIN_KIND`]: musicable_commands::browser::SET_PLUGIN_KIND
    pub fn plugin_kind(&self, plugin: &PluginInfo) -> PluginKind {
        self.plugins.kind_of(plugin)
    }

    /// What the user marked plugins as, by format and id.
    pub fn plugin_kinds(&self) -> &BTreeMap<(PluginFormat, String), PluginKind> {
        self.plugins.kinds()
    }

    /// Has the plugins marked as in `kinds`, as the settings file has them.
    pub fn set_plugin_kinds(&mut self, kinds: BTreeMap<(PluginFormat, String), PluginKind>) {
        self.plugins.set_kinds(kinds);
    }

    /// Whether `plugin` is set to Render Ahead Never
    /// ([`browser::SET_PLUGIN_RENDER_AHEAD`]), so it renders live wherever
    /// it is.
    ///
    /// [`browser::SET_PLUGIN_RENDER_AHEAD`]: musicable_commands::browser::SET_PLUGIN_RENDER_AHEAD
    pub fn plugin_never_ahead(&self, plugin: &PluginInfo) -> bool {
        self.plugins.never_ahead(plugin)
    }

    /// The plugins set to Render Ahead Never, by format and id.
    pub fn plugins_never_ahead(&self) -> &BTreeSet<(PluginFormat, String)> {
        self.plugins.never_ahead_marks()
    }

    /// Has the plugins set to Render Ahead Never as in `never`, as the
    /// settings file has them.
    pub fn set_plugins_never_ahead(&mut self, never: BTreeSet<(PluginFormat, String)>) {
        self.plugins.set_never_ahead_marks(never);
    }

    /// Whether the plugin of the device `device` says it must render live,
    /// rather than being set to Render Ahead Never.
    pub fn plugin_must_render_live(&self, device: DeviceId) -> bool {
        self.plugins.must_render_live(device)
    }

    /// Whether the user marked a plugin as another kind, or set its Render
    /// Ahead, since this last said, for the app to save and list it.
    pub fn take_plugin_marks_changed(&mut self) -> bool {
        self.plugins.take_marks_changed()
    }

    /// Plugins loaded from now on call `waker`, from any thread but an
    /// audio thread, when they need [`tick`](Self::tick) called.
    pub fn set_plugin_waker(&mut self, waker: Waker) {
        self.plugins.set_waker(waker);
    }

    /// The instance of the plugin device `device`'s plugin, if it loaded,
    /// for its window.
    pub fn plugin(&self, device: DeviceId) -> Option<&Instance> {
        self.plugins.instance(device)
    }

    pub fn plugin_mut(&mut self, device: DeviceId) -> Option<&mut Instance> {
        self.plugins.instance_mut(device)
    }

    /// Why the plugin device `device`'s plugin didn't load, if it didn't.
    pub fn plugin_problem(&self, device: DeviceId) -> Option<&str> {
        self.plugins.problem(device)
    }

    /// What the plugins' windows asked for since the last call, oldest
    /// first: to be another size, or to close.
    pub fn take_window_events(&mut self) -> Vec<(DeviceId, PluginEvent)> {
        std::mem::take(&mut self.window_events)
    }

    /// When [`tick`](Self::tick) is next due for a timer a plugin set.
    pub fn next_plugin_timer(&self) -> Option<Instant> {
        self.plugins.next_timer()
    }

    /// The project as it's saved to a file: with each plugin's state saved
    /// afresh, so it has whatever the plugin keeps that its parameters
    /// don't say.
    pub fn saved_project(&mut self) -> Project {
        self.plugins.saved(self.document.project())
    }

    /// Call regularly, such as once per UI frame, and when a plugin's waker
    /// says so. Carries out what the plugins asked for and takes in what
    /// changed in them, adds what was recorded, sends the graphs the
    /// compiler has finished and what the engine had no room for earlier,
    /// and drops the graphs and tempo maps the engine has replaced.
    pub fn tick(&mut self) {
        let changes = self.plugins.poll();
        if !changes.is_empty() {
            self.follow_plugins(changes);
        }
        self.take_recordings();
        self.tick_library();
        self.engine.handle.collect_garbage();
        self.engine.flush();
    }

    /// Takes in what the library's jobs found, hands the engine the
    /// previews it opened, and tells the library what the engine is doing.
    fn tick_library(&mut self) {
        self.library.tick();
        for message in self.library.take_for_engine() {
            // A preview the engine has no room for is dropped: the user can
            // ask for it again.
            let _ = match message {
                ToEngine::Play { id, source } => {
                    let preview = Preview::new(id, source, self.engine.handle.config());
                    self.engine.handle.preview(preview).map_err(|_| ())
                }
                ToEngine::Stop => self.engine.handle.stop_preview().map_err(|_| ()),
            };
        }
        let busy = match (self.engine.handle.stats().playing, self.engine.recording) {
            (false, _) => Busy::Idle,
            (true, false) => Busy::Playing,
            (true, true) => Busy::Recording,
        };
        self.library.set_busy(busy);
    }

    /// The browser's library.
    pub fn library(&self) -> &Library {
        &self.library
    }

    pub fn library_mut(&mut self) -> &mut Library {
        &mut self.library
    }

    /// Uses `library` from now on, in place of one that has nothing.
    pub fn set_library(&mut self, library: Library) {
        self.library = library;
    }

    /// Plays `preview` in place of the one playing, as the browser's
    /// previews play: for the Blind Test window. Returns whether the engine
    /// had room for it.
    pub fn play_sound(&mut self, preview: Preview) -> bool {
        self.engine.handle.preview(preview).is_ok()
    }

    /// Stops the preview playing, if one is.
    pub fn stop_sound(&mut self) {
        let _ = self.engine.handle.stop_preview();
    }

    /// The preview playing, and where it is, if one is.
    pub fn sound_at(&self) -> Option<PreviewAt> {
        self.engine.handle.stats().preview
    }

    /// Plays previews at `gain` from now on: 1.0 plays them as they are.
    pub fn set_preview_gain(&mut self, gain: f32) {
        self.preview_gain = gain;
        self.engine.handle.set_preview_gain(gain);
    }

    /// Has the engine render `lead` ahead of the device from now on, all
    /// but what plays live: see
    /// [`EngineHandle::set_lead`](musicable_engine::EngineHandle::set_lead).
    /// It renders each block as the device asks for it until this is
    /// called.
    pub fn set_lead(&mut self, lead: Lead) {
        self.lead = lead;
        self.engine.handle.set_lead(lead);
        // With the lead off nothing renders ahead, so the graph renders
        // whole, in one block; with it on, split, so the rest renders ahead
        // of what renders live. The graph playing until the new one comes
        // renders the same either way.
        let whole = lead == Lead::Off;
        if whole != self.engine.whole {
            self.engine.whole = whole;
            self.engine.recompile(self.document.project());
        }
    }

    /// How far ahead of the device the engine renders.
    pub fn lead(&self) -> Lead {
        self.lead
    }

    /// Adds the recordings that have finished as takes, and passes on what
    /// went wrong recording.
    fn take_recordings(&mut self) {
        let mut added = false;
        while let Some(report) = self.recorder.try_recv() {
            match report {
                Report::Finished {
                    project,
                    rate,
                    recorded,
                    files,
                    captured,
                } if project == self.opened => {
                    let editor = self.document.editor_mut();
                    editor.files.extend(files);
                    if captured {
                        take::add_capture(&mut self.document, rate, &recorded);
                    } else {
                        take::add_recording(&mut self.document, rate, &recorded);
                    }
                    added = true;
                }
                Report::Finished { files, .. } => {
                    let files: Vec<_> = files.into_iter().map(|(name, _)| name).collect();
                    self.problems.push(format!(
                        "A recording finished after its project closed. Its files are {}.",
                        files.join(", ")
                    ));
                }
                Report::Problem(problem) => self.problems.push(problem),
            }
        }
        if added {
            self.sync();
        }
    }

    /// Makes what changed in the plugins changes to the project: what
    /// someone changed in a plugin's window as transactions named after the
    /// plugin, so undo takes them back, and what a plugin changed on its own
    /// as changes the project follows, which aren't steps in the undo
    /// history.
    fn follow_plugins(&mut self, changes: Vec<(DeviceId, Change)>) {
        // A plugin's latency, or whether it must render live, is in how the
        // graph is built.
        let mut rebuild = false;
        for (device, change) in changes {
            let name = format!("Change {}", self.plugins.name(device).unwrap_or("Plugin"));
            match change {
                Change::GestureBegin => self.document.begin_gesture(),
                Change::GestureEnd => self.document.end_gesture(),
                // Moved by hand, so it overrides the parameter's automation.
                Change::Param {
                    param,
                    value,
                    by_hand: true,
                } => self.document.transact(&name, |tx| {
                    tx.set_param_by_hand(ParamAddress::Device(device, param), value);
                }),
                Change::Param { param, value, .. } => self.document.follow(&name, |tx| {
                    tx.set_param_value(ParamAddress::Device(device, param), value);
                }),
                // The plugin's own Bypass is its device's: by hand, it
                // overrides the device's Bypass lane, as the Bypass button
                // does.
                Change::Bypass { bypassed, by_hand } => {
                    let verb = if bypassed { "Bypass" } else { "Bring Back" };
                    let name = format!("{verb} {}", self.plugins.name(device).unwrap_or("Plugin"));
                    let (param, value) = (ParamAddress::Bypass(device), f64::from(bypassed));
                    if by_hand {
                        self.document
                            .transact(&name, |tx| tx.set_param_by_hand(param, value));
                    } else {
                        self.document
                            .follow(&name, |tx| tx.set_param_value(param, value));
                    }
                }
                Change::Settings {
                    settings,
                    by_hand: true,
                } => self.document.transact(&name, |tx| {
                    tx.set_plugin_settings(device, settings);
                }),
                Change::Settings { settings, .. } => self.document.follow(&name, |tx| {
                    tx.set_plugin_settings(device, settings);
                }),
                Change::Latency | Change::Live => rebuild = true,
                Change::Inputs(inputs) => self.document.follow(&name, |tx| {
                    tx.set_plugin_inputs(device, inputs);
                }),
                Change::NoteOutput(on) => self.document.follow(&name, |tx| {
                    tx.set_plugin_note_output(device, on);
                }),
                Change::Window(event) => self.window_events.push((device, event)),
                Change::Failed(problem) => self.problems.push(problem),
            }
        }
        if rebuild {
            self.rebuild();
        }
        self.sync();
    }

    /// Compiles a new graph for the project, with the plugins' nodes as
    /// they are now.
    fn rebuild(&mut self) {
        self.engine.devices = self.plugins.devices();
        self.engine.recompile(self.document.project());
    }

    /// Whether a graph the session asked for is still compiling. While one
    /// is, call [`tick`](Self::tick) often, to send it on as soon as it's
    /// ready.
    pub fn compiling(&self) -> bool {
        self.engine.received < self.engine.asked
    }

    /// Whether the engine has everything the session has sent it: no graph
    /// is compiling, and nothing waits for one or for room in the engine's
    /// queue. Call [`tick`](Self::tick) to get there.
    pub fn is_caught_up(&self) -> bool {
        !self.compiling() && self.engine.outbox.is_empty()
    }

    /// Stops the transport at engine time `time` (see [`Stats::time`]), to
    /// the frame: for an offline render that knows when it ends, such as an
    /// export. Like everything sent, it waits for the graph.
    pub fn stop_at(&mut self, time: u64) {
        let event = Event::Transport(TransportCommand::Stop);
        self.engine.push(Message::EventAt { time, event });
        self.engine.flush();
    }

    /// Waits for the graphs the session asked for, and sends them on, with
    /// what waited for them. For offline playing, such as tests and the null
    /// test, where nothing should start before the graph.
    ///
    /// # Panics
    ///
    /// If building or preparing a graph panicked, with that panic.
    pub fn wait_for_graph(&mut self) {
        while self.compiling() {
            let compiled = self.engine.compiler.recv();
            self.engine.receive(compiled);
        }
        self.engine.flush();
    }

    /// Sends the engine what the committed transactions changed: parameter
    /// values where a node has the value, and a new graph where the
    /// project's shape changed.
    fn sync(&mut self) {
        let mut tempo = None;
        let mut loop_changed = false;
        let mut punch_changed = false;
        let mut gates_changed = false;
        let mut armed_changed = false;
        let mut recompile = false;
        let committed = self.document.take_committed();
        let changed = !committed.is_empty();
        if changed {
            self.revision += 1;
        }
        for committed in committed {
            for edit in committed.edits {
                let mut param = |node: NodeKey, param: ParamId, value: f32| {
                    self.engine.push(Message::Event(Event::Param {
                        node,
                        param,
                        value: f64::from(value),
                    }));
                };
                match edit {
                    // Only the last map counts. Clips in Slice are cut on
                    // grids of bars as they're compiled, those in Spectral
                    // render for the song's tempo, and those in Granular
                    // place their grains for it.
                    Edit::Tempo { from, to } => {
                        let renders = plays_for_the_tempo(self.document.project());
                        if from.signatures() != to.signatures() || renders {
                            recompile = true;
                        }
                        self.engine.tempo = to.clone();
                        tempo = Some(to);
                    }
                    // Punching in and out happens at the loop's ends.
                    Edit::LoopRegion { .. } => {
                        loop_changed = true;
                        punch_changed = true;
                    }
                    Edit::Looping { .. } => loop_changed = true,
                    Edit::PunchIn { .. } | Edit::PunchOut { .. } => punch_changed = true,
                    Edit::Metronome { to, .. } => self.engine.push(Message::Event(Event::Param {
                        node: keys::METRONOME,
                        param: Metronome::ENABLED,
                        value: if to { 1.0 } else { 0.0 },
                    })),
                    Edit::TrackGain { track, to, .. } => {
                        param(keys::track_gain(track), Gain::GAIN, to);
                    }
                    Edit::TrackPan { track, to, .. } => {
                        param(keys::track_gain(track), Gain::PAN, to);
                    }
                    Edit::SendLevel { send, to, .. } => param(keys::send(send), Gain::GAIN, to),
                    Edit::WireGain { wire, to, .. } => param(keys::wire(wire), Gain::GAIN, to),
                    Edit::MasterGain { to, .. } => param(keys::master_gain(), Gain::GAIN, to),
                    Edit::MasterPan { to, .. } => param(keys::master_gain(), Gain::PAN, to),
                    Edit::TrackMuted { .. }
                    | Edit::TrackSolo { .. }
                    | Edit::TrackSoloSafe { .. } => {
                        gates_changed = true;
                    }
                    // At once, and in a graph built again, where a track
                    // that can monitor its input renders live, with what it
                    // reaches, and one that can't renders ahead.
                    Edit::TrackArmed { track, to, .. } => {
                        let armed = if to { 1.0 } else { 0.0 };
                        param(keys::track_input(track), InputMonitor::ARMED, armed);
                        armed_changed = true;
                        recompile = true;
                    }
                    Edit::TrackMonitor { track, to, .. } => {
                        let monitor = InputMonitor::monitor_value(to) as f32;
                        param(keys::track_input(track), InputMonitor::MONITOR, monitor);
                        // Capture holds the inputs of tracks monitoring In.
                        armed_changed = true;
                        recompile = true;
                    }
                    // A graph built again has the track render live, or as
                    // the rest of the project does.
                    Edit::TrackAlwaysLive { .. } => recompile = true,
                    // Recordings are named after their tracks.
                    Edit::TrackName { .. } => armed_changed = true,
                    // Nothing the engine plays, or nothing yet. Takes play
                    // through the clips comping makes of them.
                    Edit::ClipName { .. }
                    | Edit::AddTake { .. }
                    | Edit::RemoveTake { .. }
                    | Edit::TakeName { .. }
                    | Edit::AddLocator { .. }
                    | Edit::RemoveLocator { .. }
                    | Edit::LocatorAt { .. }
                    | Edit::LocatorName { .. } => {}
                    // The plugins have their settings, not the engine: the
                    // plugins follow below.
                    Edit::PluginParam { .. } | Edit::PluginSettings { .. } => {}
                    // A built-in device's node has its parameters.
                    Edit::NativeParam {
                        device, param, to, ..
                    } => self.engine.push(Message::Event(Event::Param {
                        node: keys::device(device),
                        param: ParamId(param.into()),
                        value: to,
                    })),
                    // A sampler reads its sample as the graph is built.
                    Edit::NativeSample { .. } => recompile = true,
                    // A device's switch bypasses it, and the plugins follow
                    // below.
                    Edit::DeviceBypass { device, to, .. } => {
                        self.engine.push(Message::Event(Event::Param {
                            node: keys::device(device),
                            param: Switch::BYPASS,
                            value: if to { 1.0 } else { 0.0 },
                        }));
                    }
                    // Tracks and their inputs are compiled into the graph,
                    // and the recorder records the armed ones.
                    Edit::AddTrack { .. } | Edit::RemoveTrack { .. } | Edit::TrackInput { .. } => {
                        recompile = true;
                        armed_changed = true;
                    }
                    // Clips are compiled into the graph, notes and all. A
                    // note edited while it sounds is let go.
                    Edit::TrackOutput { .. }
                    | Edit::TrackClipType { .. }
                    | Edit::AddSend { .. }
                    | Edit::RemoveSend { .. }
                    | Edit::SendPreFader { .. }
                    | Edit::MasterClipType { .. }
                    | Edit::AddClip { .. }
                    | Edit::RemoveClip { .. }
                    | Edit::ClipTiming { .. }
                    | Edit::ClipFades { .. }
                    | Edit::ClipWarping { .. }
                    | Edit::ClipGain { .. }
                    | Edit::AddNote { .. }
                    | Edit::RemoveNote { .. }
                    | Edit::NoteTiming { .. }
                    | Edit::NoteKey { .. }
                    | Edit::NoteVelocity { .. }
                    | Edit::Controller { .. }
                    | Edit::AddDevice { .. }
                    | Edit::RemoveDevice { .. }
                    | Edit::MoveDevice { .. }
                    | Edit::DeviceActive { .. }
                    | Edit::AddWire { .. }
                    | Edit::RemoveWire { .. }
                    | Edit::WireFrom { .. }
                    | Edit::WireChannel { .. } => recompile = true,
                    // Which side inputs a plugin has says which wires into
                    // it play. With none, the graph stays as it is, so a
                    // plugin listing its inputs as it loads doesn't rebuild
                    // it.
                    Edit::PluginInputs { device, .. } => {
                        let project = self.document.project();
                        recompile |= project.wires.iter().any(|w| w.to.device() == device);
                    }
                    // Whether a plugin has a note output says whether what
                    // takes its notes gets any. With nothing taking them,
                    // the graph stays as it is.
                    Edit::PluginNoteOutput { device, .. } => {
                        let project = self.document.project();
                        let tap = Tap::Device(device);
                        recompile |= project.wires.iter().any(|w| w.from == tap)
                            || (project.tracks.iter()).any(|t| t.input == Some(Input::Tap(tap)));
                    }
                    // So is automation, and the plugins leave what it moves
                    // to the engine.
                    Edit::AddLane { .. }
                    | Edit::RemoveLane { .. }
                    | Edit::LanePoints { .. }
                    | Edit::LaneOverridden { .. } => recompile = true,
                    // Names, and the order of a strip, change nothing the
                    // modulators play.
                    Edit::ModulatorName { .. }
                    | Edit::ControlName { .. }
                    | Edit::MoveModulator { .. } => {}
                    // A channel's modulation node has its modulators'
                    // parameters, and its routings' amounts.
                    Edit::ModulatorParam {
                        modulator,
                        param,
                        to,
                        ..
                    } => {
                        let project = self.document.project();
                        if let Some((chain, ..)) = project.find_modulator(modulator) {
                            self.engine.push(Message::Event(Event::Param {
                                node: keys::modulation(chain),
                                param: modulation::param_id(modulator, param),
                                value: to,
                            }));
                        }
                    }
                    Edit::RoutingSettings { routing, from, to }
                        if Routing {
                            amount: to.amount,
                            ..from
                        } == to =>
                    {
                        let project = self.document.project();
                        if let Some((chain, ..)) = project.find_routing(routing) {
                            self.engine.push(Message::Event(Event::Param {
                                node: keys::modulation(chain),
                                param: modulation::amount_id(routing),
                                value: to.amount,
                            }));
                        }
                    }
                    // Modulators, their controls and their routings are
                    // compiled into the graph, and so is what else a
                    // routing's settings change: its curve, its scale, its
                    // lag, and whether it plays.
                    Edit::AddModulator { .. }
                    | Edit::RemoveModulator { .. }
                    | Edit::AddControl { .. }
                    | Edit::RemoveControl { .. }
                    | Edit::AddRouting { .. }
                    | Edit::RemoveRouting { .. }
                    | Edit::RoutingSettings { .. } => recompile = true,
                }
            }
        }
        // Plugins load for new devices, and go with the ones that went, in
        // tracks too, and take in their settings.
        if changed && self.plugins.sync(self.document.project()) {
            recompile = true;
        }
        // A plugin set to render live, or ahead.
        if self.plugins.take_live_changed() {
            recompile = true;
        }
        if let Some(tempo) = tempo {
            self.engine.push(Message::Tempo(tempo));
        }
        // One message, with the loop as it ends up, covers every change to it.
        // It can go after the tempo maps: the engine keeps the loop in beats.
        if loop_changed {
            let event = loop_event(self.document.project());
            self.engine.push(Message::Event(event));
        }
        if punch_changed {
            let event = punch_event(self.document.project());
            self.engine.push(Message::Event(event));
        }
        if armed_changed {
            self.arm();
        }
        if recompile {
            self.rebuild();
        } else if gates_changed {
            self.engine.set_gates(self.document.project(), false);
        }
        self.engine.flush();
    }
}

/// Whether any of `project`'s clips plays in Spectral, which renders for
/// the song's tempo, or in Granular, which places its grains for it.
fn plays_for_the_tempo(project: &Project) -> bool {
    let mut clips = project.tracks.iter().flat_map(|track| &track.clips);
    clips.any(|clip| match &clip.content {
        ClipContent::Audio(audio) => (audio.warp.as_ref())
            .is_some_and(|w| matches!(w.mode, WarpMode::Spectral | WarpMode::Granular)),
        ClipContent::Midi(_) => false,
    })
}

/// Sets the engine's loop to the project's.
fn loop_event(project: &Project) -> Event {
    let region = project.looping.then_some(project.loop_region);
    Event::Transport(TransportCommand::Loop(region))
}

/// Sets the engine's punch range to the project's: recording punches in at
/// the loop's start and out at its end, where punching in and out are on.
fn punch_event(project: &Project) -> Event {
    Event::Transport(TransportCommand::Punch {
        start: project.punch_in.then_some(project.loop_region.start),
        end: project.punch_out.then_some(project.loop_region.end),
    })
}

/// The engine, as commands see it, and the compiler that builds its graphs.
struct EngineLink {
    handle: EngineHandle,
    compiler: Compiler,
    build: Build,
    /// Whether it prepares the graphs whole, as for an engine that renders
    /// nothing ahead ([`Graph::prepare_whole`]), or split into the half
    /// that renders ahead and the half that renders live.
    whole: bool,
    /// The audio files the graphs play.
    media: Arc<MediaPool>,
    /// The folder of the project's file, which audio clips' relative paths
    /// start from.
    folder: Option<PathBuf>,
    /// The nodes of the plugins the graphs play.
    devices: Devices,
    /// Where the audio inputs the stream opened are among the engine's
    /// device inputs, for the graphs to monitor them.
    inputs: InputLayout,
    /// The newest compiler job asked for.
    asked: u64,
    /// The newest job whose graph has come back.
    received: u64,
    /// The first job whose graph is for this project and this engine. Older
    /// ones are dropped when they come back.
    floor: u64,
    /// What the engine's queue had no room for yet, oldest first.
    outbox: VecDeque<Message>,
    /// Whether the transport plays, counting what hasn't reached the engine
    /// yet.
    playing: bool,
    /// Whether recording is on, counting what hasn't reached the engine
    /// yet.
    recording: bool,
    /// Where the last locate sent while the transport stood still puts the
    /// playhead, until the engine says the playhead is there, or plays, or
    /// a stop is sent: see [`located`](Self::located).
    located: Cell<Option<Beats>>,
    /// The project's tempo map, for the playhead in beats.
    tempo: TempoMap,
    /// The meters the graphs measure into.
    meters: Meters,
    /// Whether each output stage and send mutes, as last sent to the engine
    /// or built into a graph. Kept for nodes that have gone as well, since
    /// the engine sends their last value again if they come back.
    gates: BTreeMap<NodeKey, bool>,
    /// The loads of the last graph sent to the engine.
    loads: Option<Arc<NodeLoads>>,
}

impl Drop for EngineLink {
    fn drop(&mut self) {
        // Before the compiler goes, which waits for the graph it's building:
        // nothing will play it.
        self.media.stop();
    }
}

/// Something for the engine, in the order it was sent.
enum Message {
    Event(Event),
    /// An event for engine time `time`.
    EventAt {
        time: u64,
        event: Event,
    },
    Tempo(TempoMap),
    Graph(Box<PreparedGraph>),
    /// A writer for the record taps with this key, or `None` to stop
    /// writing them.
    Writer(NodeKey, Option<Box<dyn TapWriter>>),
    /// Holds back everything after it until the graph of this job, or of a
    /// newer one, comes back from the compiler, and becomes that graph.
    WaitForGraph(u64),
}

impl EngineLink {
    fn push(&mut self, message: Message) {
        self.outbox.push_back(message);
    }

    /// Where the last locate sent puts the playhead, in beats and in
    /// frames, while the engine, as its `stats` say, hasn't got there yet:
    /// the locate waits in the outbox, say, or in the engine's queue for
    /// its next block, or for the audio device to start. `None` once it
    /// has, and the engine's position is the playhead's.
    ///
    /// The engine takes in what's sent in order, so once it plays the play
    /// that went after the locate, it has located too. A stop sent after
    /// that forgets the locate, in case nothing asked while it played.
    fn located(&self, stats: &Stats) -> Option<(Beats, u64)> {
        let at = self.located.get()?;
        let rate = self.handle.config().sample_rate;
        let frame = self.tempo.frame_at(at, rate).max(0) as u64;
        if stats.position == frame || (self.playing && stats.playing) {
            self.located.set(None);
            return None;
        }
        Some((at, frame))
    }

    /// Where the engine's frame `position` is in the song.
    fn beats_at(&self, position: u64) -> Beats {
        let rate = self.handle.config().sample_rate;
        Beats::from_f64(self.tempo.beats_at(position as f64, rate))
    }

    /// Compiles `project` on the compiler's thread, for the engine, and
    /// sends the graph on when it's ready. The running graph plays on until
    /// then, and parameter values sent meanwhile reach the new graph too
    /// (see [`EngineHandle::swap_graph`]). Returns the compiler's job.
    fn recompile(&mut self, project: &Project) -> u64 {
        self.meters.follow(project);
        // The new graph has every gate built in. The running one needs the
        // ones that changed now.
        self.set_gates(project, true);
        let build = self.build.clone();
        let meters = self.meters.clone();
        let media = self.media.clone();
        let folder = self.folder.clone();
        let devices = self.devices.clone();
        let inputs = self.inputs.clone();
        let project = project.clone();
        let playhead = self.playhead();
        let job = move || {
            // Files load here, on the compiler's thread, with the graph, and
            // clips in Spectral ask for their segments.
            let media = media.media(&project, folder.as_deref(), playhead);
            build(&project, &meters, &media, &devices, &inputs)
        };
        let config = self.handle.config();
        self.asked = match self.whole {
            true => self.compiler.compile_whole(config, job),
            false => self.compiler.compile(config, job),
        };
        self.asked
    }

    /// Sends what the engine needs to play `project`: its graph, its tempo
    /// map, its loop and its punch range. All of it, and whatever comes
    /// after, waits for the graph.
    fn set_up(&mut self, project: &Project) {
        self.tempo = project.tempo.clone();
        let job = self.recompile(project);
        // Graphs still to come from before were for another project or
        // another engine.
        self.floor = job;
        self.push(Message::WaitForGraph(job));
        self.push(Message::Tempo(project.tempo.clone()));
        self.push(Message::Event(loop_event(project)));
        self.push(Message::Event(punch_event(project)));
    }

    /// Sets the engine's mute gates, the output stages', the sends' and the
    /// note wires', to what mute and solo say now, sending only the ones
    /// that changed. With
    /// `compiled`, a graph built from `project` is on its way, with every
    /// gate already set, so only gates the engine might send an old value of
    /// again need sending.
    fn set_gates(&mut self, project: &Project, compiled: bool) {
        let silenced = project.silenced();
        let outputs = project
            .tracks
            .iter()
            .map(|t| (keys::track_output(t.id), silenced.output(t.id)));
        let sends = project
            .tracks
            .iter()
            .flat_map(|t| &t.sends)
            .map(|s| (keys::send(s.id), silenced.send(s.id)));
        let wires = (project.wires.iter())
            .filter(|w| w.takes_notes())
            .map(|w| (keys::wire(w.id), silenced.wire(w.id)));
        let mut changed = Vec::new();
        for (node, muted) in outputs.chain(sends).chain(wires) {
            match self.gates.insert(node, muted) {
                Some(was) if was == muted => {}
                None if compiled => {}
                _ => changed.push((node, muted)),
            }
        }
        for (node, muted) in changed {
            self.push(Message::Event(Event::Param {
                node,
                param: Gain::MUTE,
                value: if muted { 1.0 } else { 0.0 },
            }));
        }
    }

    /// Takes a graph back from the compiler: in the place that waits for it,
    /// if there is one, or after everything else.
    fn receive(&mut self, compiled: Compiled) {
        if compiled.job < self.floor {
            return;
        }
        self.received = self.received.max(compiled.job);
        let graph = Box::new(compiled.graph.expect("compiled graphs are valid"));
        let waiting = self
            .outbox
            .iter_mut()
            .find(|message| matches!(message, Message::WaitForGraph(_)));
        match waiting {
            Some(waiting) => *waiting = Message::Graph(graph),
            None => self.push(Message::Graph(graph)),
        }
    }

    /// Takes the graphs the compiler has finished, then sends what's
    /// waiting, oldest first, until the engine's queue is full or the next
    /// message waits for a graph.
    fn flush(&mut self) {
        while let Some(compiled) = self.compiler.try_recv() {
            self.receive(compiled);
        }
        while let Some(message) = self.outbox.pop_front() {
            if matches!(message, Message::WaitForGraph(_)) {
                self.outbox.push_front(message);
                break;
            }
            if let Err(message) = self.send(message) {
                self.outbox.push_front(message);
                break;
            }
        }
    }

    /// Sends one message, or hands it back if the engine's queue is full.
    fn send(&mut self, message: Message) -> Result<(), Message> {
        match message {
            Message::Event(event) => self
                .handle
                .send(event)
                .map_err(|QueueFull(event)| Message::Event(event)),
            Message::EventAt { time, event } => self
                .handle
                .send_at(time, event)
                .map_err(|QueueFull(event)| Message::EventAt { time, event }),
            Message::Tempo(tempo) => self
                .handle
                .set_tempo_map(tempo)
                .map_err(|QueueFull(tempo)| Message::Tempo(tempo)),
            Message::Graph(graph) => {
                let loads = graph.loads();
                let sent = self.handle.swap_graph(*graph).map_err(|error| match error {
                    SwapError::QueueFull(graph) => Message::Graph(graph),
                    SwapError::ConfigMismatch(_) => {
                        unreachable!("graphs are prepared for the engine's config")
                    }
                });
                if sent.is_ok() {
                    self.loads = Some(loads);
                }
                sent
            }
            // The session asks for no more writers than the engine takes, so
            // only a full queue gives one back.
            Message::Writer(key, Some(writer)) => (self.handle)
                .add_writer(key, writer)
                .map_err(|writer| Message::Writer(key, Some(writer))),
            Message::Writer(key, None) => (self.handle)
                .remove_writer(key)
                .map_err(|QueueFull(key)| Message::Writer(key, None)),
            Message::WaitForGraph(job) => Err(Message::WaitForGraph(job)),
        }
    }
}

impl Transport for EngineLink {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
        self.push(Message::Event(Event::Transport(TransportCommand::Play)));
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
        // Whatever locate came before the play has reached the engine, or
        // will before the stop does.
        self.located.set(None);
        self.push(Message::Event(Event::Transport(TransportCommand::Stop)));
    }

    fn locate(&mut self, at: Beats) {
        // While it plays, the playhead moves on from where the engine is.
        self.located.set((!self.playing).then_some(at));
        self.push(Message::Event(Event::Transport(TransportCommand::Locate(
            at,
        ))));
    }

    fn playhead(&self) -> Beats {
        let stats = self.handle.stats();
        match self.located(&stats) {
            Some((at, _)) => at,
            None => self.beats_at(stats.position),
        }
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
        self.push(Message::Event(Event::Transport(TransportCommand::Record(
            on,
        ))));
    }
}
