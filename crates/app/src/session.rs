//! The open project, wired to the engine.

use std::collections::VecDeque;

use musicable_commands::{
    CommandId, Context, Document, Edit, KeyChord, Keymap, Registry, Request, Transport,
    UnknownCommand, transport,
};
use musicable_engine::compile::keys;
use musicable_engine::nodes::Metronome;
use musicable_engine::{
    EngineConfig, EngineHandle, Event, PreparedGraph, QueueFull, Stats, SwapError,
    TransportCommand, compile,
};
use musicable_model::Project;
use musicable_model::time::{Beats, TempoMap};

/// The open project, the commands that change it, and the engine that plays
/// it.
///
/// Key presses and menus run commands. Commands change the project through
/// transactions, and after each command the session sends the engine what
/// changed, so the engine plays the project as it is. `docs/commands.md`
/// describes the flow.
pub struct Session {
    document: Document,
    commands: Registry,
    keymap: Keymap,
    engine: EngineLink,
    /// What commands asked of the app, oldest first.
    requests: Vec<Request>,
}

impl Session {
    /// Opens `project` on the engine `handle` drives: swaps in the project's
    /// graph and sends its tempo map and loop. Key presses go through
    /// `keymap`.
    pub fn new(project: Project, keymap: Keymap, handle: EngineHandle) -> Self {
        let mut engine = EngineLink {
            playing: handle.stats().playing,
            handle,
            outbox: VecDeque::new(),
        };
        engine.set_up(&project);
        engine.flush();
        Self {
            document: Document::new(project),
            commands: crate::commands(),
            keymap,
            engine,
            requests: Vec::new(),
        }
    }

    /// Opens `project` in place of the open one. The transport stops, and the
    /// playhead goes to the start of the song.
    pub fn open(&mut self, project: Project) {
        self.engine.stop();
        self.engine.locate(Beats::ZERO);
        self.engine.set_up(&project);
        self.engine.flush();
        self.document = Document::new(project);
    }

    /// Plays the project on the engine `handle` drives from now on, such as
    /// one built for another audio device or buffer size, and hands back the
    /// old engine's handle.
    ///
    /// The new engine takes over where the old one was: its playhead goes to
    /// the same place in bars and beats, and it plays if the old one did.
    /// What the old engine hadn't received yet, it never will.
    pub fn replace_engine(&mut self, handle: EngineHandle) -> EngineHandle {
        let at = self.playhead();
        let playing = self.engine.playing;
        let old = std::mem::replace(&mut self.engine.handle, handle);
        self.engine.outbox.clear();
        self.engine.playing = false;
        self.engine.set_up(self.document.project());
        self.engine.locate(at);
        if playing {
            self.engine.play();
        }
        self.engine.flush();
        old
    }

    pub fn project(&self) -> &Project {
        self.document.project()
    }

    /// Whether the project changed since it was opened or last saved.
    pub fn is_modified(&self) -> bool {
        self.document.is_modified()
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.document.mark_saved();
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
        let position = self.stats().position as f64;
        let rate = self.engine.handle.config().sample_rate;
        Beats::from_f64(self.project().tempo.beats_at(position, rate))
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

    /// Runs the command `id`, then sends the engine what it changed.
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

    /// What the commands run so far asked the app to do, such as opening a
    /// file, oldest first. The app carries them out after each command.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// Sets the tempo at the playhead to `bpm`, moving every other tempo in
    /// the song by as much, then sends the engine the new tempo map. For a
    /// tempo field: see [`transport::set_tempo`].
    pub fn set_tempo(&mut self, bpm: f64) {
        let at = self.playhead();
        transport::set_tempo(&mut self.document, at, bpm);
        self.sync();
    }

    /// Call regularly, such as once per UI frame. Sends what the engine had no
    /// room for earlier, and drops the graphs and tempo maps it has replaced.
    pub fn tick(&mut self) {
        self.engine.handle.collect_garbage();
        self.engine.flush();
    }

    /// Sends the engine what the committed transactions changed.
    fn sync(&mut self) {
        let mut loop_changed = false;
        for committed in self.document.take_committed() {
            for edit in committed.edits {
                match edit {
                    Edit::Tempo { to, .. } => self.engine.push(Message::Tempo(to)),
                    Edit::LoopRegion { .. } | Edit::Looping { .. } => loop_changed = true,
                    Edit::Metronome { to, .. } => self.engine.push(Message::Event(Event::Param {
                        node: keys::METRONOME,
                        param: Metronome::ENABLED,
                        value: if to { 1.0 } else { 0.0 },
                    })),
                }
            }
        }
        // One message, with the loop as it ends up, covers every change to it.
        // It can go after the tempo maps: the engine keeps the loop in beats.
        if loop_changed {
            let event = loop_event(self.document.project());
            self.engine.push(Message::Event(event));
        }
        self.engine.flush();
    }
}

/// Sets the engine's loop to the project's.
fn loop_event(project: &Project) -> Event {
    let region = project.looping.then_some(project.loop_region);
    Event::Transport(TransportCommand::Loop(region))
}

/// The engine, as commands see it.
struct EngineLink {
    handle: EngineHandle,
    /// What the engine's queue had no room for yet, oldest first.
    outbox: VecDeque<Message>,
    /// Whether the transport plays, counting what hasn't reached the engine
    /// yet.
    playing: bool,
}

/// Something for the engine, in the order it was sent.
enum Message {
    Event(Event),
    Tempo(TempoMap),
    Graph(Box<PreparedGraph>),
}

impl EngineLink {
    fn push(&mut self, message: Message) {
        self.outbox.push_back(message);
    }

    /// Sends what the engine needs to play `project`: its graph, its tempo
    /// map and its loop.
    fn set_up(&mut self, project: &Project) {
        let graph = compile(project)
            .prepare(&self.handle.config())
            .expect("compiled graphs are valid");
        self.push(Message::Graph(Box::new(graph)));
        self.push(Message::Tempo(project.tempo.clone()));
        self.push(Message::Event(loop_event(project)));
    }

    /// Sends what's waiting, oldest first, until the engine's queue is full.
    fn flush(&mut self) {
        while let Some(message) = self.outbox.pop_front() {
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
        self.push(Message::Event(Event::Transport(TransportCommand::Stop)));
    }

    fn locate(&mut self, at: Beats) {
        self.push(Message::Event(Event::Transport(TransportCommand::Locate(
            at,
        ))));
    }
}
