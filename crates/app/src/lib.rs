//! The app: the open project wired to the engine and the audio device, the
//! commands that drive it, and the window the `musicable` binary opens.
//! `docs/app.md` describes it.

mod app;
mod appearance_window;
mod audio_files;
mod audio_window;
pub mod export;
mod export_window;
mod keymap_file;
pub mod library;
mod media;
pub mod null_test;
mod output;
mod plugin_scan;
mod plugin_windows;
mod plugins;
mod recorder;
mod session;
mod settings;
mod theme_file;

pub use app::App;
pub use output::Output;
pub use plugin_scan::{PLUGINS_FILE, SCAN_PLUGIN};
pub use plugins::PLUGIN_SETTLING;
pub use recorder::Recording;
pub use session::Session;
pub use settings::{AudioSettings, Settings, SettingsError, config_dir};

use musicable_commands::Registry;
use musicable_engine::{
    Engine, EngineConfig, EngineHandle, ParallelMonitor, ParallelOptions, ParallelRenderer,
};
use musicable_model::{Project, TestTone, TrackKind};

/// Every command the app has.
pub fn commands() -> Registry {
    let mut registry = Registry::new();
    musicable_commands::register_all(&mut registry);
    registry
}

/// An engine for `config` that renders with the parallel scheduler, its
/// handle, and the scheduler's counters. It starts the scheduler's worker
/// threads, so call it off the audio thread.
pub fn engine(config: EngineConfig) -> (Engine, EngineHandle, ParallelMonitor) {
    let renderer = ParallelRenderer::new(ParallelOptions::default());
    let monitor = renderer.monitor();
    let (engine, handle) = Engine::with_renderer(config, Box::new(renderer));
    (engine, handle, monitor)
}

/// The Tier 0 demo: a dozen test tones through their gain stages, looping
/// the first four bars at 124 BPM with the metronome on. One track is muted.
/// `crates/io/tests/fixtures/v*/demo.musicable` is this project, saved in
/// each version of the format.
pub fn demo_project() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(124.0);
    project.looping = true;
    project.metronome = true;
    let notes = [
        ("A1", 55.0),
        ("A2", 110.0),
        ("E3", 164.81),
        ("A3", 220.0),
        ("C4", 261.63),
        ("E4", 329.63),
        ("A4", 440.0),
        ("C5", 523.25),
        ("E5", 659.26),
        ("A5", 880.0),
        ("C6", 1046.5),
        ("E6", 1318.51),
    ];
    for (i, (name, frequency)) in notes.into_iter().enumerate() {
        let kind = if i % 3 == 2 {
            TrackKind::Midi
        } else {
            TrackKind::Audio
        };
        let id = project.add_track(name, kind);
        let track = project.track_mut(id).expect("the track was just added");
        track.gain = [1.0, 0.8, 0.5][i % 3];
        track.tone = Some(TestTone {
            frequency,
            level: 0.08,
        });
    }
    project.tracks[5].muted = true;
    project
}

/// `text` starting with a capital letter, for errors written to follow a
/// colon.
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
