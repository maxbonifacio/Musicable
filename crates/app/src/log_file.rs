//! The log file, `musicable.log` in the config folder, which keeps what went
//! wrong while the app ran, to find out why afterwards: warnings and errors
//! from the crates the app is built on, such as wgpu, the app's own notes,
//! such as the graphics adapter it draws with and the graphics device being
//! lost, and panics, each line with the time in UTC. Once the file has grown
//! past a megabyte, it moves to `musicable.old.log`, so the two hold the
//! latest lines. What's logged goes to the console the app was started
//! from too, if any.

use std::backtrace::Backtrace;
use std::fmt::Display;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::panic::{self, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui_wgpu::RenderState;
use log::{Level, LevelFilter, Log, Metadata, Record};

/// The log file's name in the config folder.
pub const LOG_FILE: &str = "musicable.log";
/// The log file's name once it has grown past [`LIMIT`].
pub const OLD_LOG_FILE: &str = "musicable.old.log";
/// How long the log file grows, in bytes, before it moves.
const LIMIT: u64 = 1024 * 1024;

/// Logs to the log file in `config_dir`, if there is one, from now on: what
/// the `log` crate's macros log, and panics, which Rust still prints.
pub fn start(config_dir: Option<&Path>) {
    let log: &'static LogFile = Box::leak(Box::new(LogFile::new(config_dir, LIMIT)));
    if log::set_logger(log).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    let rust = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Rust prints it to the console.
        log.write(&line(
            SystemTime::now(),
            Level::Error,
            "panic",
            &panicked(info),
        ));
        rust(info);
    }));
    log::info!(
        "Musicable {} started, process {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
}

/// Logs the graphics adapter eframe draws with, and the graphics device
/// being lost, which wgpu reports only to this callback. egui-wgpu can't
/// carry on without the device, and panics.
pub(crate) fn watch_graphics(render_state: &RenderState) {
    let info = render_state.adapter.get_info();
    let driver = format!("{} {}", info.driver, info.driver_info);
    log::info!(
        "Drawing with {:?} on {}, driver {}",
        info.backend,
        info.name,
        driver.trim()
    );
    (render_state.device).set_device_lost_callback(|reason, message| {
        log::error!("The graphics device was lost ({reason:?}): {message}");
    });
}

/// How bad a line of the log file is, as it says it: `ERROR`, `WARN`,
/// `INFO`, `DEBUG` or `TRACE`.
fn level(word: &str) -> Option<Level> {
    word.parse().ok()
}

/// What [`read`] gives back of the log file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wanted<'a> {
    /// At most how many entries, the latest.
    pub most: usize,
    /// Only those this bad, or worse.
    pub at_least: Level,
    /// Only those logged at this time or after, in UTC as the file has it,
    /// such as `2026-10-25T00:50:21.123Z`.
    pub since: Option<&'a str>,
}

/// An entry of the log file: what was logged at a time, which can run to
/// several lines, as a panic's calls do.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// In UTC, such as `2026-10-25T00:50:21.123Z`.
    pub time: String,
    pub level: Level,
    /// Who logged it, such as the module it came from.
    pub target: String,
    pub message: String,
}

/// The latest entries of the log file in the config folder `dir`, and of
/// the old one before it, as `wanted` says, oldest first, and whether
/// earlier ones that it would take were left out.
pub fn read(dir: &Path, wanted: &Wanted) -> (Vec<Entry>, bool) {
    let text = [OLD_LOG_FILE, LOG_FILE]
        .map(|name| fs::read_to_string(dir.join(name)).unwrap_or_default())
        .concat();
    let mut entries: Vec<Entry> = entries(&text)
        .filter(|entry| entry.level <= wanted.at_least)
        .filter(|entry| {
            wanted
                .since
                .is_none_or(|since| entry.time.as_str() >= since)
        })
        .collect();
    let earlier = entries.len() > wanted.most;
    entries.drain(..entries.len().saturating_sub(wanted.most));
    (entries, earlier)
}

/// The entries in the log file's `text`, oldest first. A line that doesn't
/// start with a time carries on the entry before it.
fn entries(text: &str) -> impl Iterator<Item = Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in text.lines() {
        match entry(line) {
            Some(entry) => entries.push(entry),
            None => {
                if let Some(last) = entries.last_mut() {
                    last.message.push('\n');
                    last.message.push_str(line);
                }
            }
        }
    }
    entries.into_iter()
}

/// The entry that the log file's `line` starts, if it starts one:
/// `2026-10-25T00:50:21.123Z WARN  wgpu_hal: Lost`.
fn entry(line: &str) -> Option<Entry> {
    let (time, rest) = line.split_once(' ')?;
    utc_time(time)?;
    let rest = rest.trim_start();
    let (word, rest) = rest.split_once(' ')?;
    let level = level(word)?;
    let (target, message) = rest.trim_start().split_once(": ")?;
    Some(Entry {
        time: time.to_owned(),
        level,
        target: target.to_owned(),
        message: message.to_owned(),
    })
}

/// `text` as a time in UTC as the log file writes it, if it is one, such as
/// `2026-10-25T00:50:21.123Z`: to the second, as `2026-10-25T00:50:21Z`,
/// it's taken to the millisecond, `.000`.
pub fn utc_time(text: &str) -> Option<String> {
    let text = text.strip_suffix('Z')?;
    let (seconds, millis) = text.split_once('.').unwrap_or((text, "000"));
    let digits = |text: &str| text.chars().all(|c| c.is_ascii_digit());
    let shape = seconds.len() == 19
        && (seconds.char_indices()).all(|(at, c)| match at {
            4 | 7 => c == '-',
            10 => c == 'T',
            13 | 16 => c == ':',
            _ => c.is_ascii_digit(),
        });
    (shape && millis.len() == 3 && digits(millis)).then(|| format!("{seconds}.{millis}Z"))
}

/// The logger, which adds to the log file.
struct LogFile {
    /// The config folder the file is in, if there is one.
    dir: Option<PathBuf>,
    /// How long the file grows before it moves.
    limit: u64,
    /// The file, open to add to, and how long it is, unless it couldn't be
    /// opened.
    file: Mutex<Option<(File, u64)>>,
}

impl LogFile {
    fn new(dir: Option<&Path>, limit: u64) -> Self {
        Self {
            dir: dir.map(Path::to_owned),
            limit,
            file: Mutex::new(dir.and_then(open)),
        }
    }

    /// Adds `line` to the file, first moving the file to the old file's name
    /// and starting another if it has grown past the limit.
    fn write(&self, line: &str) {
        let Some(dir) = &self.dir else {
            return;
        };
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        if file.as_ref().is_some_and(|(_, len)| *len >= self.limit) {
            let _ = fs::rename(dir.join(LOG_FILE), dir.join(OLD_LOG_FILE));
            *file = open(dir);
        }
        if let Some((file, len)) = &mut *file
            && file.write_all(line.as_bytes()).is_ok()
        {
            *len += line.len() as u64;
        }
    }
}

impl Log for LogFile {
    /// Warnings and errors, and the notes of the app's own crates.
    fn enabled(&self, metadata: &Metadata) -> bool {
        let level = metadata.level();
        level <= Level::Warn || level <= Level::Info && metadata.target().starts_with("musicable")
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let line = line(
                SystemTime::now(),
                record.level(),
                record.target(),
                record.args(),
            );
            let _ = std::io::stderr().write_all(line.as_bytes());
            self.write(&line);
        }
    }

    fn flush(&self) {}
}

/// The log file in the config folder `dir`, open to add to, and how long it
/// is already.
fn open(dir: &Path) -> Option<(File, u64)> {
    fs::create_dir_all(dir).ok()?;
    let file = (OpenOptions::new().create(true).append(true))
        .open(dir.join(LOG_FILE))
        .ok()?;
    let len = file.metadata().ok()?.len();
    Some((file, len))
}

/// A line of the log file: `message`, logged at `time` at `level` by
/// `target`, such as the module it came from.
fn line(time: SystemTime, level: Level, target: &str, message: &dyn Display) -> String {
    format!("{} {level:<5} {target}: {message}\n", utc(time))
}

/// What the log file says of a panic: on which thread and where it
/// happened, its message, and the calls that led to it.
fn panicked(info: &PanicHookInfo) -> String {
    let thread = std::thread::current();
    let at = (info.location()).map_or_else(String::new, |at| format!(" at {at}"));
    format!(
        "thread '{}' panicked{at}:\n{}\nstack backtrace:\n{}",
        thread.name().unwrap_or("<unnamed>"),
        info.payload_as_str().unwrap_or("Box<dyn Any>"),
        Backtrace::force_capture(),
    )
}

/// `time` in UTC to the millisecond, as in `2026-10-25T00:50:21.123Z`.
pub(crate) fn utc(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (days, seconds) = (since.as_secs() / 86_400, since.as_secs() % 86_400);
    // Howard Hinnant's `civil_from_days`: the date `days` days after
    // 1970-01-01, counted in 400-year eras of years starting in March.
    let days = days + 719_468;
    let (era, day_of_era) = (days / 146_097, days % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let (year, month) = match month_from_march {
        0..10 => (era * 400 + year_of_era, month_from_march + 3),
        _ => (era * 400 + year_of_era + 1, month_from_march - 9),
    };
    format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        since.subsec_millis(),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(seconds: u64, ms: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(ms)
    }

    #[test]
    fn lines_have_the_time_in_utc() {
        assert_eq!(utc(at(0, 0)), "1970-01-01T00:00:00.000Z");
        // Leap days, one of them in a century year.
        assert_eq!(utc(at(951_782_400, 0)), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc(at(1_709_164_800, 5)), "2024-02-29T00:00:00.005Z");
        assert_eq!(utc(at(4_102_444_799, 999)), "2099-12-31T23:59:59.999Z");
        // 2100 has no leap day.
        assert_eq!(utc(at(4_107_542_400, 0)), "2100-03-01T00:00:00.000Z");
        assert_eq!(
            line(at(1_793_062_221, 123), Level::Warn, "wgpu_hal", &"Lost"),
            "2026-10-27T00:50:21.123Z WARN  wgpu_hal: Lost\n"
        );
    }

    #[test]
    fn the_log_file_takes_warnings_and_the_apps_own_notes() {
        let log = LogFile::new(None, LIMIT);
        let takes =
            |level, target| log.enabled(&Metadata::builder().level(level).target(target).build());
        assert!(takes(Level::Error, "wgpu_core::device"));
        assert!(takes(Level::Warn, "egui_wgpu"));
        assert!(!takes(Level::Info, "wgpu_core::instance"));
        assert!(takes(Level::Info, "musicable_app::log_file"));
        assert!(takes(Level::Info, "musicable"));
        assert!(!takes(Level::Debug, "musicable_app::app"));
    }

    #[test]
    fn reading_gives_the_latest_entries_as_asked() {
        let dir = std::env::temp_dir().join(format!("musicable-log-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(OLD_LOG_FILE),
            "2026-10-31T05:00:00.000Z INFO  musicable_app::log_file: Musicable 0.2.0 started\n\
             2026-10-31T05:00:01.500Z WARN  wgpu_hal: Slow\n",
        )
        .unwrap();
        fs::write(
            dir.join(LOG_FILE),
            "2026-10-31T05:00:02.000Z ERROR panic: thread 'main' panicked at app.rs:1:\n\
             boom\n\
             stack backtrace:\n\
             2026-10-31T05:00:03.250Z INFO  musicable: Drawing with Dx12\n",
        )
        .unwrap();
        let all = Wanted {
            most: 100,
            at_least: Level::Trace,
            since: None,
        };
        let (entries, earlier) = read(&dir, &all);
        assert!(!earlier);
        let times: Vec<_> = entries.iter().map(|entry| entry.time.as_str()).collect();
        assert_eq!(
            times,
            [
                "2026-10-31T05:00:00.000Z",
                "2026-10-31T05:00:01.500Z",
                "2026-10-31T05:00:02.000Z",
                "2026-10-31T05:00:03.250Z",
            ]
        );
        // A panic's lines are one entry.
        assert_eq!(entries[2].level, Level::Error);
        assert_eq!(entries[2].target, "panic");
        assert_eq!(
            entries[2].message,
            "thread 'main' panicked at app.rs:1:\nboom\nstack backtrace:"
        );
        // The latest, the worst and the newest.
        let (entries, earlier) = read(&dir, &Wanted { most: 1, ..all });
        assert!(earlier);
        assert_eq!(entries[0].message, "Drawing with Dx12");
        let worst = Wanted {
            at_least: Level::Warn,
            ..all
        };
        let (entries, _) = read(&dir, &worst);
        let targets: Vec<_> = entries.iter().map(|entry| entry.target.as_str()).collect();
        assert_eq!(targets, ["wgpu_hal", "panic"]);
        let since = utc_time("2026-10-31T05:00:02Z").unwrap();
        let since = Wanted {
            since: Some(&since),
            ..all
        };
        assert_eq!(read(&dir, &since).0.len(), 2);
        // With no log file, there's nothing.
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read(&dir, &all), (Vec::new(), false));
    }

    #[test]
    fn times_are_read_as_the_log_file_writes_them() {
        assert_eq!(
            utc_time("2026-10-31T05:00:02Z").as_deref(),
            Some("2026-10-31T05:00:02.000Z")
        );
        assert_eq!(
            utc_time("2026-10-31T05:00:02.250Z").as_deref(),
            Some("2026-10-31T05:00:02.250Z")
        );
        for wrong in [
            "2026-10-31",
            "2026-10-31T05:00:02",
            "2026-10-31 05:00:02Z",
            "05:00Z",
        ] {
            assert_eq!(utc_time(wrong), None, "{wrong}");
        }
    }

    #[test]
    fn the_log_file_moves_once_it_has_grown_past_its_limit() {
        let dir = std::env::temp_dir().join(format!("musicable-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let read = |name| fs::read_to_string(dir.join(name)).unwrap();
        // 40 bytes each.
        let line = |n| format!("Line {n} {}\n", "-".repeat(32));
        let lines = |range: std::ops::Range<i32>| range.map(line).collect::<String>();
        let log = LogFile::new(Some(&dir), 100);
        for n in 0..4 {
            log.write(&line(n));
        }
        // The fourth line found the file 120 bytes long.
        assert_eq!(read(LOG_FILE), lines(3..4));
        assert_eq!(read(OLD_LOG_FILE), lines(0..3));
        // Started again, the app adds to the file.
        drop(log);
        let log = LogFile::new(Some(&dir), 100);
        for n in 4..7 {
            log.write(&line(n));
        }
        assert_eq!(read(LOG_FILE), lines(6..7));
        assert_eq!(read(OLD_LOG_FILE), lines(3..6));
        drop(log);
        fs::remove_dir_all(&dir).unwrap();
    }
}
