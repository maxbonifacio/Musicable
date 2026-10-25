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
fn utc(time: SystemTime) -> String {
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
