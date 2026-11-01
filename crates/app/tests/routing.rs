//! Routing through the app: a track that records another's Post Fader,
//! chosen as the session plays, prints that track's stem, as an export
//! renders it, sample for sample, with a latent plugin on a third track,
//! and Capture takes it the same way. A track resampling the master
//! records the mixdown without itself in it, and a MIDI track recording
//! another's notes records its clip's notes where they are, those an
//! arpeggiator plays on the other track where it played them, and those a
//! synth plays out as it renders, on the frames it played them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use musicable_app::Session;
use musicable_app::export::{Bits, Export, ExportSettings, FileFormat, Job, Span, files_for};
use musicable_commands::{Args, Editor, Keymap, mixer, transport};
use musicable_engine::{EngineConfig, rt};
use musicable_io::audio::{Decoder, SampleFormat, save_wav};
use musicable_model::clip::{AudioClip, ClipContent, Controller, MidiClip, Point};
use musicable_model::plugin::{PluginDevice, PluginKind, PluginSettings};
use musicable_model::routing::Tap;
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{
    Chain, DeviceKind, Input, Monitor, PluginFormat, Project, TestTone, TrackId, TrackKind,
};
use musicable_plugins::{PluginInfo, scan};
use musicable_test_plugins::testing::Library;
use musicable_test_plugins::{ARP_STEP, ARPEGGIATOR_ID, ECHO_UP, GAIN, GAIN_ID, LATENCY, SYNTH_ID};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 512,
};

/// The frames a device asks for at a time: the RME's period, which the
/// engine's blocks don't divide.
const PERIOD: usize = 441;

/// Four beats at 120 BPM: the song, in frames.
const SONG: u64 = 96_000;

/// The latency of the Test Gain on the bass, in frames.
const LATE: u64 = 300;

/// A folder of its own for each test, made fresh.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("musicable-routing-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("Audio")).unwrap();
    dir
}

/// A sine a channel at `frequency` and its harmonic, with a click every
/// 1,000 frames, `frames` long, at 48 kHz.
fn signal(channels: usize, frames: usize, frequency: f64) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            let frequency = frequency * (c + 1) as f64;
            (0..frames)
                .map(|i| {
                    let t = i as f64 / 48_000.0;
                    let click = if i.is_multiple_of(1_000) { 0.25 } else { 0.0 };
                    (std::f64::consts::TAU * frequency * t).sin() as f32 * 0.5 + click
                })
                .collect()
        })
        .collect()
}

/// The test plugins, copied into `dir` for the test `test`, so it gets the
/// library's statics to itself, and what a scan finds of them.
fn plugins(dir: &Path, test: &str) -> (Library, Vec<PluginInfo>) {
    let library = Library::copy(dir, &format!("routing-{test}-clap"));
    let installed = scan::scan_here(library.path()).unwrap();
    (library, installed)
}

/// The song: a kick from a WAV file at -6 dB, panned left a little; a bass
/// from another, through Test Gain with latency, which makes the kick late
/// too; and Print, an audio track that takes the input `input` gives from
/// the kick's id, if any. The files go into `dir`.
fn song(dir: &Path, input: impl FnOnce(TrackId) -> Option<Input>) -> (Project, [TrackId; 3]) {
    for (name, channels, frequency) in [("Kick", 2, 60.0), ("Bass", 1, 110.0)] {
        let audio = signal(channels, SONG as usize + 10_000, frequency);
        let planar: Vec<&[f32]> = audio.iter().map(Vec::as_slice).collect();
        let path = dir.join(format!("Audio/{name}.wav"));
        save_wav(&path, 48_000, SampleFormat::Float32, &planar).unwrap();
    }
    let mut p = Project::new();
    let whole = BeatRange::new(Beats::ZERO, Beats::new(4));
    let kick = p.add_track("Kick", TrackKind::Audio);
    let clip = p.new_clip(
        "",
        whole,
        ClipContent::Audio(AudioClip::new("Audio/Kick.wav")),
    );
    let track = p.track_mut(kick).unwrap();
    track.clips.push(clip);
    track.gain = 0.5;
    track.pan = -0.25;

    let bass = p.add_track("Bass", TrackKind::Audio);
    let clip = p.new_clip(
        "",
        whole,
        ClipContent::Audio(AudioClip::new("Audio/Bass.wav")),
    );
    let mut settings = PluginSettings::default();
    settings.set_param(GAIN, Some(0.5));
    settings.set_param(LATENCY, Some(LATE as f64));
    let gain = p.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: GAIN_ID.to_str().unwrap().to_owned(),
        name: "Test Gain".into(),
        kind: PluginKind::AudioEffect,
        inputs: Vec::new(),
        note_output: false,
        note_timing: musicable_model::plugin::NoteTiming::WithSound,
        note_timing_chosen: false,
        settings,
    }));
    let track = p.track_mut(bass).unwrap();
    track.clips.push(clip);
    track.devices.push(gain);

    let print = p.add_track("Print", TrackKind::Audio);
    p.track_mut(print).unwrap().input = input(kick);
    (p, [kick, bass, print])
}

/// Plays `project` live from the start of the song, as the app does on a
/// device, a period at a time, with its plugins loaded from `installed`,
/// recording if `record` says so and holding ten seconds for Capture, once
/// `set_up` has run on the session. The transport stops at the song's end,
/// and Capture is asked for, unless it recorded, and it plays on until
/// `track` has a take. Returns the take's file, interleaved.
fn take(
    project: &Project,
    dir: &Path,
    installed: &[PluginInfo],
    (record, track): (bool, TrackId),
    set_up: impl FnOnce(&mut Session),
) -> Vec<f32> {
    let taken = recorded(project, dir, installed, (record, track), set_up);
    let name = format!("{} 1.wav", taken.track(track).unwrap().name);
    read(&dir.join("Recordings").join(name))
}

/// Plays `project` as [`take`] does, and returns the project once `track`
/// has its take.
fn recorded(
    project: &Project,
    dir: &Path,
    installed: &[PluginInfo],
    (record, track): (bool, TrackId),
    set_up: impl FnOnce(&mut Session),
) -> Project {
    let (mut engine, handle, _) = musicable_app::engine(CONFIG);
    let keymap = Keymap::default_keymap();
    let mut session = Session::offline(project.clone(), Some(dir), keymap, handle);
    session.set_installed_plugins(installed.to_vec());
    session.set_capture_seconds(10);
    // A block before, so the session plays first.
    engine.process_interleaved(&mut [0.0; 2 * PERIOD], 2);
    session.tick();
    set_up(&mut session);
    let start = if record {
        &transport::RECORD
    } else {
        &transport::PLAY
    };
    session.run(start).unwrap();
    // The transport starts with the next block.
    session.stop_at(engine.stats().time + SONG);
    session.wait_for_graph();
    let mut block = vec![0.0; 2 * PERIOD];
    let mut asked = record;
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.project().track(track).unwrap().takes.is_empty() {
        assert!(
            Instant::now() < deadline,
            "no take after twenty seconds: {:?}",
            session.take_problems()
        );
        engine.process_interleaved(&mut block, 2);
        session.tick();
        if !asked && !engine.stats().playing {
            session.capture();
            asked = true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(engine.stats().latency, LATE);
    assert_eq!(session.take_problems(), Vec::<String>::new());
    let project = session.project().clone();
    // The engine goes before the session, and its plugins.
    drop(engine);
    project
}

/// Exports `project`'s song, with stems of `stems`, into `dir` as floats,
/// without a tail, and returns the files, interleaved: the mixdown, then
/// the stems.
fn export(
    project: &Project,
    dir: &Path,
    installed: &[PluginInfo],
    stems: &[TrackId],
) -> Vec<Vec<f32>> {
    let settings = ExportSettings {
        format: FileFormat::Wav,
        bits: Bits::Float32,
        dither: false,
        tail: false,
        stems: !stems.is_empty(),
        ..ExportSettings::default()
    };
    let stems: BTreeSet<TrackId> = stems.iter().copied().collect();
    let file = dir.join("Song.wav");
    let (mixdown, stems) = files_for(&file, project, &settings, &stems);
    let job = Job {
        project: project.clone(),
        folder: Some(dir.to_owned()),
        range: Span::Song.range(project, &Editor::default()).unwrap(),
        settings,
        config: CONFIG,
        installed: installed.to_vec(),
        waker: None,
        mixdown,
        stems,
    };
    let mut export = Export::start(job).unwrap();
    while !export.is_finished() {
        export.tick();
        thread::sleep(Duration::from_millis(1));
    }
    let report = export.report().unwrap().clone();
    assert_eq!(report.failed, None);
    report.files.iter().map(|file| read(&file.path)).collect()
}

/// The stereo file at `path`, interleaved.
fn read(path: &Path) -> Vec<f32> {
    let planar = Decoder::open(path).unwrap().read_all().unwrap();
    assert_eq!(planar.len(), 2, "{}", path.display());
    (0..planar[0].len())
        .flat_map(|i| [planar[0][i], planar[1][i]])
        .collect()
}

/// Where `got` first differs from `expected`, to the bit, by frame.
#[track_caller]
fn assert_same(got: &[f32], expected: &[f32], what: &str) {
    assert_eq!(got.len(), expected.len(), "{what}: the lengths differ");
    let first = got
        .iter()
        .zip(expected)
        .position(|(a, b)| a.to_bits() != b.to_bits());
    if let Some(i) = first {
        panic!(
            "{what}: frame {} differs: {} where the export has {}",
            i / 2,
            got[i],
            expected[i]
        );
    }
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

/// The input from the kick's Post Fader.
fn post_fader(kick: TrackId) -> Input {
    Input::Tap(Tap::PostFader(Chain::Track(kick)))
}

#[test]
fn a_track_recording_another_tracks_post_fader_prints_its_stem() {
    let dir = folder("print");
    let (library, installed) = plugins(&dir, "print");
    let (song, [kick, _, print]) = song(&dir, |_| None);
    let printed = take(&song, &dir, &installed, (true, print), |session| {
        // Chosen in the mixer, which has it play the kick.
        let tap = Tap::PostFader(Chain::Track(kick));
        let args = mixer::with_input_tap(Args::new().with("track", print), tap);
        session.run_with(&mixer::SET_INPUT, &args).unwrap();
        let track = session.project().track(print).unwrap();
        assert_eq!(track.input, Some(post_fader(kick)));
        assert_eq!(track.monitor, Monitor::In);
        let args = Args::new().with("track", print).with("armed", true);
        session.run_with(&mixer::SET_ARM, &args).unwrap();
    });
    assert_eq!(printed.len(), 2 * SONG as usize);
    assert!(printed.iter().any(|&sample| sample != 0.0));
    let exported = export(&song, &dir, &installed, &[kick]);
    assert_same(&printed, &exported[1], "the print of the kick");
    library.assert_alright();
}

#[test]
fn capture_takes_what_a_track_took_from_another() {
    let dir = folder("capture");
    let (library, installed) = plugins(&dir, "capture");
    let (mut song, [kick, _, print]) = song(&dir, |kick| Some(post_fader(kick)));
    // Not armed: Capture holds it as it monitors In.
    song.track_mut(print).unwrap().monitor = Monitor::In;
    let captured = take(&song, &dir, &installed, (false, print), |_| {});
    let exported = export(&song, &dir, &installed, &[kick]);
    assert_same(&captured, &exported[1], "the capture of the kick");
    library.assert_alright();
}

#[test]
fn a_track_resampling_the_master_records_the_mix_without_itself() {
    let dir = folder("resampling");
    let (library, installed) = plugins(&dir, "resampling");
    let (mut song, [.., print]) = song(&dir, |_| Some(Input::RESAMPLING));
    // A tone of its own, which it plays but doesn't record.
    let track = song.track_mut(print).unwrap();
    track.armed = true;
    track.tone = Some(TestTone {
        frequency: 440.0,
        level: 0.25,
    });
    let resampled = take(&song, &dir, &installed, (true, print), |_| {});
    song.track_mut(print).unwrap().muted = true;
    let exported = export(&song, &dir, &installed, &[]);
    assert_same(&resampled, &exported[0], "the master, resampled");
    library.assert_alright();
}

/// The song, with the Lead, a MIDI track playing a riff, which moves the
/// mod wheel, and the Double, a MIDI track for taking its notes, with the
/// bass's latency in the graph. The files go into `dir`.
fn riff(dir: &Path) -> (Project, MidiClip, [TrackId; 2]) {
    let (mut song, _) = song(dir, |_| None);
    let lead = song.add_track("Lead", TrackKind::Midi);
    let mut riff = MidiClip::default();
    riff.add_note(Beats::from_f64(0.5), Beats::new(1), 60, 0.75);
    riff.add_note(Beats::new(2), Beats::from_f64(1.5), 64, 0.5)
        .release_velocity = 0.25;
    let wheel = vec![
        Point::new(Beats::new(1), 0.25),
        Point::new(Beats::new(3), 0.75),
    ];
    riff.set_controller(Controller::MOD_WHEEL, wheel);
    let whole = BeatRange::new(Beats::ZERO, Beats::new(4));
    let clip = song.new_clip("Riff", whole, ClipContent::Midi(riff.clone()));
    song.track_mut(lead).unwrap().clips.push(clip);
    let double = song.add_track("Double", TrackKind::Midi);
    (song, riff, [lead, double])
}

/// Whether `track`'s one clip in `project`, over the song, has the riff's
/// notes and its mod wheel.
#[track_caller]
fn assert_doubled(project: &Project, track: TrackId, riff: &MidiClip) {
    let track = project.track(track).unwrap();
    assert_eq!(track.clips.len(), 1);
    let clip = &track.clips[0];
    assert_eq!((clip.start, clip.end()), (Beats::ZERO, Beats::new(4)));
    let ClipContent::Midi(midi) = &clip.content else {
        panic!("{clip:?}");
    };
    let notes = |clip: &MidiClip| -> Vec<_> {
        (clip.notes.iter())
            .map(|n| (n.start, n.length, n.key, n.velocity, n.release_velocity))
            .collect()
    };
    assert_eq!(notes(midi), notes(riff));
    let wheel = Controller::MOD_WHEEL;
    assert_eq!(midi.controller(wheel), riff.controller(wheel));
}

#[test]
fn a_midi_track_recording_anothers_notes_records_its_clips_notes() {
    let dir = folder("notes");
    let (library, installed) = plugins(&dir, "notes");
    let (song, riff, [lead, double]) = riff(&dir);
    let taken = recorded(&song, &dir, &installed, (true, double), |session| {
        // Chosen in the mixer, which has it play the Lead's notes.
        let args = Args::new().with("track", double).with("from", lead);
        session.run_with(&mixer::SET_INPUT, &args).unwrap();
        let track = session.project().track(double).unwrap();
        assert_eq!(track.input, Some(Input::Tap(Tap::PreFx(lead))));
        assert_eq!(track.monitor, Monitor::In);
        let args = Args::new().with("track", double).with("armed", true);
        session.run_with(&mixer::SET_ARM, &args).unwrap();
    });
    assert_doubled(&taken, double, &riff);
    library.assert_alright();
}

#[test]
fn capture_takes_the_notes_a_midi_track_took_from_another() {
    let dir = folder("captured-notes");
    let (library, installed) = plugins(&dir, "captured-notes");
    let (mut song, riff, [lead, double]) = riff(&dir);
    // Not armed: Capture holds it as it monitors In.
    let track = song.track_mut(double).unwrap();
    track.input = Some(Input::Tap(Tap::PreFx(lead)));
    track.monitor = Monitor::In;
    let taken = recorded(&song, &dir, &installed, (false, double), |_| {});
    assert_doubled(&taken, double, &riff);
    library.assert_alright();
}

#[test]
fn a_midi_track_records_what_an_arpeggiator_plays() {
    let dir = folder("arpeggiated");
    let (library, installed) = plugins(&dir, "arpeggiated");
    let (mut song, _, [lead, double]) = riff(&dir);
    let arpeggiator = song.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: ARPEGGIATOR_ID.to_str().unwrap().to_owned(),
        name: "Test Arpeggiator".into(),
        kind: PluginKind::MidiEffect,
        inputs: Vec::new(),
        note_output: false,
        note_timing: musicable_model::plugin::NoteTiming::WithSound,
        note_timing_chosen: false,
        settings: PluginSettings::default(),
    }));
    let id = arpeggiator.id;
    song.track_mut(lead).unwrap().devices.push(arpeggiator);
    let taken = recorded(&song, &dir, &installed, (true, double), |session| {
        let args = Args::new().with("track", double).with("after", id);
        session.run_with(&mixer::SET_INPUT, &args).unwrap();
        let track = session.project().track(double).unwrap();
        assert_eq!(track.input, Some(Input::Tap(Tap::Device(id))));
        let args = Args::new().with("track", double).with("armed", true);
        session.run_with(&mixer::SET_ARM, &args).unwrap();
    });
    // Each of the riff's keys again and again while it's held, a step of an
    // eighth of a second each, a quarter of a beat at 120 BPM, at its
    // velocity: C from half a beat for a beat, and E from beat 2 for a beat
    // and a half.
    let step = ARP_STEP * 2.0;
    let steps = |from: f64, count: usize, key: u8, velocity: f32| {
        (0..count).map(move |i| {
            let start = Beats::from_f64(from + step * i as f64);
            (start, Beats::from_f64(step), key, velocity)
        })
    };
    let expected: Vec<_> = (steps(0.5, 4, 60, 0.75))
        .chain(steps(2.0, 6, 64, 0.5))
        .collect();
    let track = taken.track(double).unwrap();
    assert_eq!(track.clips.len(), 1);
    let ClipContent::Midi(midi) = &track.clips[0].content else {
        panic!("{:?}", track.clips[0]);
    };
    let notes: Vec<_> = (midi.notes.iter())
        .map(|n| (n.start, n.length, n.key, n.velocity))
        .collect();
    assert_eq!(notes, expected);
    library.assert_alright();
}

#[test]
fn a_midi_track_records_what_a_synth_plays_out() {
    let dir = folder("echoed");
    // Named so, the library's synth plays each note it gets out again as
    // it renders, an octave up, on the same frame.
    let library = Library::copy(&dir, "echo-routing-clap");
    let installed = scan::scan_here(library.path()).unwrap();
    let (mut song, riff, [lead, double]) = riff(&dir);
    let synth = song.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: SYNTH_ID.to_str().unwrap().to_owned(),
        name: "Test Synth".into(),
        kind: PluginKind::Instrument,
        inputs: Vec::new(),
        // As the project was saved, once the plugin had said it has one.
        note_output: true,
        note_timing: musicable_model::plugin::NoteTiming::WithSound,
        note_timing_chosen: false,
        settings: PluginSettings::default(),
    }));
    let id = synth.id;
    song.track_mut(lead).unwrap().devices.push(synth);
    let taken = recorded(&song, &dir, &installed, (true, double), |session| {
        let args = Args::new().with("track", double).with("after", id);
        session.run_with(&mixer::SET_INPUT, &args).unwrap();
        let args = Args::new().with("track", double).with("armed", true);
        session.run_with(&mixer::SET_ARM, &args).unwrap();
    });
    // The riff's notes an octave up, where they were, with their velocities
    // and release velocities. The synth plays out no mod wheel.
    let up = |key: u8| key + ECHO_UP as u8;
    let notes = |clip: &MidiClip| -> Vec<_> {
        (clip.notes.iter())
            .map(|n| (n.start, n.length, n.key, n.velocity, n.release_velocity))
            .collect()
    };
    let expected: Vec<_> = (notes(&riff).into_iter())
        .map(|(start, length, key, velocity, release)| (start, length, up(key), velocity, release))
        .collect();
    let track = taken.track(double).unwrap();
    assert_eq!(track.clips.len(), 1);
    let ClipContent::Midi(midi) = &track.clips[0].content else {
        panic!("{:?}", track.clips[0]);
    };
    assert_eq!(notes(midi), expected);
    assert_eq!(midi.controller(Controller::MOD_WHEEL), []);
    library.assert_alright();
}
