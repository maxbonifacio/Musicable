//! Finding the installed plugins, on a thread of its own. Each new or
//! changed library loads in a process of its own, Musicable's binary run as
//! `musicable scan-plugin LIBRARY`, so a plugin that crashes or hangs as it
//! loads takes only that process down. What the scans found is kept in
//! [`PLUGINS_FILE`] in the settings folder, so the app has the plugins as
//! soon as it starts, and the next scan loads only what changed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;

use musicable_plugins::scan::{self, Outcome, ScanCache};
use musicable_plugins::{PluginInfo, Waker};

/// The file in the settings folder that remembers what the scans found.
pub const PLUGINS_FILE: &str = "plugins.json";

/// The command that makes Musicable's binary scan the plugin library after
/// it: see [`scan::child`].
pub const SCAN_PLUGIN: &str = "scan-plugin";

/// The app's plugin scans.
pub(crate) struct PluginScan {
    /// Where what the scans found is kept, if Musicable has a settings
    /// folder.
    file: Option<PathBuf>,
    /// The scan running, if one is.
    running: Option<mpsc::Receiver<Scanned>>,
}

/// What a scan found.
#[derive(Debug)]
pub(crate) struct Scanned {
    /// Every plugin installed.
    pub plugins: Vec<PluginInfo>,
    /// What went wrong: the libraries this scan found bad, and why.
    pub problems: Vec<String>,
}

impl PluginScan {
    /// Scans that keep what they find in `file`.
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            file,
            running: None,
        }
    }

    /// The plugins the last scan found, if Musicable has scanned before.
    pub fn known(&self) -> Option<Vec<PluginInfo>> {
        let file = self.file.as_deref().filter(|file| file.exists())?;
        Some(ScanCache::read(file).plugins().cloned().collect())
    }

    /// Starts scanning the places plugins are installed in, unless a scan
    /// is running. Libraries that were bad are loaded again only if
    /// `retry_bad`. `wake` is called once the scan is done.
    pub fn start(&mut self, retry_bad: bool, wake: Waker) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        let program = std::env::current_exe().map_err(|e| {
            format!("Musicable can't scan for plugins, as it can't find itself: {e}")
        })?;
        let file = self.file.clone();
        let (send, receive) = mpsc::channel();
        thread::Builder::new()
            .name("plugin scan".to_owned())
            .spawn(move || {
                let scanned = run(&program, file.as_deref(), retry_bad);
                let _ = send.send(scanned);
                wake();
            })
            .map_err(|e| format!("Musicable couldn't start scanning for plugins: {e}"))?;
        self.running = Some(receive);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// What the scan found, once it's done.
    pub fn receive(&mut self) -> Option<Scanned> {
        let received = self.running.as_ref()?.try_recv();
        match received {
            Ok(scanned) => {
                self.running = None;
                Some(scanned)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.running = None;
                Some(Scanned {
                    plugins: self.known().unwrap_or_default(),
                    problems: vec!["The plugin scan stopped before it was done.".to_owned()],
                })
            }
        }
    }
}

/// Scans the plugin libraries in CLAP's and VST3's places that are new or
/// changed since the scan kept in `file`, each with `program` in a process
/// of its own, and keeps what it found there.
fn run(program: &Path, file: Option<&Path>, retry_bad: bool) -> Scanned {
    let before = file.map(ScanCache::read).unwrap_or_default();
    let mut cache = before.clone();
    let dirs: Vec<PathBuf> = scan::clap_dirs()
        .into_iter()
        .chain(scan::vst3_dirs())
        .collect();
    let libraries = scan::find_libraries(&dirs);
    let scanned = cache.update(&libraries, retry_bad, |library| {
        scan::scan_isolated(scanner(program, library), scan::TIMEOUT)
    });
    // A bad library is left out until it changes, or the user asks.
    let mut problems: Vec<String> = (cache.files.iter())
        .filter(|file| scanned.contains(&file.path))
        .filter_map(|file| match &file.outcome {
            Outcome::Found(_) => None,
            // Says which library.
            Outcome::Failed(why) => Some(format!("{why}. Rescan Plugins tries it again.")),
            Outcome::Crashed(why) => Some(format!(
                "{} is left out: {why}. Rescan Plugins tries it again.",
                file.path.display()
            )),
        })
        .collect();
    if let Some(file) = file
        && cache != before
    {
        let written = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| cache.write(file));
        if let Err(e) = written {
            problems.push(format!(
                "Couldn't save the plugins found to {}: {e}",
                file.display()
            ));
        }
    }
    Scanned {
        plugins: cache.plugins().cloned().collect(),
        problems,
    }
}

/// The process that scans `library`: `program scan-plugin library`.
fn scanner(program: &Path, library: &Path) -> Command {
    let mut command = Command::new(program);
    command.arg(SCAN_PLUGIN).arg(library);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Win32's CREATE_NO_WINDOW: no console window flashes up for it.
        command.creation_flags(0x0800_0000);
    }
    command
}
