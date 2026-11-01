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
//!
//! A VST3 library's plugins are made too, to count their buses, which
//! tells its MIDI effects ([`PluginInfo::kind`]). That runs more of their
//! code, so the scanning process reports what the library's factory said
//! first: if making the plugins crashes or hangs, that stands.

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
    scan(path, |_| ())
}

/// Scans the library at `path`, giving `found` what a VST3 library's
/// factory says before its plugins are made to count their buses.
fn scan(path: &Path, found: impl FnOnce(&[PluginInfo])) -> Result<Vec<PluginInfo>, Error> {
    match format_of(path) {
        Some(PluginFormat::Clap) => Ok(Library::load(path)?.plugins()),
        Some(PluginFormat::Vst3) => {
            let module = Module::load(path)?;
            let mut plugins = module.plugins();
            found(&plugins);
            module.probe(&mut plugins);
            Ok(plugins)
        }
        None => Err(Error::new(format!(
            "{} isn't a plugin library",
            path.display()
        ))),
    }
}

/// The scanning process's work: scans the library at `path` and prints
/// what it found, for [`scan_isolated`] to read. For a VST3 library, it
/// prints what the factory says first, and again once it has counted the
/// plugins' buses: the last result printed stands.
pub fn child(path: &Path) {
    let outcome = match scan(path, |plugins| print(&Outcome::Found(plugins.to_vec()))) {
        Ok(plugins) => Outcome::Found(plugins),
        Err(e) => Outcome::Failed(e.to_string()),
    };
    print(&outcome);
}

/// Prints a result for [`scan_isolated`] to read.
fn print(outcome: &Outcome) {
    print_result(outcome);
}

/// Scans a library in a process of its own: `command` runs the scanning
/// process, which calls [`child`] for the library. Gives up on it after
/// `timeout`.
pub fn scan_isolated(command: Command, timeout: Duration) -> Outcome {
    match run_isolated(command, timeout) {
        Ok((out, status)) => outcome(&out, status, timeout),
        Err(e) => Outcome::Crashed(format!("the scanner didn't start: {e}")),
    }
}

/// Runs `command`, a scanning process, and gives back what it printed and
/// how it exited, or `None` if it didn't within `timeout`, when it's
/// stopped.
pub(crate) fn run_isolated(
    mut command: Command,
    timeout: Duration,
) -> io::Result<(Vec<u8>, Option<ExitStatus>)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = command.spawn()?;
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
    Ok((out, status))
}

/// The last result a scanning process printed whole: what follows
/// [`RESULT`] on a line of `out`.
pub(crate) fn last_result<T: serde::de::DeserializeOwned>(out: &[u8]) -> Option<T> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|line| line.strip_prefix(RESULT))
        .filter_map(|json| serde_json::from_str::<T>(json).ok())
        .next_back()
}

/// Prints `result` for the app's scanning process to read, as
/// [`last_result`] does.
pub(crate) fn print_result(result: &impl Serialize) {
    let json = serde_json::to_string(result).unwrap_or_default();
    // Straight to the file: unlike `println!`, which a test harness can
    // capture.
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "\n{RESULT}{json}");
    let _ = stdout.flush();
}

/// What the scanning process's output and exit say: the last result it
/// printed whole.
fn outcome(out: &[u8], status: Option<ExitStatus>, timeout: Duration) -> Outcome {
    match (last_result(out), status) {
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
            if is_library(&path) {
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

/// Whether `path` is a plugin library, not a directory to look in. A
/// `.vst3` bundle is one everywhere, and a `.clap` one on macOS. Elsewhere
/// CLAP's plugins are files, so a directory named `.clap`, as Debian
/// installs some, is searched like any other, as CLAP asks.
fn is_library(path: &Path) -> bool {
    match format_of(path) {
        Some(PluginFormat::Clap) => cfg!(target_os = "macos") || !path.is_dir(),
        Some(PluginFormat::Vst3) => true,
        None => false,
    }
}

/// What scanning found, library by library, kept from one run to the next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    /// The scanner's [`VERSION`](Self::VERSION) that found it.
    #[serde(default)]
    pub version: u32,
    /// Sorted by path.
    pub files: Vec<ScannedFile>,
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
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
    /// Goes up when the scanner finds more than it did, so every library is
    /// scanned again: 1 counts VST3 plugins' buses.
    pub const VERSION: u32 = 1;

    /// An empty cache, for this version of the scanner.
    pub fn new() -> Self {
        Self {
            version: Self::VERSION,
            files: Vec::new(),
        }
    }

    /// The cache saved at `path`, or an empty one if there's none, it
    /// can't be read, or an older scanner found it.
    pub fn read(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|cache| cache.version == Self::VERSION)
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
pub fn fingerprint(path: &Path) -> (u64, u64) {
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
        fs::create_dir_all(dir.join("Bundle.vst3/Contents/x86_64-linux")).unwrap();
        fs::create_dir_all(dir.join("Folder.clap")).unwrap();
        for file in [
            "a.clap",
            "Vendor/b.CLAP",
            "Vendor/Sub/c.vst3",
            "notes.txt",
            "Bundle.vst3/Contents/x86_64-linux/d.so",
            "Folder.clap/e.clap",
        ] {
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
        // A `.clap` directory is a bundle only on macOS. Elsewhere it's
        // searched, as Debian installs some plugins in one.
        let expected: &[&str] = if cfg!(target_os = "macos") {
            &[
                "Bundle.clap",
                "Bundle.vst3",
                "Folder.clap",
                "Vendor/Sub/c.vst3",
                "Vendor/b.CLAP",
                "a.clap",
            ]
        } else {
            &[
                "Bundle.vst3",
                "Folder.clap/e.clap",
                "Vendor/Sub/c.vst3",
                "Vendor/b.CLAP",
                "a.clap",
            ]
        };
        assert_eq!(names, expected);
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
        // The last result whole stands: a second one cut short, as by a
        // crash, leaves the first.
        let more = Outcome::Failed("it failed".into());
        let both = format!("{out}{RESULT}{}\n", serde_json::to_string(&more).unwrap());
        assert_eq!(outcome(both.as_bytes(), None, TIMEOUT), more);
        let cut = format!("{out}{RESULT}{{\"failed\":\"it");
        assert_eq!(outcome(cut.as_bytes(), None, TIMEOUT), found);
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
        // What an older scanner found is scanned again.
        let older = ScanCache {
            version: 0,
            ..cache
        };
        older.write(&saved).unwrap();
        assert_eq!(ScanCache::read(&saved), ScanCache::new());
    }
}
