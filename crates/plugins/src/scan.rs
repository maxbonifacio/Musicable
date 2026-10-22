//! Finding plugins: scanning their libraries in a process of their own,
//! and remembering what was found.
//!
//! Loading a plugin's library runs its code, and some plugins crash or hang
//! as they load. So the app scans each library in a scanning process
//! ([`scan_isolated`]), which runs [`child`] and reports what it found on
//! its standard output. A library that crashes or hangs takes only that
//! process down. A [`ScanCache`] remembers every library scanned, good or
//! bad, so the next scan only looks at new and changed files, and doesn't
//! load a bad library again until it changes.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use musicable_model::PluginFormat;
use serde::{Deserialize, Serialize};

use crate::clap::Library;
use crate::vst3::Module;
use crate::{Error, PluginInfo};

/// How long a library gets to load before it counts as hung.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// What the scanning process prints before its result, at the start of a
/// line, so whatever plugins print as they load can't be taken for it.
const RESULT: &str = "musicable-scan-result ";

/// What scanning a library found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It loaded, and holds these plugins.
    Found(Vec<PluginInfo>),
    /// It loaded but doesn't work, as when it fails to initialize.
    Failed(String),
    /// Loading it crashed the scanning process, or never finished.
    Crashed(String),
}

impl Outcome {
    /// Whether the library is one to stay away from.
    pub fn is_bad(&self) -> bool {
        !matches!(self, Self::Found(_))
    }
}

/// The format of the plugin library at `path`, by its extension.
pub fn format_of(path: &Path) -> Option<PluginFormat> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "clap" => Some(PluginFormat::Clap),
        "vst3" => Some(PluginFormat::Vst3),
        _ => None,
    }
}

/// Loads the library at `path` in this process, and lists its plugins.
/// This is what the scanning process does; the app itself calls
/// [`scan_isolated`].
pub fn scan_here(path: &Path) -> Result<Vec<PluginInfo>, Error> {
    match format_of(path) {
        Some(PluginFormat::Clap) => Ok(Library::load(path)?.plugins()),
        Some(PluginFormat::Vst3) => Ok(Module::load(path)?.plugins()),
        None => Err(Error::new(format!(
            "{} isn't a plugin library",
            path.display()
        ))),
    }
}

/// The scanning process's work: scans the library at `path` and prints
/// what it found, for [`scan_isolated`] to read.
pub fn child(path: &Path) {
    let outcome = match scan_here(path) {
        Ok(plugins) => Outcome::Found(plugins),
        Err(e) => Outcome::Failed(e.to_string()),
    };
    let json = serde_json::to_string(&outcome).unwrap_or_default();
    // Straight to the file: unlike `println!`, which a test harness can
    // capture.
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "\n{RESULT}{json}");
    let _ = stdout.flush();
}

/// Scans a library in a process of its own: `command` runs the scanning
/// process, which calls [`child`] for the library. Gives up on it after
/// `timeout`.
pub fn scan_isolated(mut command: Command, timeout: Duration) -> Outcome {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = match command.spawn() {
        Ok(process) => process,
        Err(e) => return Outcome::Crashed(format!("the scanner didn't start: {e}")),
    };
    let (send, output) = mpsc::channel();
    if let Some(mut stdout) = process.stdout.take() {
        thread::spawn(move || {
            let mut out = Vec::new();
            let _ = stdout.read_to_end(&mut out);
            let _ = send.send(out);
        });
    }
    let deadline = Instant::now() + timeout;
    let status = loop {
        match process.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = process.kill();
                let _ = process.wait();
                break None;
            }
        }
    };
    // A process the plugin started can hold the output open: don't wait
    // for it long.
    let out = output
        .recv_timeout(Duration::from_secs(2))
        .unwrap_or_default();
    outcome(&out, status, timeout)
}

/// What the scanning process's output and exit say.
fn outcome(out: &[u8], status: Option<ExitStatus>, timeout: Duration) -> Outcome {
    let found = String::from_utf8_lossy(out)
        .lines()
        .find_map(|line| line.strip_prefix(RESULT).map(str::to_owned))
        .and_then(|json| serde_json::from_str::<Outcome>(&json).ok());
    match (found, status) {
        (Some(outcome), _) => outcome,
        (None, Some(status)) => Outcome::Crashed(format!("it crashed the scanner ({status})")),
        (None, None) => Outcome::Crashed(format!(
            "it was still loading after {} seconds",
            timeout.as_secs()
        )),
    }
}

/// Where this platform's CLAP plugins are installed, as CLAP says: the
/// directories in `CLAP_PATH` first.
pub fn clap_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("CLAP_PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    if cfg!(windows) {
        dirs.extend(env("COMMONPROGRAMFILES").map(|d| d.join("CLAP")));
        dirs.extend(env("LOCALAPPDATA").map(|d| d.join("Programs").join("Common").join("CLAP")));
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        dirs.extend(env("HOME").map(|d| d.join("Library/Audio/Plug-Ins/CLAP")));
    } else {
        dirs.extend(env("HOME").map(|d| d.join(".clap")));
        dirs.push(PathBuf::from("/usr/lib/clap"));
    }
    dirs
}

/// Where this platform's VST3 plugins are installed, as VST3 says: the
/// directories in `VST3_PATH` first, which Musicable reads as it reads
/// `CLAP_PATH`.
pub fn vst3_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("VST3_PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    if cfg!(windows) {
        dirs.extend(env("COMMONPROGRAMFILES").map(|d| d.join("VST3")));
        dirs.extend(env("LOCALAPPDATA").map(|d| d.join("Programs").join("Common").join("VST3")));
    } else if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        dirs.extend(env("HOME").map(|d| d.join("Library/Audio/Plug-Ins/VST3")));
    } else {
        dirs.extend(env("HOME").map(|d| d.join(".vst3")));
        dirs.push(PathBuf::from("/usr/lib/vst3"));
        dirs.push(PathBuf::from("/usr/local/lib/vst3"));
    }
    dirs
}

/// The plugin libraries in `dirs` and the directories below them, sorted.
/// A bundle, such as a VST3 one, is a library, not a directory to look in.
pub fn find_libraries(dirs: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, found: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if format_of(&path).is_some() {
                found.push(path);
            } else if depth > 0 && path.is_dir() {
                walk(&path, depth - 1, found);
            }
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        walk(dir, 8, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

/// What scanning found, library by library, kept from one run to the next.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    /// Sorted by path.
    pub files: Vec<ScannedFile>,
}

/// One library, as it was when it was scanned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedFile {
    pub path: PathBuf,
    /// When the library last changed, in nanoseconds since 1970, and its
    /// size in bytes: if either changes, it's scanned again.
    pub modified: u64,
    pub size: u64,
    pub outcome: Outcome,
}

impl ScanCache {
    /// The cache saved at `path`, or an empty one if there's none or it
    /// can't be read.
    pub fn read(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Saves the cache to `path`, all or nothing.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let temporary = path.with_extension("json.new");
        fs::write(&temporary, json)?;
        fs::rename(&temporary, path)
    }

    /// Every plugin found, library by library.
    pub fn plugins(&self) -> impl Iterator<Item = &PluginInfo> {
        self.files.iter().flat_map(|file| match &file.outcome {
            Outcome::Found(plugins) => plugins.as_slice(),
            _ => &[],
        })
    }

    /// The plugin a project names, if it's installed.
    pub fn find(&self, format: PluginFormat, id: &str) -> Option<&PluginInfo> {
        self.plugins().find(|p| p.format == format && p.id == id)
    }

    /// The libraries that crashed, hung or failed.
    pub fn bad(&self) -> impl Iterator<Item = &ScannedFile> {
        self.files.iter().filter(|file| file.outcome.is_bad())
    }

    /// Brings the cache up to date with `libraries`, the ones installed now:
    /// scans the new ones and the ones that changed with `scan`, and
    /// forgets the ones gone. Bad libraries are scanned again only if
    /// `retry_bad`. Says which libraries were scanned.
    pub fn update(
        &mut self,
        libraries: &[PathBuf],
        retry_bad: bool,
        mut scan: impl FnMut(&Path) -> Outcome,
    ) -> Vec<PathBuf> {
        let mut scanned = Vec::new();
        let mut files = Vec::with_capacity(libraries.len());
        for path in libraries {
            let (modified, size) = fingerprint(path);
            let known = self.files.iter().find(|f| f.path == *path).filter(|f| {
                f.modified == modified && f.size == size && !(retry_bad && f.outcome.is_bad())
            });
            match known {
                Some(file) => files.push(file.clone()),
                None => {
                    files.push(ScannedFile {
                        path: path.clone(),
                        modified,
                        size,
                        outcome: scan(path),
                    });
                    scanned.push(path.clone());
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        self.files = files;
        scanned
    }
}

/// When a library last changed, and how big it is. A bundle changes when
/// its binary does, so the newest file in it counts.
fn fingerprint(path: &Path) -> (u64, u64) {
    let since_1970 = |time: SystemTime| {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos().min(u64::MAX.into()) as u64)
    };
    let Ok(metadata) = fs::metadata(path) else {
        return (0, 0);
    };
    if !metadata.is_dir() {
        return (metadata.modified().map_or(0, since_1970), metadata.len());
    }
    let mut newest = metadata.modified().map_or(0, since_1970);
    let mut size = 0;
    let mut dirs = vec![path.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                dirs.push(entry.path());
            } else {
                newest = newest.max(metadata.modified().map_or(0, since_1970));
                size += metadata.len();
            }
        }
    }
    (newest, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraries_are_found_by_extension() {
        let dir = crate::tests::temp_dir("find");
        fs::create_dir_all(dir.join("Vendor/Sub")).unwrap();
        fs::create_dir_all(dir.join("Bundle.clap/Contents/MacOS")).unwrap();
        for file in ["a.clap", "Vendor/b.CLAP", "Vendor/Sub/c.vst3", "notes.txt"] {
            fs::write(dir.join(file), b"").unwrap();
        }
        let found = find_libraries(std::slice::from_ref(&dir));
        let names: Vec<_> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(
            names,
            [
                "Bundle.clap",
                "Vendor/Sub/c.vst3",
                "Vendor/b.CLAP",
                "a.clap"
            ]
        );
    }

    #[test]
    fn output_is_read_past_what_plugins_print() {
        let found = Outcome::Found(Vec::new());
        let out = format!(
            "plugin says hello\n{RESULT}{}\nbye\n",
            serde_json::to_string(&found).unwrap()
        );
        assert_eq!(outcome(out.as_bytes(), None, TIMEOUT), found);
        assert!(matches!(
            outcome(b"loading...\n", None, TIMEOUT),
            Outcome::Crashed(reason) if reason.contains("still loading")
        ));
    }

    #[test]
    fn the_cache_scans_only_new_and_changed_libraries() {
        let dir = crate::tests::temp_dir("cache");
        let (a, b) = (dir.join("a.clap"), dir.join("b.clap"));
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let mut cache = ScanCache::default();
        let bad = |path: &Path| {
            if path == b {
                Outcome::Crashed("it crashed".into())
            } else {
                Outcome::Found(Vec::new())
            }
        };
        let libraries = [a.clone(), b.clone()];
        assert_eq!(cache.update(&libraries, false, bad), libraries);
        assert_eq!(cache.update(&libraries, false, bad), Vec::<PathBuf>::new());
        assert_eq!(cache.bad().count(), 1);
        assert_eq!(cache.update(&libraries, true, bad), libraries[1..]);
        // A changed library is scanned again; one that's gone is forgotten.
        fs::write(&a, b"a, longer").unwrap();
        assert_eq!(cache.update(&libraries[..1], false, bad), libraries[..1]);
        assert_eq!(cache.files.len(), 1);
        let saved = dir.join("plugins.json");
        cache.write(&saved).unwrap();
        assert_eq!(ScanCache::read(&saved), cache);
        assert_eq!(
            ScanCache::read(&dir.join("none.json")),
            ScanCache::default()
        );
    }
}
