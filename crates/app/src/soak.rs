//! `musicable soak`: watches a long run of the Musicable that's open, as
//! the PC checks' soaks need, without anyone watching: what the project
//! costs the CPU, dropouts, how long frames take, memory, the log's
//! warnings and errors, and whether Musicable stops answering, as it does
//! when it crashes. On Windows it can put the displays to sleep and wake
//! them on a timer too, as the egui-wgpu panic as the displays woke needed.
//!
//! It writes what it reads as a JSON line every few seconds, so a run cut
//! short still says what happened, and says how it went at the end.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use log::Level;
use musicable_mcp::client::Client;
use musicable_mcp::pipe::Address;
use serde_json::{Value as Json, json};

use crate::log_file::{self, Wanted};

const USAGE: &str = "\
usage: musicable soak [--minutes M | --hours H] [--every SECONDS]
                      [--sleep-displays MINUTES] [--asleep SECONDS] [--out FILE]

Watches the Musicable that's open, which must let agents in, for a long run:
what the project costs the CPU, dropouts, frame times, memory, the log's
warnings and errors, and whether it stops answering. It writes what it reads
to FILE as a JSON line every few seconds, and says how it went at the end.

  --minutes M, --hours H   how long to watch: 60 minutes without them
  --every SECONDS          how often to read: every 5 seconds without it
  --sleep-displays MINUTES puts the displays to sleep this often, on Windows
  --asleep SECONDS         and wakes them after this long: 60 without it
  --out FILE               where the JSON lines go: soak-<time>.jsonl here
                           without it";

/// How long it tries to reach Musicable again once it stops answering, as
/// when it's restarted, before it gives up.
const RECONNECTING: Duration = Duration::from_secs(60);

/// How `musicable soak` was asked to run.
#[derive(Debug, Clone, PartialEq)]
struct Options {
    length: Duration,
    every: Duration,
    sleep_displays: Option<Duration>,
    asleep: Duration,
    out: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            length: Duration::from_secs(60 * 60),
            every: Duration::from_secs(5),
            sleep_displays: None,
            asleep: Duration::from_secs(60),
            out: None,
        }
    }
}

/// The options in `args`, or why they're wrong.
fn options(args: impl Iterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut args = args.map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        let mut number = |name: &str| -> Result<f64, String> {
            let value = args
                .next()
                .ok_or_else(|| format!("{name} needs a number"))?;
            match value.parse::<f64>() {
                Ok(number) if number.is_finite() && number > 0.0 => Ok(number),
                _ => Err(format!("{name} needs a number above 0, not \"{value}\"")),
            }
        };
        match arg.as_str() {
            "--minutes" => options.length = Duration::from_secs_f64(number(&arg)? * 60.0),
            "--hours" => options.length = Duration::from_secs_f64(number(&arg)? * 3600.0),
            "--every" => options.every = Duration::from_secs_f64(number(&arg)?.max(0.5)),
            "--sleep-displays" => {
                options.sleep_displays = Some(Duration::from_secs_f64(number(&arg)? * 60.0));
            }
            "--asleep" => options.asleep = Duration::from_secs_f64(number(&arg)?),
            "--out" => {
                let out = args.next().ok_or("--out needs a file")?;
                options.out = Some(PathBuf::from(out));
            }
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unknown option \"{other}\"")),
        }
    }
    if options.sleep_displays.is_some() && !cfg!(windows) {
        return Err("--sleep-displays puts the displays to sleep on Windows only, so far".into());
    }
    if options
        .sleep_displays
        .is_some_and(|every| every <= options.asleep)
    {
        return Err("--sleep-displays must be longer than --asleep".into());
    }
    Ok(Some(options))
}

/// `musicable soak ...`.
pub fn command_line(args: impl Iterator<Item = OsString>) -> ExitCode {
    let options = match options(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(why) => {
            eprintln!("{why}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match soak(&options) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(why) => {
            eprintln!("musicable soak: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Watches as `options` say, and returns whether the run went cleanly: no
/// errors in the log, and Musicable answering throughout.
fn soak(options: &Options) -> Result<bool, String> {
    let dir =
        crate::config_dir().ok_or("The system doesn't say where Musicable's settings are.")?;
    let address = Address::new(&dir).map_err(|error| error.to_string())?;
    let mut client = Client::connect(&address, "musicable-soak").map_err(|_| {
        "Musicable isn't open, or doesn't let agents in: switch on \"Let agents control \
         Musicable\" on the Agents page of its Settings window."
            .to_owned()
    })?;
    let out = options.out.clone().unwrap_or_else(|| {
        let time = log_file::utc(SystemTime::now()).replace([':', '.'], "-");
        PathBuf::from(format!("soak-{time}.jsonl"))
    });
    let file = File::create(&out).map_err(|e| format!("Couldn't write {}: {e}", out.display()))?;
    let mut lines = BufWriter::new(file);
    println!(
        "Watching Musicable for {:.1} minutes, every {} s, into {}",
        options.length.as_secs_f64() / 60.0,
        options.every.as_secs_f64(),
        out.display()
    );
    let started = Instant::now();
    let mut summary = Summary::default();
    let mut log_since = log_file::utc(SystemTime::now());
    let mut displays = Displays::new(options);
    let mut last_said = Instant::now();
    let mut next = Instant::now();
    while started.elapsed() < options.length {
        if let Some(event) = displays.tick(Instant::now()) {
            summary.display_events += 1;
            write_line(&mut lines, &json!({ "time": now(), "displays": event }));
        }
        let mut line = json!({ "time": now(), "seconds": started.elapsed().as_secs() });
        match read(&client) {
            Ok((performance, status)) => {
                summary.add(&performance);
                line["performance"] = performance;
                line["playing"] = status["playing"].clone();
                line["process"] = status["process"].clone();
            }
            Err(why) => {
                summary.stops += 1;
                line["stopped"] = json!(why);
                write_line(&mut lines, &line);
                println!("{} Musicable stopped answering: {why}", clock(started));
                match reconnect(&address) {
                    Some(again) => {
                        client = again;
                        println!("{} Musicable answers again", clock(started));
                        continue;
                    }
                    None => {
                        let (entries, _) = log_file::read(&dir, &recent(&log_since));
                        for entry in entries {
                            summary.logged(&entry);
                            write_line(&mut lines, &logged_line(&entry));
                        }
                        break;
                    }
                }
            }
        }
        let (entries, _) = log_file::read(&dir, &recent(&log_since));
        for entry in &entries {
            summary.logged(entry);
            println!(
                "{} {} {}",
                clock(started),
                entry.level,
                first_line(&entry.message)
            );
        }
        if let Some(last) = entries.last() {
            // Past the last one read, to the millisecond.
            log_since = after(&last.time);
        }
        line["log"] = (entries.iter().map(logged_line)).collect();
        write_line(&mut lines, &line);
        if last_said.elapsed() >= Duration::from_secs(60) {
            last_said = Instant::now();
            println!("{} {}", clock(started), summary.lately());
        }
        next += options.every;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    displays.wake();
    let _ = lines.flush();
    let clean = summary.stops == 0 && summary.errors == 0;
    println!("{}", summary.report(started.elapsed()));
    println!("What it read is in {}", out.display());
    Ok(clean)
}

/// What the performance and the status say now.
fn read(client: &Client) -> Result<(Json, Json), String> {
    let performance = client.call("get_performance", json!({}))?;
    let status = client.call("get_status", json!({}))?;
    Ok((performance, status))
}

/// A connection to Musicable again, if it answers within [`RECONNECTING`].
fn reconnect(address: &Address) -> Option<Client> {
    let wait = RECONNECTING;
    Client::connect_within(address, "musicable-soak", wait, || None).ok()
}

/// What to read of the log: the warnings and errors since `since`.
fn recent(since: &str) -> Wanted<'_> {
    Wanted {
        most: 200,
        at_least: Level::Warn,
        since: Some(since),
    }
}

/// The time a millisecond after `time`, as the log file writes times, so
/// what's read next starts after it.
fn after(time: &str) -> String {
    let Some(stem) = time.strip_suffix('Z') else {
        return time.to_owned();
    };
    let Some((seconds, millis)) = stem.rsplit_once('.') else {
        return time.to_owned();
    };
    match millis.parse::<u32>() {
        Ok(millis) if millis < 999 => format!("{seconds}.{:03}Z", millis + 1),
        // The next second: rare enough to read an entry twice.
        _ => time.to_owned(),
    }
}

fn logged_line(entry: &log_file::Entry) -> Json {
    json!({
        "time": entry.time,
        "level": entry.level.as_str(),
        "target": entry.target,
        "message": entry.message,
    })
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

fn write_line(out: &mut impl Write, json: &Json) {
    let _ = writeln!(out, "{json}");
    let _ = out.flush();
}

fn now() -> String {
    log_file::utc(SystemTime::now())
}

/// How long since `started`, as `1:02:03`.
fn clock(started: Instant) -> String {
    let seconds = started.elapsed().as_secs();
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// `value` rounded to `places` decimal places, with no minus sign on a
/// change that rounds to nothing.
fn rounded(value: f64, places: i32) -> f64 {
    let scale = 10f64.powi(places);
    // Adding 0 turns -0 into 0.
    (value * scale).round() / scale + 0.0
}

/// How the run has gone so far.
#[derive(Debug, Default)]
struct Summary {
    reads: u64,
    /// The highest audio load, and the sum of them for the average.
    load_max: f64,
    load_sum: f64,
    loads: u64,
    /// The dropouts at the first read and the last.
    dropouts: Option<(u64, u64)>,
    /// The longest frame, if any frames ran: headless, none do.
    frames_longest_ms: Option<f64>,
    /// Private memory where the system says it, else resident: the first,
    /// the last and the most.
    memory: Option<(f64, f64, f64)>,
    warnings: u64,
    errors: u64,
    panics: u64,
    stops: u64,
    display_events: u64,
}

impl Summary {
    fn add(&mut self, performance: &Json) {
        self.reads += 1;
        if let Some(load) = performance["load"].as_f64() {
            self.load_max = self.load_max.max(load);
            self.load_sum += load;
            self.loads += 1;
        }
        if let Some(dropouts) = performance["dropouts"].as_u64() {
            let first = self
                .dropouts
                .map_or(dropouts, |(first, _)| first.min(dropouts));
            self.dropouts = Some((first, dropouts));
        }
        if let Some(longest) = performance["frames"]["longest_ms"].as_f64() {
            self.frames_longest_ms = Some(
                self.frames_longest_ms
                    .map_or(longest, |most| most.max(longest)),
            );
        }
        let memory = &performance["memory"];
        if let Some(mb) = memory["private_mb"]
            .as_f64()
            .or(memory["resident_mb"].as_f64())
        {
            self.memory = Some(match self.memory {
                None => (mb, mb, mb),
                Some((first, _, most)) => (first, mb, most.max(mb)),
            });
        }
    }

    fn logged(&mut self, entry: &log_file::Entry) {
        match entry.level {
            Level::Error if entry.target == "panic" => {
                self.errors += 1;
                self.panics += 1;
            }
            Level::Error => self.errors += 1,
            _ => self.warnings += 1,
        }
    }

    /// A line on how it's going.
    fn lately(&self) -> String {
        let dropouts = self.dropouts.map_or(0, |(first, last)| last - first);
        let memory = self
            .memory
            .map_or("memory unknown".to_owned(), |(first, last, _)| {
                format!(
                    "{last:.0} MB, {:+.0} MB since the start",
                    rounded(last - first, 0)
                )
            });
        let frames = self.frames_longest_ms.map_or("no frames".to_owned(), |ms| {
            format!("frames at most {ms:.1} ms")
        });
        format!(
            "load at most {:.2}, {dropouts} dropouts, {frames}, {memory}, {} warnings, {} errors",
            self.load_max, self.warnings, self.errors
        )
    }

    /// How the whole run went, over `length`.
    fn report(&self, length: Duration) -> String {
        let hours = length.as_secs_f64() / 3600.0;
        let mut report = format!(
            "Watched for {:.1} minutes, {} reads: {}.",
            length.as_secs_f64() / 60.0,
            self.reads,
            self.lately()
        );
        if self.loads > 0 {
            report.push_str(&format!(
                " The load averaged {:.2}.",
                self.load_sum / self.loads as f64
            ));
        }
        if let Some((first, last, most)) = self.memory
            && hours > 0.0
        {
            report.push_str(&format!(
                " Memory went from {first:.0} MB to {last:.0} MB, at most {most:.0} MB: {:+.1} MB \
                 an hour.",
                rounded((last - first) / hours, 1)
            ));
        }
        if self.panics > 0 {
            report.push_str(&format!(" {} panics.", self.panics));
        }
        if self.stops > 0 {
            report.push_str(&format!(
                " Musicable stopped answering {} times.",
                self.stops
            ));
        }
        if self.display_events > 0 {
            report.push_str(&format!(
                " The displays slept and woke {} times.",
                self.display_events / 2
            ));
        }
        report
    }
}

/// Puts the displays to sleep and wakes them on a timer, as `options` say.
struct Displays {
    every: Option<Duration>,
    asleep: Duration,
    /// When they last went to sleep, while they sleep.
    since: Option<Instant>,
    last: Instant,
}

impl Displays {
    fn new(options: &Options) -> Self {
        Self {
            every: options.sleep_displays,
            asleep: options.asleep,
            since: None,
            last: Instant::now(),
        }
    }

    /// Puts them to sleep or wakes them, if it's time, and says which.
    fn tick(&mut self, now: Instant) -> Option<&'static str> {
        let every = self.every?;
        match self.since {
            Some(since) if now - since >= self.asleep => {
                self.wake();
                Some("woken")
            }
            None if now - self.last >= every => {
                self.since = Some(now);
                self.last = now;
                displays::sleep();
                Some("asleep")
            }
            _ => None,
        }
    }

    /// Wakes them, if they sleep.
    fn wake(&mut self) {
        if self.since.take().is_some() {
            displays::wake();
        }
    }
}

#[cfg(windows)]
mod displays {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND,
    };

    /// Puts the displays to sleep, as their power saving does.
    pub(super) fn sleep() {
        // 2 turns them off. Posted, so a window that doesn't answer can't
        // hold it up.
        let (wparam, lparam) = (WPARAM(SC_MONITORPOWER as usize), LPARAM(2));
        // SAFETY: a message to every top-level window, with no pointers.
        let _ = unsafe { PostMessageW(Some(HWND_BROADCAST), WM_SYSCOMMAND, wparam, lparam) };
    }

    /// Wakes them, as moving the mouse does: a pixel there and back.
    pub(super) fn wake() {
        let nudge = |dx: i32| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [nudge(1), nudge(-1)];
        // SAFETY: the inputs are as big as the size given.
        unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    }
}

#[cfg(not(windows))]
mod displays {
    /// Elsewhere, `--sleep-displays` is refused before it gets here.
    pub(super) fn sleep() {}
    pub(super) fn wake() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(args: &[&str]) -> Result<Option<Options>, String> {
        options(args.iter().map(OsString::from))
    }

    #[test]
    fn the_options_say_how_long_how_often_and_where() {
        let options = parsed(&["--hours", "11", "--every", "10", "--out", "run.jsonl"])
            .unwrap()
            .unwrap();
        assert_eq!(options.length, Duration::from_secs(11 * 3600));
        assert_eq!(options.every, Duration::from_secs(10));
        assert_eq!(options.out, Some(PathBuf::from("run.jsonl")));
        assert_eq!(
            parsed(&["--minutes"]),
            Err("--minutes needs a number".into())
        );
        assert_eq!(
            parsed(&["--every", "-1"]),
            Err("--every needs a number above 0, not \"-1\"".into())
        );
        assert_eq!(parsed(&["--help"]), Ok(None));
        if cfg!(windows) {
            let options = parsed(&["--sleep-displays", "10", "--asleep", "30"])
                .unwrap()
                .unwrap();
            assert_eq!(options.sleep_displays, Some(Duration::from_secs(600)));
            assert_eq!(
                parsed(&["--sleep-displays", "1", "--asleep", "90"]),
                Err("--sleep-displays must be longer than --asleep".into())
            );
        } else {
            assert!(parsed(&["--sleep-displays", "10"]).is_err());
        }
    }

    #[test]
    fn the_summary_says_how_the_run_went() {
        let mut summary = Summary::default();
        let read = |load: f64, dropouts: u64, longest: f64, mb: f64| {
            json!({
                "load": load,
                "dropouts": dropouts,
                "frames": {"longest_ms": longest},
                "memory": {"resident_mb": mb + 20.0, "private_mb": mb},
            })
        };
        summary.add(&read(0.2, 3, 4.0, 400.0));
        summary.add(&read(0.6, 3, 16.5, 420.0));
        summary.add(&read(0.4, 5, 7.0, 410.0));
        let entry = |level, target: &str| log_file::Entry {
            time: "2026-11-01T00:00:00.000Z".into(),
            level,
            target: target.into(),
            message: "boom".into(),
        };
        summary.logged(&entry(Level::Warn, "wgpu_hal"));
        summary.logged(&entry(Level::Error, "panic"));
        assert_eq!(
            summary.lately(),
            "load at most 0.60, 2 dropouts, frames at most 16.5 ms, 410 MB, +10 MB since the \
             start, 1 warnings, 1 errors"
        );
        let report = summary.report(Duration::from_secs(1800));
        assert!(report.contains("+20.0 MB an hour"), "{report}");
        assert!(report.contains("The load averaged 0.40."), "{report}");
        assert!(report.contains("1 panics."), "{report}");
    }

    #[test]
    fn a_headless_run_with_steady_memory_says_so() {
        let mut summary = Summary::default();
        // Headless runs no frames, and memory that falls a little hasn't
        // changed to the megabyte.
        summary.add(&json!({"load": 0.0, "dropouts": 0, "frames": null,
            "memory": {"resident_mb": 30.0, "private_mb": 12.6}}));
        summary.add(&json!({"load": 0.0, "dropouts": 0, "frames": null,
            "memory": {"resident_mb": 30.0, "private_mb": 12.59}}));
        assert_eq!(
            summary.lately(),
            "load at most 0.00, 0 dropouts, no frames, 13 MB, +0 MB since the start, 0 warnings, \
             0 errors"
        );
        let report = summary.report(Duration::from_secs(3600));
        assert!(report.contains("+0.0 MB an hour"), "{report}");
    }

    #[test]
    fn the_log_is_read_on_from_just_after_its_last_entry() {
        assert_eq!(
            after("2026-11-01T00:00:01.250Z"),
            "2026-11-01T00:00:01.251Z"
        );
        assert_eq!(
            after("2026-11-01T00:00:01.999Z"),
            "2026-11-01T00:00:01.999Z"
        );
    }

    #[test]
    fn displays_sleep_and_wake_on_a_timer() {
        let options = Options {
            sleep_displays: Some(Duration::from_secs(600)),
            asleep: Duration::from_secs(60),
            ..Options::default()
        };
        let mut displays = Displays::new(&options);
        let start = displays.last;
        let at = |s: u64| start + Duration::from_secs(s);
        assert_eq!(displays.tick(at(599)), None);
        // The tests leave the system's displays alone.
        if !cfg!(windows) {
            assert_eq!(displays.tick(at(600)), Some("asleep"));
            assert_eq!(displays.tick(at(659)), None);
            assert_eq!(displays.tick(at(660)), Some("woken"));
            assert_eq!(displays.tick(at(1199)), None);
            assert_eq!(displays.tick(at(1200)), Some("asleep"));
        }
    }
}
