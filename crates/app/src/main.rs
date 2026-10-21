// Release builds on Windows open no console window. The commands that print
// attach to the console they were started from, if any.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use eframe::egui;
use musicable_app::App;
use musicable_app::null_test::{self, Player, Render};
use musicable_commands::{Keymap, KeymapFile, Registry};
use musicable_engine::{EngineConfig, ParallelOptions};
use musicable_model::Project;

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
usage: musicable [PROJECT]             open the app, with PROJECT if given
       musicable keymap [FILE]         print the keys, with the changes in the keymap FILE
       musicable null-test [PROJECT]   check that the parallel scheduler plays PROJECT, or
                                       the demo, exactly as the reference renderer does";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    match first.as_ref().and_then(|arg| arg.to_str()) {
        Some("keymap") => {
            attach_console();
            print_keymap(&musicable_app::commands(), args.next())
        }
        Some("null-test") => {
            attach_console();
            null_test(args)
        }
        Some("-h" | "--help") => {
            attach_console();
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(option) if option.starts_with('-') => {
            attach_console();
            eprintln!("unknown option \"{option}\"\n{USAGE}");
            ExitCode::FAILURE
        }
        _ => run(first.map(PathBuf::from)),
    }
}

/// Opens the app's window, with the project at `path` if given, and returns
/// once it closes.
fn run(path: Option<PathBuf>) -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Musicable")
            .with_app_id("musicable")
            .with_inner_size([1100.0, 640.0])
            .with_min_inner_size([760.0, 360.0])
            // The system's default icon rather than egui's logo, until
            // Musicable has its own.
            .with_icon(egui::IconData::default()),
        // Ctrl+C, X and V go to the keymap like other keys. Text fields
        // still get them: see `musicable_ui::KeyPresses`.
        clipboard_shortcuts: false,
        ..eframe::NativeOptions::default()
    };
    let result = eframe::run_native(
        "Musicable",
        options,
        Box::new(|_| Ok(Box::new(App::new(path)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("Musicable couldn't open its window: {error}");
            attach_console();
            eprintln!("{message}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Musicable")
                .set_description(message)
                .show();
            ExitCode::FAILURE
        }
    }
}

/// `musicable keymap [FILE]`: prints the key bindings of the default keymap,
/// with the changes in the keymap file `FILE`, if given.
fn print_keymap(commands: &Registry, file: Option<OsString>) -> ExitCode {
    let mut keymap = Keymap::default_keymap();
    if let Some(path) = file {
        let path = Path::new(&path);
        match KeymapFile::read(path) {
            Ok((changes, problems)) => {
                for problem in problems {
                    eprintln!("{}: {problem}", path.display());
                }
                changes.apply_to(&mut keymap);
            }
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    for command in commands.iter() {
        let chords: Vec<_> = keymap
            .chords_for(&command.id)
            .map(|c| c.to_string())
            .collect();
        let chords = if chords.is_empty() {
            "(none)".to_owned()
        } else {
            chords.join(", ")
        };
        println!("{:<28} {:<18} {chords}", command.id, command.title);
    }
    for (chord, id) in keymap.dangling(commands) {
        eprintln!("{chord} runs {id}, which isn't a command");
    }
    ExitCode::SUCCESS
}

const NULL_TEST_USAGE: &str = "\
usage: musicable null-test [PROJECT] [options]

Plays PROJECT, or the Tier 0 demo, from the start of the song with the
parallel scheduler and with the reference renderer side by side, offline,
and checks they play the same, sample for sample. Then it saves the project,
opens it again, and checks it still plays the same.

  --seconds <s>       how long to play (default: 20)
  --rate <hz>         the sample rate (default: 48000)
  --buffer <frames>   the buffer size, which the engine renders in
                      (default: 128)";

/// What `musicable null-test` plays.
struct NullTest {
    project: Option<PathBuf>,
    seconds: f64,
    rate: u32,
    buffer: usize,
}

impl NullTest {
    /// The options in `args`, or `None` if they ask for help.
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>, String> {
        let mut test = Self {
            project: None,
            seconds: 20.0,
            rate: 48_000,
            buffer: 128,
        };
        while let Some(arg) = args.next() {
            let Some(option) = arg.to_str().filter(|arg| arg.starts_with('-')) else {
                if test.project.replace(PathBuf::from(&arg)).is_some() {
                    return Err("null-test takes one project".to_owned());
                }
                continue;
            };
            let mut value = || {
                args.next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| format!("{option} needs a number"))
            };
            match option {
                "-h" | "--help" => return Ok(None),
                "--seconds" => {
                    test.seconds = number(option, &value()?)?;
                    if !(test.seconds > 0.0 && test.seconds <= 3600.0) {
                        return Err(format!("{option} is more than 0 and at most 3600"));
                    }
                }
                "--rate" => test.rate = number(option, &value()?)?,
                "--buffer" => test.buffer = number(option, &value()?)?,
                _ => return Err(format!("unknown option \"{option}\"")),
            }
        }
        if test.rate == 0 || !(1..=65_536).contains(&test.buffer) {
            return Err("the rate is more than 0, and the buffer from 1 to 65536".to_owned());
        }
        Ok(Some(test))
    }
}

fn number<T: std::str::FromStr>(option: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{option} takes a number, not \"{text}\""))
}

/// `musicable null-test`: see [`NULL_TEST_USAGE`].
fn null_test(args: impl Iterator<Item = OsString>) -> ExitCode {
    let test = match NullTest::parse(args) {
        Ok(Some(test)) => test,
        Ok(None) => {
            println!("{NULL_TEST_USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{NULL_TEST_USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let (project, name) = match &test.project {
        Some(path) => match musicable_io::project::load(path) {
            Ok(project) => (project, path.display().to_string()),
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => (musicable_app::demo_project(), "the demo".to_owned()),
    };
    // Saved and opened again, as File > Save and File > Open would, without
    // the file.
    let reopened = musicable_io::project::to_string(&project)
        .map_err(|error| error.to_string())
        .and_then(|text| musicable_io::project::from_str(&text).map_err(|e| e.to_string()));
    let reopened = match reopened {
        Ok(reopened) => reopened,
        Err(error) => {
            eprintln!("Saving it and opening it again failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let config = EngineConfig {
        sample_rate: test.rate,
        max_block: test.buffer,
    };
    let frames = (test.seconds * f64::from(test.rate)).round().max(1.0) as u64;
    let tracks = match project.tracks.len() {
        1 => "1 track".to_owned(),
        n => format!("{n} tracks"),
    };
    println!(
        "Null test of {name}: {tracks}, {} s at {} Hz in {}-frame buffers\n",
        test.seconds, test.rate, test.buffer,
    );
    let shared = Render::Parallel(ParallelOptions {
        inline_below: None,
        ..ParallelOptions::default()
    });
    let same_project = reopened == project;
    let runs: [(&str, Project, Render); 4] = [
        ("Reference renderer", project.clone(), Render::Reference),
        ("Parallel, as the app plays", project.clone(), Render::App),
        ("Parallel, every block shared", project, shared),
        ("Saved and opened again", reopened, Render::App),
    ];
    let names = runs.each_ref().map(|run| run.0);
    let mut players: Vec<Player> = runs
        .into_iter()
        .map(|(_, project, render)| Player::new(project, &render, config))
        .collect();
    let differences = null_test::compare(&mut players, frames);

    let speed = |player: &Player| {
        let took = player.took().as_secs_f64();
        let played = frames as f64 / f64::from(test.rate);
        format!("{took:.2} s, {:.0}x real time", played / took.max(1e-9))
    };
    let mut passed = true;
    for ((name, player), difference) in names.iter().zip(&players).zip(&differences) {
        let outcome = match (difference, player.parallel()) {
            // The reference.
            (None, None) => format!("{}, {}", level(player.peak()), speed(player)),
            (Some(d), _) => {
                passed = false;
                let channel = ["left", "right"][d.channel];
                format!(
                    "DIFFERENT at {:.3} s (frame {}), {channel} channel: {} instead of {}",
                    d.frame as f64 / f64::from(test.rate),
                    d.frame,
                    d.got,
                    d.expected,
                )
            }
            (None, Some(stats)) => {
                let blocks = stats.parallel_blocks + stats.inline_blocks;
                let mut outcome = format!(
                    "identical, {}; {} of {blocks} blocks shared, {} workers",
                    speed(player),
                    stats.parallel_blocks,
                    stats.threads,
                );
                if stats.parallel_blocks > 0 {
                    // While blocks were shared, counting the audio thread.
                    outcome += &format!(", {:.1} threads busy", stats.cores_used());
                }
                outcome
            }
        };
        println!("{name:<30} {outcome}");
    }
    if !same_project {
        passed = false;
        println!("\nSaved and opened again, the project wasn't the same.");
    }
    if players[0].peak() == 0.0 {
        println!("\nIt was silent all the way, so the test shows little.");
    }
    if passed {
        println!(
            "\nPassed: the parallel scheduler played it exactly as the reference renderer \
             did, before and after saving."
        );
        ExitCode::SUCCESS
    } else {
        println!("\nFailed.");
        ExitCode::FAILURE
    }
}

/// A peak level, in decibels relative to full scale.
fn level(peak: f32) -> String {
    if peak > 0.0 {
        format!("peak {:.1} dBFS", 20.0 * peak.log10())
    } else {
        "silent".to_owned()
    }
}

/// Lets a release build on Windows print to the console it was started from.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // Safety: AttachConsole takes no pointers. Without a console to attach
    // to, it fails and nothing changes.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}
