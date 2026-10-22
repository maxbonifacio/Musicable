//! What the commands' tests share: a transport that plays along and records
//! what it's asked, and a document with every command, run as the app runs
//! them.

use musicable_model::clip::ClipId;
use musicable_model::time::Beats;
use musicable_model::{Project, TrackId};

use crate::{
    Args, CommandError, CommandId, Context, Document, Registry, Request, Transport, Value, edit,
};

/// Records what commands ask of the transport, and plays along: locating
/// moves its playhead.
#[derive(Debug, Default)]
pub(crate) struct StubTransport {
    pub playing: bool,
    pub recording: bool,
    pub playhead: Beats,
    pub calls: Vec<String>,
}

impl Transport for StubTransport {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
        self.calls.push("play".into());
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
        self.calls.push("stop".into());
    }

    fn locate(&mut self, at: Beats) {
        self.playhead = at;
        self.calls.push(format!("locate {}", at.to_f64()));
    }

    fn playhead(&self) -> Beats {
        self.playhead
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
        self.calls.push(format!("record {on}"));
    }
}

/// A document with every command, and what running them asks for.
pub(crate) struct Setup {
    pub registry: Registry,
    pub document: Document,
    pub transport: StubTransport,
    pub requests: Vec<Request>,
}

impl Setup {
    pub fn new(project: Project) -> Self {
        let mut registry = Registry::new();
        crate::register_all(&mut registry);
        Self {
            registry,
            document: Document::new(project),
            transport: StubTransport::default(),
            requests: Vec::new(),
        }
    }

    /// Runs `id` without arguments, as a key does: if it doesn't apply, it
    /// does nothing.
    pub fn run(&mut self, id: &CommandId) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
        };
        self.registry.run(id, &mut ctx).unwrap();
    }

    pub fn run_with(&mut self, id: &CommandId, args: Args) -> Result<Value, CommandError> {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
        };
        self.registry.run_with(id, &args, &mut ctx)
    }

    /// Whether `id` applies without arguments.
    pub fn applies(&self, id: &CommandId) -> bool {
        self.registry.applies(id, &self.document)
    }

    pub fn select_clips(&mut self, clips: &[ClipId]) {
        let args = Args::new().with("clips", clips.to_vec());
        self.run_with(&edit::SELECT_CLIPS, args).unwrap();
    }

    pub fn select_tracks(&mut self, tracks: &[TrackId]) {
        let args = Args::new().with("tracks", tracks.to_vec());
        self.run_with(&edit::SELECT_TRACKS, args).unwrap();
    }

    /// Selects from beat `start` to beat `end` on `tracks`.
    pub fn select_time(&mut self, start: i64, end: i64, tracks: &[TrackId]) {
        let args = Args::new()
            .with("start", Beats::new(start))
            .with("end", Beats::new(end))
            .with("tracks", tracks.to_vec());
        self.run_with(&edit::SELECT_TIME, args).unwrap();
    }

    /// The names of the transactions committed since the last call.
    pub fn committed_names(&mut self) -> Vec<String> {
        let committed = self.document.take_committed();
        committed.into_iter().map(|c| c.name).collect()
    }
}
