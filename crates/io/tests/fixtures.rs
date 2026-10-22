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
use musicable_model::clip::{AudioClip, ClipContent, MidiClip, Point};
use musicable_model::mixer::Bus;
use musicable_model::native::{NativeDevice, NativeKind, Unit, eq, sampler, synth, utility};
use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings, PluginState};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::{
    AudioInput, ClipType, DeviceKind, Input, MidiInput, Monitor, PluginFormat, Project, TestTone,
    Track, TrackId, TrackKind,
};

fn fixtures() -> Vec<(&'static str, Project)> {
    vec![
        ("empty", Project::new()),
        ("demo", demo()),
        ("tempo-changes", tempo_changes()),
        ("awkward-values", awkward_values()),
        ("mixer", mixer()),
        ("clips", clips()),
        ("locators", locators()),
        ("plugins", plugins()),
        ("recording", recording()),
        ("built-in", built_in()),
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

/// A mix, from version 2: drums in a group inside another group, sends
/// before and after the fader to two return tracks, pan, mute, solo and
/// solo-safe, an armed track, test devices on tracks and on the master, every
/// clip type, and ids with gaps where sends and devices were deleted.
fn mixer() -> Project {
    let mut project = Project::new();
    let names = [
        ("Kick", TrackKind::Audio, 55.0),
        ("Snare", TrackKind::Audio, 196.0),
        ("Bass", TrackKind::Midi, 82.41),
        ("Vocal", TrackKind::Audio, 440.0),
        ("Drums", TrackKind::Group, 0.0),
        ("Band", TrackKind::Group, 0.0),
        ("Reverb", TrackKind::Return, 0.0),
        ("Delay", TrackKind::Return, 0.0),
    ];
    let ids = names.map(|(name, kind, frequency)| {
        let id = project.add_track(name, kind);
        if frequency > 0.0 {
            project.track_mut(id).unwrap().tone = Some(TestTone {
                frequency,
                level: 0.1,
            });
        }
        id
    });
    let [kick, snare, bass, vocal, drums, band, reverb, delay] = ids;
    for (track, group) in [(kick, drums), (snare, drums), (drums, band), (bass, band)] {
        project.track_mut(track).unwrap().output = Bus::Group(group);
    }
    // A send that was deleted, leaving a gap in the ids.
    project.new_send(reverb);
    for (track, to, level, pre_fader) in [
        (snare, reverb, 0.3, false),
        (vocal, reverb, 0.5, true),
        (vocal, delay, 0.25, false),
        (band, delay, f32::from_bits(0x15ae_43fd), false),
    ] {
        let mut send = project.new_send(to);
        send.level = level;
        send.pre_fader = pre_fader;
        project.track_mut(track).unwrap().sends.push(send);
    }
    let device = |project: &mut Project, gain| project.new_device(DeviceKind::TestGain { gain });
    let kick_gain = device(&mut project, 0.5);
    device(&mut project, 1.0);
    let boost = device(&mut project, 2.0);
    let invert = device(&mut project, -1.0);
    let master_gain = device(&mut project, 0.9);
    project.track_mut(kick).unwrap().devices = vec![kick_gain];
    project.track_mut(snare).unwrap().devices = vec![boost, invert];
    project.master.devices = vec![master_gain];

    fn track(project: &mut Project, id: TrackId) -> &mut Track {
        project.track_mut(id).unwrap()
    }
    track(&mut project, snare).pan = -0.3;
    track(&mut project, snare).clip_type = ClipType::Hard;
    track(&mut project, bass).pan = 0.25;
    track(&mut project, bass).muted = true;
    track(&mut project, vocal).pan = -1.0;
    track(&mut project, vocal).gain = 0.7;
    track(&mut project, vocal).solo = true;
    track(&mut project, vocal).armed = true;
    track(&mut project, kick).solo_safe = true;
    track(&mut project, band).pan = 1.0;
    track(&mut project, band).clip_type = ClipType::Soft;
    track(&mut project, delay).solo_safe = false;
    project.master.gain = 0.8;
    project.master.pan = -0.0;
    project.master.clip_type = ClipType::Soft;
    project.next_device_id += 2;
    project
}

/// Clips, from version 3: audio takes that crossfade, one from a second
/// into its file and one at an absolute path, MIDI clips of notes with every
/// kind of expression, notes alike, a test instrument, a clip starting into
/// its notes, and ids with gaps where clips and notes were deleted.
fn clips() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(96.0);
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));

    let deleted = ClipContent::Audio(AudioClip::new("gone.wav"));
    project.new_clip("Deleted", beats(0, 1), deleted);
    let mut first = AudioClip::new("Audio/Drums take 1.wav");
    first.fade_out = Beats::new(1);
    let first = project.new_clip("Take 1", beats(0, 8), ClipContent::Audio(first));
    let mut second = AudioClip::new("Audio/Drums take 2.flac");
    second.offset = 48_000;
    second.gain = 0.5;
    second.fade_in = Beats::new(1);
    second.fade_out = Beats::from_ratio(1, 3);
    let second = project.new_clip("Take 2", beats(7, 16), ClipContent::Audio(second));
    let hit = ClipContent::Audio(AudioClip::new("C:/Samples/Hit.aiff"));
    let hit = project.new_clip("Hit", beats(16, 17), hit);
    project.track_mut(drums).unwrap().clips = vec![first, second, hit];

    let mut chords = MidiClip {
        offset: Beats::from_ratio(-1, 2),
        ..MidiClip::default()
    };
    for (i, key) in [60, 64, 67].into_iter().enumerate() {
        let note = chords.add_note(Beats::ZERO, Beats::new(4), key, 0.5 + i as f32 / 8.0);
        note.release_velocity = 0.25;
    }
    let lead = chords.add_note(Beats::new(4), Beats::new(2), 72, 1.0);
    // Bent up a tone, then all the way down; pressed harder on the same
    // tick, which wins; and slid after the note ends, where it doesn't play.
    lead.expression.pitch_bend = vec![
        Point::new(Beats::ZERO, 0.0),
        Point::new(Beats::from_ratio(1, 2), 2.0),
        Point::new(Beats::new(1), -120.0),
    ];
    lead.expression.pressure = vec![
        Point::new(Beats::from_ratio(1, 4), 0.3),
        Point::new(Beats::from_ratio(1, 4), 0.9),
    ];
    lead.expression.timbre = vec![Point::new(Beats::new(3), 1.0)];
    chords.add_note(Beats::new(6), Beats::new(1), 0, 0.0);
    chords.add_note(Beats::new(6), Beats::new(1), 0, 0.0);
    let gone = chords.add_note(Beats::new(7), Beats::new(1), 127, 1.0).id;
    chords.notes.retain(|note| note.id != gone);
    let chords = project.new_clip("Chords", beats(0, 8), ClipContent::Midi(chords));
    let empty = project.new_clip("", beats(8, 9), ClipContent::Midi(MidiClip::default()));
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.25 });
    let track = project.track_mut(keys).unwrap();
    track.clips = vec![empty, chords];
    track.devices = vec![instrument];
    project.next_clip_id += 1;
    project
}

/// Locators, from version 4: out of order, two at the same place, one
/// before the song starts and one as far in as a file reaches, names JSON
/// has to escape, and a gap in the ids where one was deleted.
fn locators() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(87.0);
    for (name, at) in [
        ("Chorus", Beats::new(64)),
        ("Intro", Beats::ZERO),
        ("Deleted", Beats::new(8)),
        ("Verse \"1\"\t\u{1f3a4}", Beats::new(16)),
        ("Verse 2", Beats::new(16)),
        ("", Beats::from_ratio(-7, 3)),
        ("End", Beats::from_ticks(MAX_TICKS)),
    ] {
        let locator = project.new_locator(name, at);
        project.locators.push(locator);
    }
    project.locators.retain(|locator| locator.name != "Deleted");
    project.add_track("Audio", TrackKind::Audio);
    project
}

/// Plugins, from version 5: a CLAP instrument with a state and parameters
/// set after it, a VST3 effect as it starts, one with parameters but no
/// state, one on the master, a state of every byte, parameter values at the
/// edges of their range, names JSON has to escape, and a gap in the device
/// ids where one was deleted.
fn plugins() -> Project {
    let mut project = Project::new();
    let synth = project.add_track("Synth", TrackKind::Midi);
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let plugin = |format, id: &str, name: &str, instrument, settings| {
        DeviceKind::Plugin(PluginDevice {
            format,
            id: id.into(),
            name: name.into(),
            instrument,
            settings,
        })
    };
    let params = |values: &[(u32, f64)]| {
        values
            .iter()
            .map(|&(id, value)| PluginParam { id, value })
            .collect()
    };

    let surge = plugin(
        PluginFormat::Clap,
        "org.surge-synth-team.surge-xt",
        "Surge XT",
        true,
        PluginSettings {
            state: Some(PluginState::new((0..=255).collect::<Vec<u8>>())),
            params: params(&[(0, 0.5), (17, -0.0), (4_294_967_295, f64::MAX)]),
        },
    );
    let surge = project.new_device(surge);
    let gone = plugin(
        PluginFormat::Clap,
        "org.example.gone",
        "Deleted",
        false,
        PluginSettings::default(),
    );
    project.new_device(gone);
    let eq = plugin(
        PluginFormat::Vst3,
        "ABCDEF019182FAEB4D6574612D457131",
        "EQ \"8\"\t\u{1f39b}\u{fe0f}",
        false,
        PluginSettings::default(),
    );
    let eq = project.new_device(eq);
    let gate = plugin(
        PluginFormat::Clap,
        "com.example.gate",
        "",
        false,
        PluginSettings {
            state: None,
            params: params(&[(3, f64::MIN), (9, f64::from_bits(1)), (12, 1.0 / 3.0)]),
        },
    );
    let gate = project.new_device(gate);
    let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
    let limiter = plugin(
        PluginFormat::Clap,
        "com.example.limiter",
        "Limiter",
        false,
        PluginSettings {
            state: Some(PluginState::new(*b"{\"ceiling\": -0.3}")),
            params: Vec::new(),
        },
    );
    let limiter = project.new_device(limiter);
    project.track_mut(synth).unwrap().devices = vec![surge, gain];
    project.track_mut(vocal).unwrap().devices = vec![gate, eq];
    project.master.devices = vec![limiter];
    project
}

/// Recording, from version 6: punching in and out, inputs of every kind
/// (the default input, a mono channel of another, one that isn't there any
/// more, a MIDI input on one channel, and none), every kind of monitoring,
/// an armed track, takes of audio and of MIDI with the parts comped onto
/// their tracks, a take of two clips, an empty one, and a gap in the take
/// ids where one was deleted.
fn recording() -> Project {
    let mut project = Project::new();
    project.punch_in = true;
    project.punch_out = true;
    project.loop_region = BeatRange::new(Beats::new(4), Beats::new(12));
    project.looping = true;
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let guitar = project.add_track("Guitar \"DI\"", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let room = project.add_track("Room", TrackKind::Audio);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let audio = |file: &str, offset| {
        let mut audio = AudioClip::new(file);
        audio.offset = offset;
        ClipContent::Audio(audio)
    };

    // Two passes of the loop, and the second half of the first comped in.
    let mut first = project.new_take("Take 1");
    let clip = project.new_clip("Vocal", beats(4, 12), audio("Recordings/Vocal 1.wav", 0));
    first.clips.push(clip);
    project.new_take("Deleted");
    let mut second = project.new_take("Take 2");
    let clip = project.new_clip(
        "Vocal",
        beats(4, 12),
        audio("Recordings/Vocal 1.wav", 384_000),
    );
    second.clips.push(clip);
    let comped = [
        project.new_clip(
            "Vocal",
            beats(4, 8),
            audio("Recordings/Vocal 1.wav", 384_000),
        ),
        project.new_clip(
            "Vocal",
            beats(8, 12),
            audio("Recordings/Vocal 1.wav", 192_000),
        ),
    ];
    let track = project.track_mut(vocal).unwrap();
    track.armed = true;
    track.takes = vec![first, second];
    track.clips = comped.into();

    // A take of two clips, recorded through a punch, and an empty one.
    let mut punched = project.new_take("Take 1");
    for (range, offset) in [(beats(4, 6), 0), (beats(10, 12), 96_000)] {
        let clip = project.new_clip("Guitar", range, audio("C:/Takes/Guitar.wav", offset));
        punched.clips.push(clip);
    }
    let empty = project.new_take("");
    let track = project.track_mut(guitar).unwrap();
    track.input = Some(Input::Audio(AudioInput {
        device: "{0.0.1.00000000}.{a5c2e7b0-9f1d-4c3e-8b6a-2d4f6e8a0c1b}".into(),
        name: "Line In (USB Audio \u{1f3b8})".into(),
        channel: 2,
        stereo: false,
    }));
    track.monitor = Monitor::In;
    track.takes = vec![punched, empty];

    let mut played = MidiClip::default();
    played.add_note(Beats::ZERO, Beats::new(1), 48, 0.75);
    let mut take = project.new_take("Take 1");
    take.clips
        .push(project.new_clip("Keys", beats(4, 8), ClipContent::Midi(played)));
    let track = project.track_mut(keys).unwrap();
    track.input = Some(Input::Midi(MidiInput {
        device: "Seaboard RISE 2".into(),
        name: "Seaboard RISE 2".into(),
        channel: Some(15),
    }));
    track.monitor = Monitor::Off;
    track.takes = vec![take];

    let track = project.track_mut(room).unwrap();
    track.input = None;
    track.armed = true;
    project.track_mut(reverb).unwrap().monitor = Monitor::In;
    project
}

/// Musicable's own devices, from version 7: one of every kind, an EQ with
/// bands at the edges of their ranges, a sampler with a sample and one
/// without, a synth playing a MIDI track, a utility on the master, and a
/// gap in the device ids where one was deleted.
fn built_in() -> Project {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Midi);
    let lead = project.add_track("Lead", TrackKind::Midi);
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let device = |project: &mut Project, kind, values: &[(usize, f64)]| {
        let mut device = NativeDevice::new(kind);
        for &(param, value) in values {
            device.values[param] = value;
        }
        project.new_device(DeviceKind::Native(device))
    };

    let mut kick = NativeDevice::new(NativeKind::Sampler);
    kick.sample = Some("Samples/Kick \"808\".wav".into());
    kick.values[sampler::ROOT] = 36.0;
    kick.values[sampler::MODE] = 1.0;
    let kick = project.new_device(DeviceKind::Native(kick));
    let empty = device(
        &mut project,
        NativeKind::Sampler,
        &[(sampler::VOICES, 32.0)],
    );
    // Deleted.
    device(&mut project, NativeKind::Gate, &[]);
    let synth = device(
        &mut project,
        NativeKind::Synth,
        &[
            (synth::OSC1_WAVE, 3.0),
            (synth::OSC2_PITCH, -24.0),
            (synth::OSC2_DETUNE, 7.0),
            (synth::FILTER_ENV, -96.0),
            (synth::GLIDE, 1.0 / 3.0),
        ],
    );
    let band = |b: usize, param: usize| b * eq::PER_BAND + param;
    let tone = device(
        &mut project,
        NativeKind::Eq,
        &[
            (band(0, eq::ON), 1.0),
            (band(0, eq::FREQ), 10.0),
            (band(2, eq::GAIN), -24.0),
            (band(2, eq::Q), 18.0),
            (band(5, eq::SHAPE), 5.0),
            (band(7, eq::ON), 1.0),
            (band(7, eq::FREQ), 22_000.0),
            (eq::OUTPUT, -0.0),
        ],
    );
    let compressor = device(&mut project, NativeKind::Compressor, &[]);
    let delay = device(&mut project, NativeKind::Delay, &[]);
    let reverb = device(&mut project, NativeKind::Reverb, &[]);
    let utility = device(
        &mut project,
        NativeKind::Utility,
        &[(utility::GAIN, utility::MAX_GAIN), (utility::PAN, -1.0)],
    );
    project.track_mut(drums).unwrap().devices = vec![kick, empty];
    project.track_mut(lead).unwrap().devices = vec![synth, delay];
    project.track_mut(vocal).unwrap().devices = vec![tone, compressor, reverb];
    project.master.devices = vec![utility];
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

/// Each built-in device's parameters as `docs/project-format.md` lists
/// them: a table for each kind, a row for each parameter.
fn parameter_tables() -> String {
    let mut text = String::new();
    for kind in NativeKind::ALL {
        text += &format!("**{}** (`\"{}\"`):\n\n", kind.name(), kind.key());
        text += "| # | Key | Control | Values | Default |\n|---|---|---|---|---|\n";
        // The first parameter with each list of choices.
        let mut listed: Vec<(&[&str], &str)> = Vec::new();
        for (number, p) in kind.params().iter().enumerate() {
            let range = format!("{} to {}", p.min, p.max);
            let values = match p.unit {
                Unit::Switch => "0 off, 1 on".to_owned(),
                Unit::Choice(choices) => match listed.iter().find(|(c, _)| *c == choices) {
                    Some((_, first)) => format!("as `{first}`"),
                    None => {
                        listed.push((choices, p.key));
                        let named = choices.iter().enumerate().map(|(i, c)| format!("{i} {c}"));
                        named.collect::<Vec<_>>().join(", ")
                    }
                },
                Unit::Number if p.stepped => format!("{range}, whole"),
                Unit::Number => range,
                Unit::Note => format!("{range}, a MIDI note"),
                Unit::Gain => format!("{range}, linear gain"),
                Unit::Pan => format!("{range}, left to right"),
                Unit::Ratio => format!("{range}, a ratio to 1"),
                Unit::Semitones if p.stepped => format!("{range} semitones, whole"),
                unit => {
                    let word = match unit {
                        Unit::Decibels => "dB",
                        Unit::Hertz => "Hz",
                        Unit::Milliseconds => "ms",
                        Unit::Seconds => "s",
                        Unit::Percent => "%",
                        Unit::Semitones => "semitones",
                        Unit::Cents => "cents",
                        _ => unreachable!(),
                    };
                    format!("{range} {word}")
                }
            };
            text += &format!(
                "| {number} | `{}` | {} | {values} | {} |\n",
                p.key,
                p.full_name(),
                p.default
            );
        }
        text += "\n";
    }
    text
}

#[test]
fn the_documented_parameters_are_the_tables() {
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/project-format.md");
    let page = fs::read_to_string(page).unwrap().replace("\r\n", "\n");
    let tables = parameter_tables();
    assert!(
        page.contains(&tables),
        "docs/project-format.md doesn't list the built-in devices' parameters as they \
         are. Put this in its place:\n\n{tables}"
    );
}
