//! Exports, as the export window starts them: the project rendered offline,
//! on a thread of its own, to files. An exported mixdown is what the app
//! plays live, to the sample, with a latent plugin, a built-in device,
//! automation, audio files at another rate and a clip in Spectral, which
//! the export waits for the segments of; each stem is its track
//! soloed, and with solo on the mix is as heard. The tail renders a
//! release and leaves the quiet end off, normalising takes the peak to full
//! scale, and FLAC and 16-bit files hold the same samples, rounded. A
//! Bypass lane switches on the sample it asks for. A muted kick wired into
//! a CLAP or a VST3 plugin's side input ducks a bass as it does live,
//! whichever track is late. A cancelled export, or one that fails, leaves
//! no files behind.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use musicable_app::Session;
use musicable_app::export::{
    Bits, Export, ExportSettings, FileFormat, Job, Report, Span, Stage, files_for,
};
use musicable_commands::{Editor, Keymap, transport};
use musicable_engine::{EngineConfig, rt};
use musicable_io::audio::{Decoder, SampleFormat, save_wav, write_aiff};
use musicable_model::automation::{Lane, Point};
use musicable_model::clip::{AudioClip, ClipContent, MidiClip, Offset, Warping};
use musicable_model::native::{NativeDevice, NativeKind, compressor};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings};
use musicable_model::routing::{Port, Tap};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{Warp, WarpMode};
use musicable_model::{
    Chain, Device, DeviceKind, PluginFormat, Project, TestTone, TrackId, TrackKind,
};
use musicable_plugins::PluginInfo;
use musicable_plugins::scan;
use musicable_test_plugins::testing::Library;
use musicable_test_plugins::vst3::{VST3_GAIN_ID, VST3_SIDECHAIN};
use musicable_test_plugins::{GAIN, GAIN_ID, LATENCY, SIDECHAIN};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 512,
};

/// The frames a device asks for at a time: the RME's period, which the
/// engine's blocks don't divide.
const PERIOD: usize = 441;

/// Test Gain's latency on the bass, in frames.
const BASS_LATENCY: u64 = 300;

/// How many frames apart a kick's clicks are, and how long each is.
const BEAT: usize = 4_800;
const CLICK: usize = 10;

/// The latency of the Test Gain that makes a track late, in frames, where a
/// kick ducks a bass.
const LATE: u64 = 120;

/// A folder of its own for each test, made fresh.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("musicable-export-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("Audio")).unwrap();
    dir
}

/// A test signal: a sine a channel, with a click every 1,000 frames.
fn signal(channels: usize, frames: usize, rate: u32) -> Vec<Vec<f32>> {
    (0..channels)
        .map(|c| {
            let frequency = 150.0 * (c + 1) as f64;
            (0..frames)
                .map(|i| {
                    let t = i as f64 / f64::from(rate);
                    let click = if i.is_multiple_of(1_000) { 0.25 } else { 0.0 };
                    (std::f64::consts::TAU * frequency * t).sin() as f32 * 0.5 + click
                })
                .collect()
        })
        .collect()
}

fn planar(channels: &[Vec<f32>]) -> Vec<&[f32]> {
    channels.iter().map(Vec::as_slice).collect()
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

fn audio_clip(project: &mut Project, track: TrackId, from: Beats, to: Beats, audio: AudioClip) {
    let clip = project.new_clip("", BeatRange::new(from, to), ClipContent::Audio(audio));
    project.track_mut(track).unwrap().clips.push(clip);
}

/// The test plugins, copied into `dir` for the test `test`, so it gets the
/// library's statics to itself, and what a scan finds of them.
fn plugins(dir: &Path, test: &str) -> (Library, Vec<PluginInfo>) {
    let library = Library::copy(dir, &format!("export-{test}-clap"));
    let installed = scan::scan_here(library.path()).unwrap();
    (library, installed)
}

/// Four beats at 120 BPM, two seconds: drums from a WAV file through the
/// compressor, their fader automated, a bass line from an AIFF file at
/// 44.1 kHz through Test Gain with latency and its gain automated, chords
/// on a test instrument, a test tone, panned and muted, and the drums again
/// in Spectral, faster and five semitones down. The audio files go into
/// `dir`.
fn song(dir: &Path) -> Project {
    let drums = signal(2, 60_000, 48_000);
    let path = dir.join("Audio/Drums.wav");
    save_wav(&path, 48_000, SampleFormat::Int24, &planar(&drums)).unwrap();
    let bass = signal(1, 60_000, 44_100);
    let mut file = BufWriter::new(File::create(dir.join("Audio/Bass.aif")).unwrap());
    write_aiff(&mut file, 44_100, SampleFormat::Int16, &planar(&bass)).unwrap();
    drop(file);

    let mut p = Project::new();
    let drums = p.add_track("Drums", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Drums.wav");
    audio.gain = 0.5;
    audio.fade_out = beats(1, 4);
    audio_clip(&mut p, drums, beats(0, 1), beats(2, 1), audio);
    let mut audio = AudioClip::new("Audio/Drums.wav");
    audio.offset = Offset::Frames(10_000);
    audio_clip(&mut p, drums, beats(5, 2), beats(4, 1), audio);
    let mut squash = NativeDevice::new(NativeKind::Compressor);
    squash.set(compressor::THRESHOLD, -24.0);
    let squash = p.new_device(DeviceKind::Native(squash));
    p.track_mut(drums).unwrap().devices.push(squash);
    let down_and_up = vec![
        Point::new(beats(0, 1), 1.0),
        Point::new(beats(3, 2), 0.25),
        Point::new(beats(3, 1), 1.0),
    ];
    let fader = ParamAddress::Volume(Chain::Track(drums));
    p.automation.push(Lane::new(fader, down_and_up));

    let bass = p.add_track("Bass", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Bass.aif");
    audio.gain = 0.7;
    audio_clip(&mut p, bass, beats(1, 2), beats(7, 2), audio);
    let mut settings = PluginSettings::default();
    settings.set_param(GAIN, Some(0.5));
    settings.set_param(LATENCY, Some(BASS_LATENCY as f64));
    let gain = p.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: GAIN_ID.to_str().unwrap().to_owned(),
        name: "Test Gain".into(),
        kind: PluginKind::AudioEffect,
        inputs: Vec::new(),
        note_output: false,
        settings,
    }));
    let up = vec![Point::new(beats(1, 1), 0.5), Point::new(beats(3, 1), 1.0)];
    p.automation
        .push(Lane::new(ParamAddress::Device(gain.id, GAIN), up));
    let track = p.track_mut(bass).unwrap();
    track.devices.push(gain);
    track.pan = 0.3;

    let keys = p.add_track("Keys", TrackKind::Midi);
    let mut chords = MidiClip::default();
    for (i, key) in [60, 64, 67].into_iter().enumerate() {
        chords.add_note(beats(i as i64, 2), beats(3, 2), key, 0.6);
    }
    let clip = p.new_clip(
        "Chords",
        BeatRange::new(Beats::ZERO, beats(4, 1)),
        ClipContent::Midi(chords),
    );
    let instrument = p.new_device(DeviceKind::TestInstrument { level: 0.2 });
    let track = p.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    track.gain = 0.8;

    let tone = p.add_track("Tone", TrackKind::Audio);
    let track = p.track_mut(tone).unwrap();
    track.tone = Some(TestTone {
        frequency: 330.0,
        level: 0.1,
    });
    track.pan = -0.5;
    track.muted = true;

    let voice = p.add_track("Voice", TrackKind::Audio);
    let mut audio = AudioClip::new("Audio/Drums.wav");
    let mut warp = Warp::new(0, Beats::ZERO, 100.0);
    warp.mode = WarpMode::Spectral;
    warp.transpose = -5;
    let warping = Warping {
        offset: Offset::Beats(Beats::ZERO),
        warp: Some(warp),
    };
    assert!(audio.set_warping(warping));
    audio.gain = 0.5;
    audio_clip(&mut p, voice, beats(1, 1), beats(3, 1), audio);
    p
}

/// What the app plays of `project`, live, from the start of the song: the
/// app's engine, asked for a device's period at a time, with the session
/// ticking in between, its plugins loaded from `installed`. Returns the
/// `frames` frames the graph's latency delays the song by, and then
/// `frames` frames, interleaved.
fn live(
    project: &Project,
    folder: &Path,
    installed: &[PluginInfo],
    config: EngineConfig,
    frames: usize,
) -> (u64, Vec<f32>) {
    let (mut engine, handle, _) = musicable_app::engine(config);
    let keymap = Keymap::default_keymap();
    let mut session = Session::offline(project.clone(), Some(folder), keymap, handle);
    session.set_installed_plugins(installed.to_vec());
    session.run(&transport::PLAY).unwrap();
    session.wait_for_graph();
    let mut output = Vec::new();
    let mut block = vec![0.0; 2 * PERIOD];
    let mut latency = None;
    while output.len() < 2 * frames + 2 * latency.unwrap_or(0) as usize {
        engine.process_interleaved(&mut block, 2);
        session.tick();
        output.extend_from_slice(&block);
        latency = latency.or(Some(engine.stats().latency));
    }
    let latency = latency.unwrap();
    let start = 2 * latency as usize;
    output.truncate(start + 2 * frames);
    assert_eq!(session.take_problems(), Vec::<String>::new());
    // The engine goes before the session, and its plugins.
    drop(engine);
    (latency, output.split_off(start))
}

/// Settings for files that hold what's rendered as it is: floats, without
/// dither or a tail.
fn exact() -> ExportSettings {
    ExportSettings {
        format: FileFormat::Wav,
        bits: Bits::Float32,
        dither: false,
        tail: false,
        ..ExportSettings::default()
    }
}

/// An export of the song, or of four beats if it has no clips, to `file`
/// with `settings`, the stems wanted those of `stems`.
fn job(
    project: &Project,
    folder: &Path,
    installed: &[PluginInfo],
    settings: ExportSettings,
    file: &Path,
    stems: &BTreeSet<TrackId>,
) -> Job {
    let (mixdown, stems) = files_for(file, project, &settings, stems);
    Job {
        project: project.clone(),
        folder: Some(folder.to_owned()),
        range: Span::Song
            .range(project, &Editor::default())
            .unwrap_or(BeatRange::new(Beats::ZERO, Beats::new(4))),
        settings,
        config: CONFIG,
        installed: installed.to_vec(),
        waker: None,
        mixdown,
        stems,
    }
}

/// Carries `job` out, ticking as the UI would, and says how it went.
fn export(job: Job) -> Report {
    let mut export = Export::start(job).unwrap();
    while !export.is_finished() {
        export.tick();
        thread::sleep(Duration::from_millis(1));
    }
    export.report().unwrap().clone()
}

/// The file at `path`, interleaved, and its sample rate.
fn read(path: &Path) -> (Vec<f32>, u32) {
    let mut decoder = Decoder::open(path).unwrap();
    let info = decoder.info();
    assert_eq!(info.channels, 2);
    let planar = decoder.read_all().unwrap();
    let samples = (0..planar[0].len())
        .flat_map(|i| [planar[0][i], planar[1][i]])
        .collect();
    (samples, info.sample_rate)
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
            "{what}: frame {} differs: {} where the live render has {}",
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

#[test]
fn an_exported_mixdown_is_what_the_app_plays_to_the_sample() {
    let dir = folder("mixdown");
    let (library, installed) = plugins(&dir, "mixdown");
    let song = song(&dir);
    let file = dir.join("Song.wav");
    let report = export(job(
        &song,
        &dir,
        &installed,
        exact(),
        &file,
        &BTreeSet::new(),
    ));
    assert_eq!((report.failed.as_deref(), report.cancelled), (None, false));
    assert_eq!(report.problems, Vec::<String>::new());
    let length = 96_000;
    let [written] = &report.files[..] else {
        panic!("one file: {:?}", report.files);
    };
    assert_eq!((&written.path, written.frames), (&file, length));

    let (latency, played) = live(&song, &dir, &installed, CONFIG, length as usize);
    assert_eq!(latency, BASS_LATENCY, "the bass delays the rest");
    let (exported, rate) = read(&file);
    assert_eq!(rate, 48_000);
    assert_same(&exported, &played, "the mixdown");
    let peak = played.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert_eq!(written.peak, peak);
    assert!(peak > 0.1, "it isn't silent: {peak}");
    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
    library.assert_alright();
}

#[test]
fn a_bypass_lane_switches_on_the_sample_it_asks_for() {
    let dir = folder("bypass");
    let (library, installed) = plugins(&dir, "bypass");
    // The bass alone, through Test Gain, which halves it 300 frames late.
    let mut song = song(&dir);
    song.tracks.retain(|track| track.name == "Bass");
    let gain = song.tracks[0].devices[0].id;
    song.automation
        .retain(|lane| lane.param.device() == Some(gain));
    let render = |song: &Project, name: &str| {
        let file = dir.join(format!("{name}.wav"));
        let settings = exact();
        let report = export(job(
            song,
            &dir,
            &installed,
            settings,
            &file,
            &BTreeSet::new(),
        ));
        assert_eq!((report.failed.as_deref(), report.cancelled), (None, false));
        assert_eq!(report.problems, Vec::<String>::new());
        read(&file).0
    };
    let playing = render(&song, "Playing");
    let mut bypassed = song.clone();
    bypassed.tracks[0].devices[0].bypassed = true;
    let bypassed = render(&bypassed, "Bypassed");
    assert_ne!(playing, bypassed);
    // Bypassed from beat 2 on.
    let at = |beat: i64, value: f64| Point::new(Beats::new(beat), value);
    let steps = vec![at(0, 0.0), at(2, 0.0), at(2, 1.0)];
    song.automation
        .push(Lane::new(ParamAddress::Bypass(gain), steps));
    let switched = render(&song, "Switched");

    // The fade starts on the frame of the step, as the plugin's latency is
    // made up, and takes 10 ms.
    let step = song.tempo.frame_at(Beats::new(2), CONFIG.sample_rate) as usize;
    let fade = 480;
    assert_eq!(
        (playing.len(), bypassed.len()),
        (switched.len(), switched.len())
    );
    for (i, &got) in switched.iter().enumerate() {
        let over = match (i / 2).checked_sub(step) {
            None => 0.0,
            Some(k) => ((k + 1).min(fade)) as f32 / fade as f32,
        };
        let due = playing[i] * (1.0 - over) + bypassed[i] * over;
        if (got - due).abs() > 1e-6 {
            panic!("{got} on frame {} where {due} was due", i / 2);
        }
    }
    // As the app plays it.
    let (_, played) = live(&song, &dir, &installed, CONFIG, switched.len() / 2);
    assert_same(&switched, &played, "the bass");
    library.assert_alright();
}

/// Test Gain in `format`, `latency` frames late, its device listing
/// `inputs`.
fn test_gain(
    project: &mut Project,
    format: PluginFormat,
    latency: u64,
    inputs: Vec<PluginInput>,
) -> Device {
    let (id, latency) = match format {
        PluginFormat::Clap => (GAIN_ID.to_str().unwrap(), latency as f64),
        PluginFormat::Vst3 => (VST3_GAIN_ID, latency as f64 / 48_000.0),
    };
    let mut settings = PluginSettings::default();
    settings.set_param(LATENCY, Some(latency));
    project.new_device(DeviceKind::Plugin(PluginDevice {
        format,
        id: id.to_owned(),
        name: "Test Gain".into(),
        kind: PluginKind::AudioEffect,
        inputs,
        note_output: false,
        settings,
    }))
}

/// Two beats at 120 BPM: a kick that clicks at full scale, muted, and a bass
/// that holds a quarter, through Test Gain in `format`, whose side input
/// the kick keys, so the bass goes silent where the kick clicks. A Test
/// Gain on each track makes the one called `late` late. As the app saves
/// it once the plugins have loaded: the ducker lists its side input, and
/// the wire goes into it. The audio files go into `dir`.
fn ducking(dir: &Path, format: PluginFormat, late: &str) -> Project {
    let clicks: Vec<f32> = (0..48_000)
        .map(|i| if i % BEAT < CLICK { 1.0 } else { 0.0 })
        .collect();
    let float = SampleFormat::Float32;
    save_wav(&dir.join("Audio/Kick.wav"), 48_000, float, &[&clicks]).unwrap();
    save_wav(
        &dir.join("Audio/Bass.wav"),
        48_000,
        float,
        &[&[0.25; 48_000]],
    )
    .unwrap();
    let mut p = Project::new();
    let kick = p.add_track("Kick", TrackKind::Audio);
    let bass = p.add_track("Bass", TrackKind::Audio);
    for (track, name) in [(kick, "Kick"), (bass, "Bass")] {
        let audio = AudioClip::new(format!("Audio/{name}.wav"));
        audio_clip(&mut p, track, Beats::ZERO, beats(2, 1), audio);
        let latency = if name == late { LATE } else { 0 };
        let gain = test_gain(&mut p, format, latency, Vec::new());
        p.track_mut(track).unwrap().devices.push(gain);
    }
    let id = match format {
        PluginFormat::Clap => SIDECHAIN,
        PluginFormat::Vst3 => VST3_SIDECHAIN,
    };
    let sidechain = PluginInput {
        id,
        name: "Sidechain".into(),
    };
    let ducker = test_gain(&mut p, format, 0, vec![sidechain]);
    let into = Port::PluginInput(ducker.id, id);
    let wire = p.new_wire(Tap::PostFader(Chain::Track(kick)), into);
    p.wires.push(wire);
    p.track_mut(bass).unwrap().devices.push(ducker);
    p.track_mut(kick).unwrap().muted = true;
    p
}

#[test]
fn a_muted_kick_ducks_a_bass_through_a_plugins_side_input_as_it_does_live() {
    let dir = folder("sidechain");
    let (clap, mut installed) = plugins(&dir, "sidechain");
    let vst3 = Library::copy_vst3(&dir, "export-sidechain-vst3");
    installed.extend(scan::scan_here(vst3.path()).unwrap());
    // The bass, silent where the kick clicks, and lined up with it.
    let ducked: Vec<f32> = (0..48_000)
        .flat_map(|i| [if i % BEAT < CLICK { 0.0 } else { 0.25 }; 2])
        .collect();
    for format in [PluginFormat::Clap, PluginFormat::Vst3] {
        for late in ["Kick", "Bass"] {
            let what = format!("{format:?}, the {} late", late.to_lowercase());
            let song = ducking(&dir, format, late);
            let file = dir.join(format!("{format:?} {late}.wav"));
            let report = export(job(
                &song,
                &dir,
                &installed,
                exact(),
                &file,
                &BTreeSet::new(),
            ));
            assert_eq!((report.failed.as_deref(), report.cancelled), (None, false));
            assert_eq!(report.problems, Vec::<String>::new());
            let (latency, played) = live(&song, &dir, &installed, CONFIG, 48_000);
            assert_eq!(latency, LATE, "{what}");
            if let Some(i) = played.iter().zip(&ducked).position(|(a, b)| a != b) {
                let (got, due) = (played[i], ducked[i]);
                panic!("{what}: frame {} plays {got} where {due} was due", i / 2);
            }
            assert_same(&read(&file).0, &played, &what);
        }
    }
    clap.assert_alright();
    vst3.assert_alright();
}

#[test]
fn stems_are_their_tracks_soloed_and_with_solo_on_the_mix_is_as_heard() {
    let dir = folder("stems");
    let (library, installed) = plugins(&dir, "stems");
    let mut song = song(&dir);
    song.tracks[0].solo = true;
    let tracks: BTreeSet<TrackId> = song.tracks.iter().map(|track| track.id).collect();
    let settings = ExportSettings {
        stems: true,
        ..exact()
    };
    let file = dir.join("Song.wav");
    let report = export(job(&song, &dir, &installed, settings, &file, &tracks));
    assert_eq!(report.failed, None);
    let names: Vec<String> = report
        .files
        .iter()
        .map(|written| written.path.file_name().unwrap().to_string_lossy().into())
        .collect();
    assert_eq!(
        names,
        [
            "Song.wav",
            "Song 01 Drums.wav",
            "Song 02 Bass.wav",
            "Song 03 Keys.wav",
            "Song 04 Tone.wav",
            "Song 05 Voice.wav",
        ]
    );
    let length = 96_000;

    // The mix as it's heard: the drums, soloed.
    let (_, played) = live(&song, &dir, &installed, CONFIG, length);
    assert_same(&read(&file).0, &played, "the mixdown");

    // Each stem is its track on its own, the muted one too.
    for (n, track) in song.tracks.iter().enumerate() {
        let mut alone = song.clone();
        for other in &mut alone.tracks {
            other.solo = other.id == track.id;
            other.muted = false;
        }
        let (_, played) = live(&alone, &dir, &installed, CONFIG, length);
        let stem = &report.files[n + 1];
        assert!(stem.peak > 0.0, "{} is silent", track.name);
        assert_same(&read(&stem.path).0, &played, &track.name);
    }
    library.assert_alright();
}

#[test]
fn the_tail_holds_the_release_and_leaves_the_quiet_end_off() {
    let dir = folder("tail");
    // A chord that lasts to the end of its clip, and so of the song, then
    // lets go: its release is the tail.
    let mut song = Project::new();
    let keys = song.add_track("Keys", TrackKind::Midi);
    let mut chord = MidiClip::default();
    for key in [57, 60, 64] {
        chord.add_note(beats(1, 1), beats(1, 1), key, 0.8);
    }
    let clip = song.new_clip(
        "Chord",
        BeatRange::new(Beats::ZERO, beats(2, 1)),
        ClipContent::Midi(chord),
    );
    let instrument = song.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let track = song.track_mut(keys).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    let settings = ExportSettings {
        tail: true,
        ..exact()
    };
    let file = dir.join("Tail.wav");
    let report = export(job(&song, &dir, &[], settings, &file, &BTreeSet::new()));
    assert_eq!(report.failed, None);
    let length = 48_000;
    let frames = report.files[0].frames as usize;
    // The release takes 30 ms, and fades below a 16-bit step before then.
    assert!(
        (length + 1_000..=length + 1_440).contains(&frames),
        "{frames} frames"
    );

    // Live, the song plays on into silence: the file is what it played
    // until then.
    let (_, played) = live(&song, &dir, &[], CONFIG, length + 48_000);
    let (exported, _) = read(&file);
    assert_same(&exported, &played[..2 * frames], "the song and its tail");
    let quiet = 1.0 / 65_536.0;
    assert!(exported[2 * frames - 2..].iter().any(|s| s.abs() >= quiet));
    assert!(played[2 * frames..].iter().all(|s| s.abs() < quiet));
}

#[test]
fn normalising_takes_the_peak_to_full_scale() {
    let dir = folder("normalise");
    let (library, installed) = plugins(&dir, "normalise");
    let song = song(&dir);
    let settings = ExportSettings {
        normalize: true,
        ..exact()
    };
    let file = dir.join("Loud.wav");
    let report = export(job(
        &song,
        &dir,
        &installed,
        settings,
        &file,
        &BTreeSet::new(),
    ));
    assert_eq!(report.failed, None);
    let written = &report.files[0];
    let (_, played) = live(&song, &dir, &installed, CONFIG, 96_000);
    let peak = played.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert_eq!(written.peak, peak);
    let gain = written.gain.expect("it normalised");
    assert_eq!(gain, 1.0 / peak);
    let louder: Vec<f32> = played.iter().map(|s| s * gain).collect();
    let (exported, _) = read(&file);
    assert_same(&exported, &louder, "the normalised mixdown");
    let top = exported.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert!((top - 1.0).abs() < 1e-6, "{top}");
    let names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains("Loud"))
        .collect();
    assert_eq!(names, ["Loud.wav"], "the float file is gone");
    library.assert_alright();
}

#[test]
fn flac_aiff_and_16_bit_files_hold_the_samples_rounded() {
    let dir = folder("formats");
    let (library, installed) = plugins(&dir, "formats");
    let song = song(&dir);
    let (_, played) = live(&song, &dir, &installed, CONFIG, 96_000);
    let none = BTreeSet::new();
    let mut exported = Vec::new();
    for (format, bits, dither, name) in [
        (FileFormat::Flac, Bits::Int24, false, "Song.flac"),
        (FileFormat::Aiff, Bits::Int24, false, "Song.aif"),
        (FileFormat::Wav, Bits::Int16, true, "Dithered.wav"),
        (FileFormat::Wav, Bits::Int16, true, "Dithered again.wav"),
    ] {
        let settings = ExportSettings {
            format,
            bits,
            dither,
            ..exact()
        };
        let file = dir.join(name);
        let report = export(job(&song, &dir, &installed, settings, &file, &none));
        assert_eq!(report.failed, None, "{name}");
        exported.push(read(&file).0);
    }
    let step = |bits: i32| 1.0 / 2.0_f32.powi(bits - 1);
    for (samples, bits, within) in [
        (&exported[0], 24, 0.5),
        (&exported[1], 24, 0.5),
        // Dither adds up to a step, either way.
        (&exported[2], 16, 1.5),
    ] {
        assert_eq!(samples.len(), played.len());
        let worst = samples
            .iter()
            .zip(&played)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst <= step(bits) * within * 1.0001, "{bits}: {worst}");
    }
    assert_eq!(exported[0], exported[1], "FLAC and AIFF round alike");
    assert_eq!(exported[2], exported[3], "dither is the same every time");
    library.assert_alright();
}

#[test]
fn files_are_at_the_rate_asked_for_rendered_at_it() {
    let dir = folder("rate");
    let mut song = Project::new();
    let tone = song.add_track("Tone", TrackKind::Audio);
    song.track_mut(tone).unwrap().tone = Some(TestTone {
        frequency: 1_000.0,
        level: 0.25,
    });
    let audio = AudioClip::new("Audio/Drums.wav");
    let drums = signal(2, 60_000, 48_000);
    let path = dir.join("Audio/Drums.wav");
    save_wav(&path, 48_000, SampleFormat::Int24, &planar(&drums)).unwrap();
    audio_clip(&mut song, tone, Beats::ZERO, beats(1, 1), audio);
    let settings = ExportSettings {
        sample_rate: Some(96_000),
        ..exact()
    };
    let file = dir.join("Fast.wav");
    let report = export(job(&song, &dir, &[], settings, &file, &BTreeSet::new()));
    assert_eq!(report.failed, None);
    let (exported, rate) = read(&file);
    assert_eq!((rate, report.files[0].frames), (96_000, 48_000));
    let config = EngineConfig {
        sample_rate: 96_000,
        ..CONFIG
    };
    let (_, played) = live(&song, &dir, &[], config, 48_000);
    assert_same(&exported, &played, "the song at 96 kHz");
}

#[test]
fn a_cancelled_export_leaves_no_files_behind() {
    let dir = folder("cancel");
    // Ten minutes of test tone, which takes a while.
    let mut song = Project::new();
    let tone = song.add_track("Tone", TrackKind::Audio);
    song.track_mut(tone).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.25,
    });
    let file = dir.join("Long.flac");
    let settings = ExportSettings {
        format: FileFormat::Flac,
        ..ExportSettings::default()
    };
    let mut job = job(&song, &dir, &[], settings, &file, &BTreeSet::new());
    job.range = BeatRange::new(Beats::ZERO, Beats::new(1_200));
    let mut export = Export::start(job).unwrap();
    while !matches!(export.stage(), Stage::Rendering { done, .. } if done > 0) {
        assert!(!export.is_finished(), "{:?}", export.report());
        export.tick();
        thread::sleep(Duration::from_millis(1));
    }
    export.cancel();
    let report = export.report().unwrap();
    assert!(report.cancelled);
    assert_eq!(report.files, []);
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "only the Audio folder"
    );
}

#[test]
fn an_export_that_fails_says_why_and_leaves_no_files_behind() {
    let dir = folder("fails");
    let mut song = Project::new();
    let tone = song.add_track("Tone", TrackKind::Audio);
    song.track_mut(tone).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.25,
    });
    let file = dir.join("Nowhere/Song.wav");
    let mut job = job(&song, &dir, &[], exact(), &file, &BTreeSet::new());
    let report = export(job.clone());
    let failed = report.failed.expect("there's no such folder");
    assert!(failed.contains("Song.wav"), "{failed}");
    assert_eq!(report.files, []);

    // Nothing to export isn't an export at all.
    job.mixdown = None;
    assert!(Export::start(job.clone()).is_err());
    job.mixdown = Some(dir.join("Song.wav"));
    job.range = BeatRange::new(Beats::new(4), Beats::new(4));
    assert!(Export::start(job).is_err());
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        1,
        "only the Audio folder"
    );
}
