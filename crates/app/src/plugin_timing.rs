//! Measuring when the notes plugins with a latency play come, on a thread
//! of its own: see [`musicable_plugins::timing`]. Each plugin plays in a
//! process of its own, Musicable's binary run as `musicable
//! time-plugin-notes FORMAT LIBRARY ID KIND`, so a plugin that crashes or
//! hangs takes only that process down. What it found is kept in
//! [`TIMINGS_FILE`] in the settings folder, by plugin and the library it's
//! in as it was, so a plugin is measured once, and again once its library
//! changes.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

use musicable_model::PluginFormat;
use musicable_model::plugin::{NoteTiming, PluginKind};
use musicable_plugins::{PluginInfo, Waker, scan, timing};
use serde::{Deserialize, Serialize};

/// The file in the settings folder that keeps what was measured.
pub const TIMINGS_FILE: &str = "note-timings.json";

/// The command that makes Musicable's binary measure a plugin: see
/// [`timing::child`].
pub const TIME_PLUGIN_NOTES: &str = "time-plugin-notes";

/// A plugin, by format and id.
pub(crate) type PluginKey = (PluginFormat, String);

/// What was measured of one plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// `"clap"` or `"vst3"`.
    format: String,
    id: String,
    /// The library it was in, and its fingerprint then
    /// ([`scan::fingerprint`]): once it changes, the plugin is measured
    /// again.
    path: PathBuf,
    modified: u64,
    size: u64,
    /// What it was taken to do with notes as it played, as a plugin
    /// kind's key: once it's marked as another kind, it's measured again,
    /// as that kind is played another thing. An entry without one is
    /// measured again too.
    #[serde(default)]
    kind: Option<String>,
    /// `"with_sound"`, `"as_played"`, or `None` where playing it said
    /// nothing.
    timing: Option<String>,
}

/// The file's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    plugins: Vec<Entry>,
}

/// What measuring a plugin said: its timing, if playing it said, or why it
/// didn't load or play.
type Measured = Result<Option<NoteTiming>, String>;

/// The app's measurements of plugins' note timing.
pub(crate) struct PluginTimings {
    /// Where they're kept, if Musicable has a settings folder.
    file: Option<PathBuf>,
    kept: Kept,
    /// The plugins waiting to be measured, with what they do with notes.
    waiting: VecDeque<(PluginInfo, PluginKind)>,
    /// The measurement running, if one is: of which plugin, and what it
    /// will say.
    running: Option<(PluginInfo, PluginKind, mpsc::Receiver<Measured>)>,
}

impl PluginTimings {
    /// Measurements kept in `file`, with those it has already.
    pub fn new(file: Option<PathBuf>) -> Self {
        let kept = (file.as_deref())
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            file,
            kept,
            waiting: VecDeque::new(),
            running: None,
        }
    }

    /// What was measured of each plugin whose library is as it was then,
    /// with what it was taken to do with notes as it played: `None` for one
    /// that playing said nothing of.
    pub fn known(&self) -> BTreeMap<PluginKey, (PluginKind, Option<NoteTiming>)> {
        (self.kept.plugins.iter())
            .filter(|entry| scan::fingerprint(&entry.path) == (entry.modified, entry.size))
            .filter_map(|entry| {
                let format = musicable_plugins::format_name::parse(&entry.format)?;
                let kind = PluginKind::from_key(entry.kind.as_deref()?)?;
                let timing = entry.timing.as_deref().and_then(NoteTiming::from_key);
                Some(((format, entry.id.clone()), (kind, timing)))
            })
            .collect()
    }

    /// Measures `plugin`, which does `kind` with notes, once the ones
    /// waiting are done, unless it's waiting or running already.
    pub fn request(&mut self, plugin: PluginInfo, kind: PluginKind) {
        let same = |other: &PluginInfo| other.format == plugin.format && other.id == plugin.id;
        let running = self
            .running
            .as_ref()
            .is_some_and(|(p, of, _)| same(p) && *of == kind);
        if !running && !self.waiting.iter().any(|(p, of)| same(p) && *of == kind) {
            self.waiting.push_back((plugin, kind));
        }
    }

    /// Starts measuring the next plugin waiting, if none is being measured.
    /// `wake` is called once it's done.
    pub fn start(&mut self, wake: Waker) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        let Some((plugin, kind)) = self.waiting.pop_front() else {
            return Ok(());
        };
        let program = std::env::current_exe().map_err(|e| {
            format!("Musicable can't measure plugins, as it can't find itself: {e}")
        })?;
        let (send, receive) = mpsc::channel();
        let command = measurer(&program, &plugin, kind);
        thread::Builder::new()
            .name("plugin note timing".to_owned())
            .spawn(move || {
                let _ = send.send(timing::measure_isolated(command, timing::TIMEOUT));
                wake();
            })
            .map_err(|e| format!("Musicable couldn't start measuring a plugin: {e}"))?;
        self.running = Some((plugin, kind, receive));
        Ok(())
    }

    /// What the measurement running found, once it's done: the plugin, what
    /// it was taken to do with notes, and its timing, if playing it said,
    /// which is kept from now on. A plugin that didn't load, crashed or hung
    /// counts as saying nothing.
    pub fn receive(&mut self) -> Option<(PluginKey, PluginKind, Option<NoteTiming>)> {
        let (plugin, _, receive) = self.running.as_ref()?;
        let timing = match receive.try_recv() {
            Ok(Ok(timing)) => timing,
            Ok(Err(why)) => {
                log::info!("{} didn't say when its notes come: {why}", plugin.name);
                None
            }
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => None,
        };
        let (plugin, kind, _) = self.running.take()?;
        self.keep(&plugin, kind, timing);
        Some(((plugin.format, plugin.id), kind, timing))
    }

    /// Keeps what was measured of `plugin`, and saves it.
    fn keep(&mut self, plugin: &PluginInfo, kind: PluginKind, timing: Option<NoteTiming>) {
        let (modified, size) = scan::fingerprint(&plugin.path);
        let format = musicable_plugins::format_name::name(plugin.format).to_owned();
        let entries = &mut self.kept.plugins;
        entries.retain(|entry| !(entry.format == format && entry.id == plugin.id));
        entries.push(Entry {
            format,
            id: plugin.id.clone(),
            path: plugin.path.clone(),
            modified,
            size,
            kind: Some(kind.key().to_owned()),
            timing: timing.map(|timing| timing.key().to_owned()),
        });
        if let Some(file) = &self.file
            && let Err(e) = write(file, &self.kept)
        {
            log::warn!(
                "Couldn't save the plugins' note timings to {}: {e}",
                file.display()
            );
        }
    }
}

/// Saves `kept` to `file`, all or nothing.
fn write(file: &Path, kept: &Kept) -> std::io::Result<()> {
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let json = serde_json::to_vec_pretty(kept).map_err(std::io::Error::other)?;
    let temporary = file.with_extension("json.new");
    std::fs::write(&temporary, json)?;
    std::fs::rename(&temporary, file)
}

/// The process that measures `plugin`, which does `kind` with notes:
/// `program time-plugin-notes FORMAT LIBRARY ID KIND`.
fn measurer(program: &Path, plugin: &PluginInfo, kind: PluginKind) -> Command {
    let mut command = Command::new(program);
    command
        .arg(TIME_PLUGIN_NOTES)
        .arg(musicable_plugins::format_name::name(plugin.format))
        .arg(&plugin.path)
        .arg(&plugin.id)
        .arg(kind.key());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Win32's CREATE_NO_WINDOW: no console window flashes up for it.
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_measured_is_kept_while_the_library_is_as_it_was() {
        let dir = std::env::temp_dir().join(format!("musicable-timings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let library = dir.join("arp.clap");
        std::fs::write(&library, b"a library").unwrap();
        let file = dir.join(TIMINGS_FILE);
        let plugin = PluginInfo {
            format: PluginFormat::Clap,
            id: "org.example.arp".into(),
            name: "Arp".into(),
            vendor: String::new(),
            version: String::new(),
            description: String::new(),
            features: Vec::new(),
            buses: None,
            path: library.clone(),
        };
        let mut timings = PluginTimings::new(Some(file.clone()));
        timings.keep(&plugin, PluginKind::MidiEffect, Some(NoteTiming::WithSound));
        let key = (PluginFormat::Clap, "org.example.arp".to_owned());
        let midi_effect = (PluginKind::MidiEffect, Some(NoteTiming::WithSound));
        let measured = BTreeMap::from([(key.clone(), midi_effect)]);
        assert_eq!(PluginTimings::new(Some(file.clone())).known(), measured);
        // Measured again once its library changes.
        std::fs::write(&library, b"a new version of the library").unwrap();
        assert!(PluginTimings::new(Some(file)).known().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
