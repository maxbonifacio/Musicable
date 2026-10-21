use std::path::Path;
use std::process::ExitCode;

use musicable_commands::{Keymap, KeymapFile, Registry};
use musicable_model::Project;

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

fn main() -> ExitCode {
    let commands = musicable_app::commands();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("keymap") => print_keymap(&commands, args.next()),
        Some(other) => {
            eprintln!("unknown command \"{other}\"; try `musicable keymap [FILE]`");
            ExitCode::FAILURE
        }
        None => {
            let project = Project::new();
            println!(
                "musicable {}: {} tracks, {} commands registered",
                env!("CARGO_PKG_VERSION"),
                project.tracks.len(),
                commands.iter().count()
            );
            ExitCode::SUCCESS
        }
    }
}

/// `musicable keymap [FILE]`: prints the key bindings of the default keymap,
/// with the changes in the keymap file `FILE`, if given.
fn print_keymap(commands: &Registry, file: Option<String>) -> ExitCode {
    let mut keymap = Keymap::default_keymap();
    if let Some(path) = file {
        match KeymapFile::read(Path::new(&path)) {
            Ok((changes, problems)) => {
                for problem in problems {
                    eprintln!("{path}: {problem}");
                }
                changes.apply_to(&mut keymap);
            }
            Err(error) => {
                eprintln!("{path}: {error}");
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
