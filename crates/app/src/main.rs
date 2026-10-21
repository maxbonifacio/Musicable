// Release builds on Windows open no console window. The commands that print
// attach to the console they were started from, if any.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use eframe::egui;
use musicable_app::App;
use musicable_commands::{Keymap, KeymapFile, Registry};

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
usage: musicable [PROJECT]       open the app, with PROJECT if given
       musicable keymap [FILE]   print the keys, with the changes in the keymap FILE";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    match first.as_ref().and_then(|arg| arg.to_str()) {
        Some("keymap") => {
            attach_console();
            print_keymap(&musicable_app::commands(), args.next())
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
