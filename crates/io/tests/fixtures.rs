//! The fixture projects in `tests/fixtures`, a folder for each version of
//! the format.
//!
//! A fixture's name stands for the same project in every version: the one
//! [`fixtures`] builds. Every file must load as its project, so files saved
//! by older versions keep loading as they did, and the current version's
//! files must be exactly what saving the project writes. To cover something
//! new, add a fixture rather than changing one, or the older files stop
//! matching.
//!
//! `MUSICABLE_BLESS=1 cargo test -p musicable-io --test fixtures` rewrites
//! the current version's files. Older versions' files never change.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use musicable_io::project::{self, EXTENSION, MAX_TICKS, VERSION};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::{Project, TestTone, TrackKind};

fn fixtures() -> Vec<(&'static str, Project)> {
    vec![
        ("empty", Project::new()),
        ("demo", demo()),
        ("tempo-changes", tempo_changes()),
        ("awkward-values", awkward_values()),
    ]
}

/// The Tier 0 demo: a dozen test tones through their gain stages, looping
/// the first four bars with the metronome on.
fn demo() -> Project {
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
        let track = project.track_mut(id).unwrap();
        track.gain = [1.0, 0.8, 0.5][i % 3];
        track.tone = Some(TestTone {
            frequency,
            level: 0.08,
        });
    }
    project.tracks[5].muted = true;
    project
}

/// Tempo and time signature changes, some off the beat, and a loop over the
/// bars of 7/8.
fn tempo_changes() -> Project {
    let mut project = Project::new();
    let tempo = &mut project.tempo;
    tempo.set_tempo(133.0);
    tempo.insert_tempo(Beats::new(16), 87.5);
    // On the second triplet eighth of beat 32.
    tempo.insert_tempo(Beats::from_ratio(97, 3), 174.25);
    tempo.insert_tempo(Beats::from_ratio(1001, 16), 99.99);
    tempo.insert_signature(4, signature(6, 8));
    tempo.insert_signature(8, signature(7, 8));
    tempo.insert_signature(10, signature(3, 4));
    project.loop_region = BeatRange::new(tempo.bar_start(8), tempo.bar_start(10));
    project.looping = true;
    let id = project.add_track("Lead", TrackKind::Midi);
    project.track_mut(id).unwrap().tone = Some(TestTone {
        frequency: 392.0,
        level: 0.25,
    });
    project
}

/// Values that are easy to get wrong: names JSON has to escape, floats at
/// the edges of their ranges, the furthest positions a file can hold, a
/// backwards loop region, every kind of track, and track ids out of order
/// with gaps where tracks were deleted.
fn awkward_values() -> Project {
    let mut project = Project::new();
    let tempo = &mut project.tempo;
    tempo.set_tempo(TempoMap::MAX_BPM);
    tempo.insert_tempo(Beats::from_ticks(1), TempoMap::MIN_BPM);
    tempo.insert_tempo(Beats::from_ticks(MAX_TICKS), 1.0 / 3.0 * 100.0);
    // The longest bar, then the shortest, then the furthest bar that still
    // starts within reach.
    tempo.set_signature(signature(255, 1));
    tempo.insert_signature(1, signature(1, 64));
    let shortest = signature(1, 64).bar_length().ticks();
    let furthest = 1 + (MAX_TICKS - tempo.bar_start(1).ticks()) / shortest;
    tempo.insert_signature(furthest, signature(4, 4));
    project.loop_region =
        BeatRange::new(Beats::from_ticks(MAX_TICKS), Beats::from_ticks(-MAX_TICKS));
    project.looping = true;

    let names = [
        "",
        "\"Quotes\" and \\backslashes\\",
        "Tab\tnew\nline\r",
        "Ünïcödé • 音楽 • 🎛️",
        "\u{0}\u{1f}\u{7f}",
        "\u{2028}\u{2029}",
        "  spaces  ",
        "مرحبا",
    ];
    let gains = [
        -0.0,
        f32::MIN_POSITIVE,
        f32::from_bits(1),
        f32::MAX,
        -1.0,
        0.1,
        1.0 / 3.0,
        // Reading this through an f64 rounds it the wrong way.
        f32::from_bits(0x15ae_43fd),
    ];
    let tones = [
        None,
        Some((f64::MAX, f32::MIN)),
        Some((f64::from_bits(1), -0.0)),
        Some((-440.0, f32::from_bits(0x95ae_43fd))),
        Some((1e-300, 1e-38)),
        Some((0.0, 1.0)),
        Some((-0.0, f32::MAX)),
        None,
    ];
    let kinds = [
        TrackKind::Audio,
        TrackKind::Midi,
        TrackKind::Return,
        TrackKind::Group,
    ];
    for (i, name) in names.into_iter().enumerate() {
        if i == 3 {
            project.add_track("Deleted", TrackKind::Audio);
        }
        let id = project.add_track(name, kinds[i % kinds.len()]);
        let track = project.track_mut(id).unwrap();
        track.gain = gains[i];
        track.muted = i % 2 == 1;
        track.tone = tones[i].map(|(frequency, level)| TestTone { frequency, level });
    }
    project.tracks.retain(|track| track.name != "Deleted");
    project.tracks.swap(0, 5);
    // As if three more tracks had come and gone.
    project.next_track_id += 3;
    project
}

fn signature(numerator: u8, denominator: u8) -> TimeSignature {
    TimeSignature::new(numerator, denominator).unwrap()
}

fn folder(version: u32) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("v{version}"))
}

fn blessing() -> bool {
    env::var_os("MUSICABLE_BLESS").is_some()
}

#[test]
fn current_files_are_what_saving_writes() {
    let folder = folder(VERSION);
    if blessing() {
        fs::create_dir_all(&folder).unwrap();
    }
    let mut stale = Vec::new();
    for (name, project) in fixtures() {
        let saved = project::to_string(&project).unwrap();
        let path = folder.join(format!("{name}.{EXTENSION}"));
        if blessing() {
            fs::write(&path, saved).unwrap();
        } else if fs::read_to_string(&path).ok() != Some(saved) {
            stale.push(path.display().to_string());
        }
    }
    assert!(
        stale.is_empty(),
        "these fixtures aren't what saving their projects writes:\n  {}\n\
         If the format changed, raise VERSION and add a migration step first \
         (see docs/project-format.md). If only the projects changed, run\n  \
         MUSICABLE_BLESS=1 cargo test -p musicable-io --test fixtures",
        stale.join("\n  ")
    );
}

#[test]
fn every_version_loads_as_the_same_projects() {
    let fixtures = fixtures();
    for version in 1..=VERSION {
        // Being rewritten, and checked on the next run.
        if version == VERSION && blessing() {
            continue;
        }
        let folder = folder(version);
        let files = fs::read_dir(&folder).unwrap_or_else(|error| {
            panic!(
                "{}: {error}. Fixtures of every version stay, so files that old \
                 keep loading.",
                folder.display()
            )
        });
        for file in files {
            let path = file.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap();
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some(EXTENSION),
                "{}",
                path.display()
            );
            let Some((_, project)) = fixtures.iter().find(|(n, _)| *n == name) else {
                panic!("{} isn't one of the fixtures", path.display());
            };
            let loaded =
                project::load(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert_eq!(&loaded, project, "{}", path.display());
            // Equality can't tell -0.0 from 0.0, but the saved text can.
            assert_eq!(
                project::to_string(&loaded).unwrap(),
                project::to_string(project).unwrap(),
                "{}",
                path.display()
            );
        }
    }
    assert!(
        !folder(VERSION + 1).exists(),
        "there are fixtures for a version after {VERSION}"
    );
}

#[test]
fn the_documented_example_is_a_real_file() {
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/project-format.md");
    // Git on Windows may check the page out with CRLF line endings.
    let page = fs::read_to_string(page).unwrap().replace("\r\n", "\n");
    let example = page
        .split("```json\n")
        .nth(1)
        .and_then(|rest| rest.split("```").next())
        .expect("docs/project-format.md starts with an example file");
    let project = project::from_str(example).unwrap();
    assert_eq!(project::to_string(&project).unwrap(), example);
}
