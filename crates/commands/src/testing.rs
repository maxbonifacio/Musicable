//! What the commands' tests share: a transport that plays along and records
//! what it's asked, and a document with every command, run as the app runs
//! them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use musicable_model::clip::ClipId;
use musicable_model::plugin::{NoteTiming, PluginKind, PluginState};
use musicable_model::time::Beats;
use musicable_model::{DeviceId, DeviceKind, PluginFormat, Project, TrackId};

use crate::{
    Args, BatchError, CommandError, CommandId, Context, Document, Library, Registry, Request,
    Transport, Value, edit,
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

/// A library of the files given, which records what commands ask of it.
#[derive(Debug, Default)]
pub(crate) struct StubLibrary {
    /// Every file it has, in the order searches find them.
    pub files: Vec<PathBuf>,
    /// The files' tags.
    pub tags: BTreeSet<(PathBuf, String)>,
    /// The presets it has, and what they hold.
    pub presets: BTreeMap<PathBuf, DeviceKind>,
    /// The presets saved: the device, its name, and the preset's name.
    pub saved: Vec<(DeviceKind, String, Option<String>)>,
    /// The states the plugins of devices have now, which the project may
    /// not have yet.
    pub states: BTreeMap<DeviceId, PluginState>,
    pub calls: Vec<String>,
}

impl StubLibrary {
    fn has(&self, file: &Path) -> Result<(), String> {
        match self.files.iter().any(|f| f == file) {
            true => Ok(()),
            false => Err(format!("{} isn't there", file.display())),
        }
    }
}

impl Library for StubLibrary {
    fn search(&mut self, text: &str, limit: usize) -> Vec<PathBuf> {
        self.calls.push(format!("search {text} {limit}"));
        let text = text.to_lowercase();
        let found = self.files.iter().filter(|file| {
            let name = file.to_string_lossy().to_lowercase();
            text.split_whitespace().all(|word| name.contains(word))
        });
        found.take(limit).cloned().collect()
    }

    fn find_similar(&mut self, file: &Path, limit: usize) -> Result<Vec<PathBuf>, String> {
        self.calls
            .push(format!("similar {} {limit}", file.display()));
        self.has(file)?;
        let others = self.files.iter().filter(|f| *f != file);
        Ok(others.take(limit).cloned().collect())
    }

    fn preview(&mut self, file: &Path) -> Result<(), String> {
        self.has(file)?;
        self.calls.push(format!("preview {}", file.display()));
        Ok(())
    }

    fn stop_preview(&mut self) {
        self.calls.push("stop preview".to_owned());
    }

    fn tag(&mut self, file: &Path, tag: &str, on: bool) -> Result<bool, String> {
        self.has(file)?;
        let key = (file.to_owned(), tag.to_owned());
        Ok(match on {
            true => self.tags.insert(key),
            false => self.tags.remove(&key),
        })
    }

    fn add_folder(&mut self, folder: &Path) -> Result<(), String> {
        self.calls.push(format!("add {}", folder.display()));
        Ok(())
    }

    fn remove_folder(&mut self, folder: &Path) -> Result<(), String> {
        self.calls.push(format!("remove {}", folder.display()));
        Ok(())
    }

    fn rescan(&mut self, folder: Option<&Path>) -> Result<(), String> {
        let folder = folder.map(|f| f.to_string_lossy().into_owned());
        self.calls.push(format!("rescan {folder:?}"));
        Ok(())
    }

    fn load_preset(&mut self, preset: &Path) -> Result<DeviceKind, String> {
        let found = self.presets.get(preset).cloned();
        found.ok_or_else(|| format!("{} isn't a preset", preset.display()))
    }

    fn save_preset(
        &mut self,
        _: DeviceId,
        device: &DeviceKind,
        device_name: &str,
        name: Option<&str>,
    ) -> Result<PathBuf, String> {
        let saved = (
            device.clone(),
            device_name.to_owned(),
            name.map(str::to_owned),
        );
        self.saved.push(saved);
        let name = name.map_or_else(|| format!("{device_name} 1"), str::to_owned);
        Ok(PathBuf::from(format!(
            "/presets/{device_name}/{name}.musicable-preset"
        )))
    }

    fn plugin_state(&mut self, id: DeviceId) -> Option<PluginState> {
        self.calls.push(format!("state {}", id.0));
        self.states.get(&id).cloned()
    }

    fn set_plugin_kind(
        &mut self,
        format: PluginFormat,
        id: &str,
        kind: PluginKind,
    ) -> Result<(), String> {
        if id.is_empty() {
            return Err("there's no plugin with no id".to_owned());
        }
        let kind = kind.key();
        self.calls.push(format!("kind {format:?} {id} {kind}"));
        Ok(())
    }

    fn set_plugin_render_ahead(
        &mut self,
        format: PluginFormat,
        id: &str,
        never: bool,
    ) -> Result<(), String> {
        let render_ahead = if never { "never" } else { "auto" };
        (self.calls).push(format!("render ahead {format:?} {id} {render_ahead}"));
        Ok(())
    }

    fn set_plugin_note_timing(
        &mut self,
        format: PluginFormat,
        id: &str,
        timing: Option<NoteTiming>,
    ) -> Result<(), String> {
        let timing = timing.map_or("auto", NoteTiming::key);
        (self.calls).push(format!("note timing {format:?} {id} {timing}"));
        Ok(())
    }
}

/// A document with every command, and what running them asks for.
pub(crate) struct Setup {
    pub registry: Registry,
    pub document: Document,
    pub transport: StubTransport,
    pub requests: Vec<Request>,
    pub library: StubLibrary,
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
            library: StubLibrary::default(),
        }
    }

    /// Runs `id` without arguments, as a key does: if it doesn't apply, it
    /// does nothing.
    pub fn run(&mut self, id: &CommandId) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
            library: &mut self.library,
        };
        self.registry.run(id, &mut ctx).unwrap();
    }

    pub fn run_with(&mut self, id: &CommandId, args: Args) -> Result<Value, CommandError> {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
            library: &mut self.library,
        };
        self.registry.run_with(id, &args, &mut ctx)
    }

    /// Runs `commands` as one step called `name`: see
    /// [`Registry::run_batch`].
    pub fn run_batch(
        &mut self,
        name: &str,
        commands: &[(CommandId, Args)],
    ) -> Result<Vec<Value>, BatchError> {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
            library: &mut self.library,
        };
        self.registry.run_batch(name, commands, &mut ctx)
    }

    pub fn run_linked(
        &mut self,
        name: &str,
        commands: &[crate::Linked],
    ) -> Result<Vec<Value>, BatchError> {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.transport,
            requests: &mut self.requests,
            library: &mut self.library,
        };
        self.registry.run_linked(name, commands, &mut ctx)
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

    /// Selects `devices` in the devices panel, which gives it the focus.
    pub fn select_devices(&mut self, devices: &[DeviceId]) {
        let args = Args::new().with("devices", devices.to_vec());
        self.run_with(&crate::device::SELECT, args).unwrap();
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
