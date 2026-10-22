//! The open project, wired to the engine.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use musicable_audio::{InputBuffer, MidiBuffer};
use musicable_commands::{
    Args, CommandError, CommandId, Context, Document, Edit, Editor, KeyChord, Keymap, Registry,
    Request, Transport, UnknownCommand, Value, mixer, take, transport,
};
use musicable_engine::compile::keys;
use musicable_engine::media::Media;
use musicable_engine::nodes::{Gain, InputMonitor, Metronome};
use musicable_engine::{
    Compiled, Compiler, Devices, EngineConfig, EngineHandle, Event, Graph, InputLayout, Meters,
    NodeKey, ParamId, PreparedGraph, QueueFull, Stats, SwapError, TransportCommand,
    compile_with_inputs,
};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::{DeviceId, Input, Monitor, Project, TrackKind};
use musicable_plugins::{Instance, PluginEvent, PluginInfo, Waker};

use crate::media::MediaPool;
use crate::plugins::{Change, Plugins};
use crate::recorder::{self, Armed, Buffers, Place, Recorder, Recording, Report};

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
/// for a project that isn't saved, in the user's music folder. Armed MIDI
/// tracks record the notes their MIDI inputs play. When recording stops,
/// what each track recorded becomes takes, in one step of the undo
/// history.
pub struct Session {
    document: Document,
    commands: Registry,
    keymap: Keymap,
    engine: EngineLink,
    plugins: Plugins,
    recorder: Recorder,
    /// Where recordings of a project that isn't saved go.
    unsaved_recordings: Option<PathBuf>,
    /// Counts the projects opened, to tell a recording of an earlier one.
    opened: u64,
    /// What the plugins' windows asked for, oldest first.
    window_events: Vec<(DeviceId, PluginEvent)>,
    /// What went wrong with plugins as they played.
    problems: Vec<String>,
    /// What commands asked of the app, oldest first.
    requests: Vec<Request>,
    /// Counts the project's changes.
    revision: u64,
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
        )
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
        Self::building(project, None, keymap, handle, media, Arc::new(build))
    }

    fn building(
        project: Project,
        folder: Option<&Path>,
        keymap: Keymap,
        mut handle: EngineHandle,
        media: MediaPool,
        build: Build,
    ) -> Self {
        let plugins = Plugins::new(handle.config());
        let recorder = Recorder::spawn();
        if let Some(timeline) = handle.take_timeline() {
            recorder.set_timeline(timeline);
        }
        let mut engine = EngineLink {
            playing: handle.stats().playing,
            recording: false,
            tempo: project.tempo.clone(),
            handle,
            compiler: Compiler::spawn(),
            build,
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
            opened: 0,
            window_events: Vec::new(),
            problems: Vec::new(),
            requests: Vec::new(),
            revision: 0,
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
        self.arm();
    }

    /// Plays the project on the engine `handle` drives from now on, such as
    /// one built for another audio device or buffer size, and hands back the
    /// old engine's handle.
    ///
    /// The new engine takes over where the old one was: its playhead goes to
    /// the same place in bars and beats, and it plays if the old one did,
    /// once the project's graph for it is ready. What the old engine hadn't
    /// received yet, it never will.
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

    /// Tells the recorder which tracks to record from the next recording
    /// on, and where.
    fn arm(&mut self) {
        let project = self.document.project();
        let armed = project
            .tracks
            .iter()
            .filter(|track| track.armed)
            .filter_map(|track| match (&track.input, track.kind) {
                (Some(input @ Input::Audio(_)), TrackKind::Audio)
                | (Some(input @ Input::Midi(_)), TrackKind::Midi) => Some(Armed {
                    track: track.id,
                    name: track.name.clone(),
                    input: input.clone(),
                    offset: 0,
                }),
                _ => None,
            })
            .collect();
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
        self.recorder.set_tracks(rate, armed, place, self.opened);
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

    /// Whether the project changed since it was opened or last saved.
    pub fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.document.mark_saved();
    }

    /// The name of the change Undo would take back, for the Edit menu.
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
    pub fn heard_position(&self, output_latency: u64) -> u64 {
        let stats = self.engine.handle.stats();
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
            .lookup(chord)
            .is_some_and(|id| self.commands.applies(id, &self.document))
    }

    /// Runs the command the keymap binds to `chord`. Returns `false` if there
    /// is none, so the key can go elsewhere.
    pub fn key_pressed(&mut self, chord: KeyChord) -> bool {
        let Some(id) = self.keymap.lookup(chord).cloned() else {
            return false;
        };
        // A keymap can bind a command this version doesn't have.
        self.run(&id).is_ok()
    }

    /// Runs the command `id` without arguments, as a menu or a key does,
    /// then sends the engine what it changed. A command that needs
    /// arguments, or doesn't apply now, does nothing.
    pub fn run(&mut self, id: &CommandId) -> Result<(), UnknownCommand> {
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.engine,
            requests: &mut self.requests,
        };
        self.commands.run(id, &mut context)?;
        self.sync();
        Ok(())
    }

    /// Runs the command `id` with `args`, then sends the engine what it
    /// changed, and returns the command's result. See
    /// [`Registry::run_with`].
    pub fn run_with(&mut self, id: &CommandId, args: &Args) -> Result<Value, CommandError> {
        let mut context = Context {
            document: &mut self.document,
            transport: &mut self.engine,
            requests: &mut self.requests,
        };
        let result = self.commands.run_with(id, args, &mut context);
        self.sync();
        result
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
    /// [`mixer::Change::command`].
    pub fn change_mixer(&mut self, change: mixer::Change) {
        let (id, args) = change.command();
        // The mixer's controls only name tracks the project has.
        let _ = self.run_with(&id, &args);
    }

    /// The level meters of the project's tracks and of the master, as the
    /// engine last measured them.
    pub fn meters(&self) -> &Meters {
        &self.engine.meters
    }

    /// What went wrong reading the project's audio files and loading its
    /// plugins since the last call, such as a file or a plugin that's
    /// missing, for the app to show. Each file's problem comes once.
    pub fn take_problems(&mut self) -> Vec<String> {
        let mut problems = self.engine.media.take_problems();
        problems.extend(self.plugins.take_problems());
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
        self.engine.handle.collect_garbage();
        self.engine.flush();
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
                } if project == self.opened => {
                    let editor = self.document.editor_mut();
                    editor.files.extend(files);
                    take::add_recording(&mut self.document, rate, &recorded);
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

    /// Makes what changed in the plugins changes to the project, as
    /// transactions named after each plugin, so undo takes them back.
    fn follow_plugins(&mut self, changes: Vec<(DeviceId, Change)>) {
        let mut latency = false;
        for (device, change) in changes {
            let name = format!("Change {}", self.plugins.name(device).unwrap_or("Plugin"));
            match change {
                Change::GestureBegin => self.document.begin_gesture(),
                Change::GestureEnd => self.document.end_gesture(),
                Change::Param { param, value } => self.document.transact(&name, |tx| {
                    tx.set_plugin_param(device, param, Some(value));
                }),
                Change::Settings(settings) => self.document.transact(&name, |tx| {
                    tx.set_plugin_settings(device, settings);
                }),
                Change::Latency => latency = true,
                Change::Window(event) => self.window_events.push((device, event)),
                Change::Failed(problem) => self.problems.push(problem),
            }
        }
        if latency {
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
                    // Only the last map counts.
                    Edit::Tempo { to, .. } => {
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
                    Edit::MasterGain { to, .. } => param(keys::master_gain(), Gain::GAIN, to),
                    Edit::MasterPan { to, .. } => param(keys::master_gain(), Gain::PAN, to),
                    Edit::TrackMuted { .. }
                    | Edit::TrackSolo { .. }
                    | Edit::TrackSoloSafe { .. } => {
                        gates_changed = true;
                    }
                    Edit::TrackArmed { track, to, .. } => {
                        let armed = if to { 1.0 } else { 0.0 };
                        param(keys::track_input(track), InputMonitor::ARMED, armed);
                        armed_changed = true;
                    }
                    Edit::TrackMonitor { track, to, .. } => {
                        let monitor = InputMonitor::monitor_value(to) as f32;
                        param(keys::track_input(track), InputMonitor::MONITOR, monitor);
                    }
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
                    | Edit::AddNote { .. }
                    | Edit::RemoveNote { .. }
                    | Edit::NoteTiming { .. }
                    | Edit::NoteKey { .. }
                    | Edit::NoteVelocity { .. }
                    | Edit::Controller { .. }
                    | Edit::AddDevice { .. }
                    | Edit::RemoveDevice { .. } => recompile = true,
                }
            }
        }
        // Plugins load for new devices, and go with the ones that went, in
        // tracks too, and take in their settings.
        if changed && self.plugins.sync(self.document.project()) {
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
    /// The project's tempo map, for the playhead in beats.
    tempo: TempoMap,
    /// The meters the graphs measure into.
    meters: Meters,
    /// Whether each output stage and send mutes, as last sent to the engine
    /// or built into a graph. Kept for nodes that have gone as well, since
    /// the engine sends their last value again if they come back.
    gates: BTreeMap<NodeKey, bool>,
}

/// Something for the engine, in the order it was sent.
enum Message {
    Event(Event),
    Tempo(TempoMap),
    Graph(Box<PreparedGraph>),
    /// Holds back everything after it until the graph of this job, or of a
    /// newer one, comes back from the compiler, and becomes that graph.
    WaitForGraph(u64),
}

impl EngineLink {
    fn push(&mut self, message: Message) {
        self.outbox.push_back(message);
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
        self.asked = self.compiler.compile(self.handle.config(), move || {
            // Files load here, on the compiler's thread, with the graph.
            let media = media.media(&project, folder.as_deref());
            build(&project, &meters, &media, &devices, &inputs)
        });
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

    /// Sets the engine's mute gates, the output stages' and the sends', to
    /// what mute and solo say now, sending only the ones that changed. With
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
        let mut changed = Vec::new();
        for (node, muted) in outputs.chain(sends) {
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
            Message::Tempo(tempo) => self
                .handle
                .set_tempo_map(tempo)
                .map_err(|QueueFull(tempo)| Message::Tempo(tempo)),
            Message::Graph(graph) => self.handle.swap_graph(*graph).map_err(|error| match error {
                SwapError::QueueFull(graph) => Message::Graph(graph),
                SwapError::ConfigMismatch(_) => {
                    unreachable!("graphs are prepared for the engine's config")
                }
            }),
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
        self.push(Message::Event(Event::Transport(TransportCommand::Stop)));
    }

    fn locate(&mut self, at: Beats) {
        self.push(Message::Event(Event::Transport(TransportCommand::Locate(
            at,
        ))));
    }

    fn playhead(&self) -> Beats {
        self.beats_at(self.handle.stats().position)
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
