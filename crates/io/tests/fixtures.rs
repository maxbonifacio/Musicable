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
use musicable_model::automation::{self, Lane};
use musicable_model::clip::{AudioClip, ClipContent, Controller, MidiClip, Offset, Point};
use musicable_model::mixer::Bus;
use musicable_model::modulation::{ControlKind, Curve, ModulatorKind, Output, envelope, lfo};
use musicable_model::native::{
    NativeDevice, NativeKind, ParamInfo, Unit, compressor, eq, gate, sampler, synth, utility,
};
use musicable_model::param::{ClipParam, ParamAddress, ParamSpec};
use musicable_model::plugin::{
    PluginDevice, PluginInput, PluginParam, PluginSettings, PluginState,
};
use musicable_model::routing::{Port, Tap};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::warp::{
    GranularSettings, Onset, Onsets, Origin, Preserve, SliceSettings, SpectralSettings,
    TransientLoop, Warp, WarpMarker, WarpMode,
};
use musicable_model::{
    AudioInput, Chain, ClipType, DeviceKind, Input, MidiInput, Monitor, PluginFormat, Project,
    TestTone, Track, TrackId, TrackKind,
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
        ("controllers", controllers()),
        ("built-in", built_in()),
        ("automation", automation()),
        ("workspace", workspace()),
        ("warping", warping()),
        ("transients", transients()),
        ("slicing", slicing()),
        ("spectral", spectral()),
        ("high-quality", high_quality()),
        ("granular", granular()),
        ("bypass", bypass()),
        ("modulation", modulation()),
        ("sidechain", sidechain()),
        ("plugin-inputs", plugin_inputs()),
        ("tracks-from-tracks", tracks_from_tracks()),
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
    second.offset = Offset::Frames(48_000);
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
            inputs: Vec::new(),
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
        audio.offset = Offset::Frames(offset);
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

/// Controllers, from version 7: the sustain pedal down and up across a
/// clip's notes, held on at its end; the mod wheel with two points on the
/// same tick, the later winning; the lowest and highest controllers, one
/// MIDI names and one it doesn't; values at the edges of the range; a point
/// before the clip's own time starts and one as far in as a file reaches;
/// and a take's clip that moves a controller.
fn controllers() -> Project {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let at = |beats: f64, value| Point::new(Beats::from_f64(beats), value);

    let mut chords = MidiClip {
        offset: Beats::from_ratio(-1, 2),
        ..MidiClip::default()
    };
    chords.add_note(Beats::ZERO, Beats::new(2), 60, 0.75);
    chords.add_note(Beats::new(2), Beats::new(2), 64, 0.75);
    let controller = |number| Controller::new(number).unwrap();
    chords.set_controller(
        Controller::SUSTAIN,
        vec![at(-0.5, 1.0), at(1.75, 0.0), at(2.0, 1.0)],
    );
    chords.set_controller(
        Controller::MOD_WHEEL,
        vec![
            at(0.0, 0.0),
            at(1.0, 0.25),
            at(1.0, 64.0 / 127.0),
            at(3.5, 1.0),
        ],
    );
    chords.set_controller(controller(0), vec![at(0.0, 0.0)]);
    chords.set_controller(controller(3), vec![at(0.25, 1.0 / 3.0)]);
    chords.set_controller(
        controller(Controller::MAX),
        vec![Point::new(Beats::from_ticks(MAX_TICKS), 1.0)],
    );
    let chords = project.new_clip("Chords", beats(0, 4), ClipContent::Midi(chords));

    let mut played = MidiClip::default();
    played.add_note(Beats::ZERO, Beats::new(1), 48, 0.5);
    played.set_controller(Controller::EXPRESSION, vec![at(0.0, 0.5), at(0.5, 0.0)]);
    let mut take = project.new_take("Take 1");
    take.clips
        .push(project.new_clip("Keys", beats(4, 8), ClipContent::Midi(played)));
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let track = project.track_mut(keys).unwrap();
    track.clips = vec![chords];
    track.takes = vec![take];
    track.devices = vec![instrument];
    project
}

/// Musicable's own devices, from version 8: one of every kind, an EQ with
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

/// The workspace a project was saved with, from version 10: one whose name
/// isn't plain ASCII.
fn workspace() -> Project {
    let mut project = Project::new();
    project.add_track("Vocals", TrackKind::Audio);
    project.workspace = Some("Mixage à deux".to_owned());
    project
}

/// Automation, from version 9: lanes on a track's volume and pan, the
/// master's volume and pan, a send, a test device's parameter, a built-in
/// EQ's frequency and shape, and a plugin's, with ramps, steps, a lane of
/// one point, one that starts into the song, values a float can only just
/// hold, the ends of a built-in parameter's range, and an overridden lane.
fn automation() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(90.0);
    let bass = project.add_track("Bass", TrackKind::Audio);
    let synth = project.add_track("Synth", TrackKind::Midi);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let send = project.new_send(reverb);
    let send_id = send.id;
    project.track_mut(bass).unwrap().sends.push(send);
    let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
    let gain_id = gain.id;
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.8 });
    let filter = DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "com.example.filter".into(),
        name: "Filter".into(),
        instrument: false,
        inputs: Vec::new(),
        settings: PluginSettings::default(),
    });
    let filter = project.new_device(filter);
    let filter_id = filter.id;
    let tone = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
    let tone_id = tone.id;
    project.track_mut(bass).unwrap().devices = vec![gain];
    project.track_mut(synth).unwrap().devices = vec![instrument, filter, tone];

    let lane = |param, points: &[(f64, f64)]| {
        let points = points
            .iter()
            .map(|&(at, value)| automation::Point::new(Beats::from_f64(at), value))
            .collect();
        Lane::new(param, points)
    };
    let mut overridden = lane(
        ParamAddress::Pan(Chain::Track(bass)),
        &[(0.0, -0.5), (8.0, 0.5)],
    );
    overridden.overridden = true;
    project.track_mut(bass).unwrap().pan = 0.125;
    project.automation = vec![
        // A fade in, a step down at bar 3 and a ramp back up.
        lane(
            ParamAddress::Volume(Chain::Track(bass)),
            &[
                (0.0, 0.0),
                (4.0, 1.0),
                (8.0, 1.0),
                (8.0, 0.25),
                (15.5, ParamSpec::VOLUME_MAX),
            ],
        ),
        overridden,
        lane(ParamAddress::Send(send_id), &[(2.0 / 3.0, 0.1)]),
        lane(ParamAddress::Volume(Chain::Master), &[(0.0, 0.8)]),
        lane(
            ParamAddress::Pan(Chain::Master),
            &[(4.0, -1.0), (4.0, 1.0), (12.0, -0.0)],
        ),
        lane(
            ParamAddress::Device(gain_id, 0),
            &[(0.0, f64::from_bits(1)), (1.0, -2.0)],
        ),
        lane(
            ParamAddress::Device(filter_id, 4_294_967_295),
            &[(0.25, 1e300), (0.5, -1e-300), (0.75, 20_000.0)],
        ),
        // A sweep from the bottom of the range to the top, and a step from
        // one shape to another.
        lane(
            ParamAddress::Device(tone_id, eq::FREQ as u32),
            &[(0.0, 10.0), (16.0, 22_000.0)],
        ),
        lane(
            ParamAddress::Device(tone_id, eq::SHAPE as u32),
            &[(4.0, 4.0), (4.0, 9.0)],
        ),
    ];
    project
}

/// A warp as `Warp::new` makes one, but with Slice looping forward, as a
/// new clip's did until 26 October 2026: every version's files hold that.
fn warp(frame: u64, beat: Beats, tempo: f64) -> Warp {
    let mut warp = Warp::new(frame, beat, tempo);
    warp.slice.transient_loop = TransientLoop::Forward;
    warp
}

/// Warping, from version 11: a loop warped with one marker at a tempo of
/// its own; a vocal with markers that slow it down and speed it up, from
/// before its own time's zero, starting before its first marker, with a
/// gain curve that starts before it too and steps up to the top; the
/// slowest and fastest tempos, and markers as far in as a file reaches; an
/// unwarped clip with a gain curve in its file's seconds; and a warped take.
fn warping() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(128.0);
    let drums = project.add_track("Drums", TrackKind::Audio);
    let vocals = project.add_track("Vocals", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let marker = |frame, beat| WarpMarker { frame, beat };
    let point = |at, value| automation::Point::new(at, value);
    let warped = |file: &str, offset, warp| {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(offset);
        audio.warp = Some(warp);
        audio
    };

    let breaks = warped(
        "Loops/Break 120 BPM.wav",
        Beats::ZERO,
        warp(0, Beats::ZERO, 120.0),
    );
    let breaks = project.new_clip("Break", beats(0, 8), ClipContent::Audio(breaks));
    let slowest = warped(
        "Pad.wav",
        Beats::new(-2),
        warp(
            MAX_TICKS as u64,
            Beats::from_ticks(MAX_TICKS),
            Warp::MIN_TEMPO,
        ),
    );
    let slowest = project.new_clip("Slowest", beats(8, 16), ClipContent::Audio(slowest));
    let fastest = warped(
        "Hats.wav",
        Beats::from_ticks(-MAX_TICKS),
        warp(12_345, Beats::from_ticks(-MAX_TICKS), Warp::MAX_TEMPO),
    );
    let fastest = project.new_clip("Fastest", beats(16, 17), ClipContent::Audio(fastest));
    project.track_mut(drums).unwrap().clips = vec![breaks, slowest, fastest];

    let mut vocal = warped(
        "Vocal.wav",
        Beats::from_ratio(-1, 4),
        Warp {
            markers: vec![
                marker(1_000, Beats::from_ratio(1, 2)),
                marker(48_000, Beats::new(1)),
                marker(60_000, Beats::from_ratio(10, 3)),
            ],
            ..warp(0, Beats::ZERO, 90.0)
        },
    );
    vocal.gain = 0.5;
    vocal.fade_in = Beats::from_ratio(1, 8);
    vocal.set_lane(
        ClipParam::Gain,
        vec![
            point(Beats::from_ratio(-1, 2), 0.0),
            point(Beats::new(1), 1.0),
            point(Beats::new(1), ParamSpec::CLIP_GAIN_MAX),
        ],
    );
    let vocal = project.new_clip("Vocal", beats(4, 8), ClipContent::Audio(vocal));
    let mut take = AudioClip::new("Vocal.wav");
    take.offset = Offset::Frames(96_000);
    take.set_lane(
        ClipParam::Gain,
        vec![point(Beats::new(2), 1.0), point(Beats::new(4), 0.0)],
    );
    let take = project.new_clip("Vocal", beats(8, 12), ClipContent::Audio(take));
    let mut first = project.new_take("Take 1");
    let looped = warped("Vocal.wav", Beats::new(1), warp(0, Beats::ZERO, 64.5));
    first
        .clips
        .push(project.new_clip("Vocal", beats(4, 12), ClipContent::Audio(looped)));
    let track = project.track_mut(vocals).unwrap();
    track.clips = vec![vocal, take];
    track.takes = vec![first];
    project
}

/// Transients, from version 12: a loop warped with transients analysis
/// found, one the user added, one deleted and one moved, from the file's
/// first frame to as far in as a file reaches, with strengths and sharpness
/// at both ends; a warped take with none; and a warped clip that hasn't
/// taken them from analysis yet.
fn transients() -> Project {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let onset = |frame, strength, percussiveness, origin| Onset {
        frame,
        strength,
        percussiveness,
        origin,
    };
    let warped = |file: &str, onsets| {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(Warp {
            onsets,
            ..warp(0, Beats::ZERO, 120.0)
        });
        audio
    };
    let found = Onsets::new([
        onset(0, 1.0, 1.0, Origin::Found),
        onset(12_000, 0.25, 0.75, Origin::Found),
        onset(24_000, 0.5, 0.0, Origin::Hidden),
        onset(30_000, 0.0, 0.5, Origin::Added),
        onset(36_000, 0.125, 1.0, Origin::Hidden),
        onset(36_500, 0.125, 1.0, Origin::Added),
        onset(MAX_TICKS as u64, 0.75, 0.25, Origin::Found),
    ]);
    let looped = warped("Loops/Break 120 BPM.wav", Some(found));
    let looped = project.new_clip("Break", beats(0, 8), ClipContent::Audio(looped));
    let waiting = warped("Loops/Fill.wav", None);
    let waiting = project.new_clip("Fill", beats(8, 12), ClipContent::Audio(waiting));
    let mut take = project.new_take("Take 1");
    let none = warped("Loops/Break 120 BPM.wav", Some(Onsets::default()));
    take.clips
        .push(project.new_clip("Break", beats(0, 4), ClipContent::Audio(none)));
    let track = project.track_mut(drums).unwrap();
    track.clips = vec![looped, waiting];
    track.takes = vec![take];
    project
}

/// Slice, from version 13: loops in Slice, cut at their transients with
/// each loop mode, and on each grid of notes, with Transient Envelope at
/// both ends; transposes and detunes at their ends, kept by a clip in
/// Re-Pitch too; and a curve for each of Slice's parameters, with steps.
fn slicing() -> Project {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let warped = |warp: Warp| {
        let mut audio = AudioClip::new("Loops/Break 120 BPM.wav");
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(warp);
        audio
    };
    let slice = |preserve, transient_loop, envelope| Warp {
        mode: WarpMode::Slice,
        slice: SliceSettings {
            preserve,
            transient_loop,
            envelope,
        },
        ..warp(0, Beats::ZERO, 120.0)
    };
    let mut clips = Vec::new();
    let loops = [
        TransientLoop::Off,
        TransientLoop::Forward,
        TransientLoop::BackAndForth,
    ];
    for (k, &preserve) in Preserve::ALL.iter().enumerate() {
        let looping = loops[k % loops.len()];
        let envelope = [0.0, 37.5, SliceSettings::ENVELOPE_MAX][k % 3];
        let audio = warped(slice(preserve, looping, envelope));
        let at = 4 * k as i64;
        clips.push(project.new_clip("Break", beats(at, at + 4), ClipContent::Audio(audio)));
    }
    let mut lowest = slice(Preserve::Transients, TransientLoop::Forward, 100.0);
    (lowest.transpose, lowest.detune) = (-Warp::TRANSPOSE_MAX, -Warp::DETUNE_MAX);
    let lowest = project.new_clip("Low", beats(28, 32), ClipContent::Audio(warped(lowest)));
    let mut kept = warp(0, Beats::ZERO, 120.0);
    (kept.transpose, kept.detune) = (Warp::TRANSPOSE_MAX, Warp::DETUNE_MAX);
    kept.slice.preserve = Preserve::Sixteenth;
    let kept = project.new_clip("Kept", beats(32, 36), ClipContent::Audio(warped(kept)));
    let mut moving = warped(slice(Preserve::Eighth, TransientLoop::Off, 50.0));
    let at = |beat: i64| Beats::new(beat);
    let point = |at, value| automation::Point::new(at, value);
    moving.set_lane(
        ClipParam::Transpose,
        vec![point(at(0), -48.0), point(at(2), 12.5), point(at(4), 48.0)],
    );
    moving.set_lane(
        ClipParam::Detune,
        vec![point(Beats::from_ratio(-1, 2), -50.0), point(at(1), 50.0)],
    );
    moving.set_lane(
        ClipParam::TransientEnvelope,
        vec![point(at(0), 0.0), point(at(4), 100.0)],
    );
    moving.set_lane(
        ClipParam::Preserve,
        vec![point(at(0), 0.0), point(at(2), 0.0), point(at(2), 6.0)],
    );
    moving.set_lane(
        ClipParam::TransientLoop,
        vec![point(at(1), 2.0), point(at(1), 1.0), point(at(3), 0.0)],
    );
    let moving = project.new_clip("Moving", beats(36, 40), ClipContent::Audio(moving));
    clips.extend([lowest, kept, moving]);
    project.track_mut(drums).unwrap().clips = clips;
    project
}

/// Spectral, from version 14: a mix and a vocal in Spectral with Formants
/// at both ends and between, transposed, a clip in Slice keeping its own
/// Formants, and a curve for Formants.
fn spectral() -> Project {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let warped = |file, warp: Warp| {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(warp);
        audio
    };
    let spectral = |formants| Warp {
        mode: WarpMode::Spectral,
        spectral: SpectralSettings {
            formants,
            ..SpectralSettings::default()
        },
        ..warp(0, Beats::ZERO, 96.0)
    };
    let mix = warped("Mixes/Song.wav", spectral(SpectralSettings::FORMANTS_MAX));
    let mix = project.new_clip("Song", beats(0, 16), ClipContent::Audio(mix));
    let mut up = spectral(0.0);
    (up.transpose, up.detune) = (5, -12);
    let up = project.new_clip(
        "Up",
        beats(16, 20),
        ClipContent::Audio(warped("Vox.wav", up)),
    );
    let mut kept = spectral(62.5);
    kept.mode = WarpMode::Slice;
    let kept = project.new_clip(
        "Kept",
        beats(20, 24),
        ClipContent::Audio(warped("Vox.wav", kept)),
    );
    let mut moving = warped("Vox.wav", spectral(50.0));
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    moving.set_lane(
        ClipParam::Formants,
        vec![point(0, 0.0), point(2, 25.0), point(4, 100.0)],
    );
    moving.set_lane(ClipParam::Transpose, vec![point(0, -5.0), point(4, 5.0)]);
    let moving = project.new_clip("Moving", beats(24, 28), ClipContent::Audio(moving));
    project.track_mut(vocal).unwrap().clips = vec![mix, up, kept, moving];
    project
}

/// High quality, from version 15: a mix and vocals in Spectral with High
/// quality on and Envelope at both ends and between, one with it off
/// keeping its own Envelope, a clip in Slice keeping both, and a curve for
/// Envelope.
fn high_quality() -> Project {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let warped = |file, warp: Warp| {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(warp);
        ClipContent::Audio(audio)
    };
    let high_quality = |envelope| Warp {
        mode: WarpMode::Spectral,
        spectral: SpectralSettings {
            high_quality: true,
            envelope,
            ..SpectralSettings::default()
        },
        ..warp(0, Beats::ZERO, 96.0)
    };
    let mix = warped(
        "Mixes/Song.wav",
        high_quality(SpectralSettings::ENVELOPE_DEFAULT),
    );
    let mix = project.new_clip("Song", beats(0, 16), mix);
    let mut high = high_quality(SpectralSettings::ENVELOPE_MIN);
    (high.transpose, high.spectral.formants) = (7, 75.0);
    let high = project.new_clip("High", beats(16, 20), warped("Vox.wav", high));
    let mut low = high_quality(SpectralSettings::ENVELOPE_MAX);
    low.transpose = -12;
    let low = project.new_clip("Low", beats(20, 24), warped("Vox.wav", low));
    let mut quick = high_quality(64.5);
    quick.spectral.high_quality = false;
    let quick = project.new_clip("Quick", beats(24, 28), warped("Vox.wav", quick));
    let mut kept = high_quality(200.0);
    kept.mode = WarpMode::Slice;
    let kept = project.new_clip("Kept", beats(28, 32), warped("Vox.wav", kept));
    let mut moving = AudioClip::new("Vox.wav");
    moving.offset = Offset::Beats(Beats::ZERO);
    moving.warp = Some(high_quality(SpectralSettings::ENVELOPE_DEFAULT));
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    moving.set_lane(
        ClipParam::Envelope,
        vec![point(0, 8.0), point(2, 128.0), point(4, 512.0)],
    );
    moving.set_lane(ClipParam::Formants, vec![point(0, 100.0), point(4, 0.0)]);
    moving.set_lane(ClipParam::Transpose, vec![point(0, 0.0), point(4, 12.0)]);
    let moving = project.new_clip("Moving", beats(32, 36), ClipContent::Audio(moving));
    project.track_mut(vocal).unwrap().clips = vec![mix, high, low, quick, kept, moving];
    project
}

/// Granular, from version 16: a vocal and a bass line following their
/// pitch with Grain Size at both ends and between, transposed, pads off it
/// with Fluctuation at both ends and between, a clip in Spectral keeping
/// its own Granular settings, and curves for Grain Size and Fluctuation.
fn granular() -> Project {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let warped = |file, warp: Warp| {
        let mut audio = AudioClip::new(file);
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(warp);
        audio
    };
    let granular = |follow_pitch, grain_size, fluctuation| Warp {
        mode: WarpMode::Granular,
        granular: GranularSettings {
            follow_pitch,
            grain_size,
            fluctuation,
        },
        ..warp(0, Beats::ZERO, 96.0)
    };
    let vox = granular(true, GranularSettings::GRAIN_SIZE_DEFAULT, 25.0);
    let vox = project.new_clip(
        "Vox",
        beats(0, 8),
        ClipContent::Audio(warped("Vox.wav", vox)),
    );
    let mut bass = granular(true, GranularSettings::GRAIN_SIZE_MAX, 0.0);
    (bass.transpose, bass.detune) = (-7, 33);
    let bass = warped("Bass.wav", bass);
    let bass = project.new_clip("Bass", beats(8, 12), ClipContent::Audio(bass));
    let pad = granular(false, GranularSettings::GRAIN_SIZE_MIN, 0.0);
    let pad = project.new_clip(
        "Pad",
        beats(12, 16),
        ClipContent::Audio(warped("Pad.wav", pad)),
    );
    let mut wide = granular(false, 137.5, GranularSettings::FLUCTUATION_MAX);
    wide.transpose = 12;
    let wide = project.new_clip(
        "Wide",
        beats(16, 20),
        ClipContent::Audio(warped("Pad.wav", wide)),
    );
    let mut kept = granular(false, 72.25, 62.5);
    kept.mode = WarpMode::Spectral;
    let kept = project.new_clip(
        "Kept",
        beats(20, 24),
        ClipContent::Audio(warped("Pad.wav", kept)),
    );
    let mut moving = warped("Pad.wav", granular(false, 30.0, 25.0));
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    moving.set_lane(
        ClipParam::GrainSize,
        vec![point(0, 5.0), point(2, 60.0), point(4, 500.0)],
    );
    moving.set_lane(ClipParam::Fluctuation, vec![point(0, 100.0), point(4, 0.0)]);
    moving.set_lane(ClipParam::Transpose, vec![point(0, 0.0), point(4, -12.0)]);
    let moving = project.new_clip("Moving", beats(24, 28), ClipContent::Audio(moving));
    project.track_mut(vocal).unwrap().clips = vec![vox, bass, pad, wide, kept, moving];
    project
}

/// Bypass and Deactivate, from version 17: a bypassed EQ, a deactivated
/// compressor and a reverb with a Bypass lane that steps on and off on a
/// track, an instrument both bypassed and deactivated, and a bypassed plugin
/// on the master.
fn bypass() -> Project {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let native = |project: &mut Project, kind| {
        project.new_device(DeviceKind::Native(NativeDevice::new(kind)))
    };
    let mut tone = native(&mut project, NativeKind::Eq);
    tone.bypassed = true;
    let mut compressor = native(&mut project, NativeKind::Compressor);
    compressor.active = false;
    let reverb = native(&mut project, NativeKind::Reverb);
    let reverb_id = reverb.id;
    let mut synth = native(&mut project, NativeKind::Synth);
    (synth.bypassed, synth.active) = (true, false);
    let mut limiter = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Vst3,
        id: "ABCDEF0123456789ABCDEF0123456789".into(),
        name: "Limiter".into(),
        instrument: false,
        inputs: Vec::new(),
        settings: PluginSettings::default(),
    }));
    limiter.bypassed = true;
    project.track_mut(vocal).unwrap().devices = vec![tone, compressor, reverb];
    project.track_mut(keys).unwrap().devices = vec![synth];
    project.master.devices = vec![limiter];
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    let steps = vec![
        point(0, 0.0),
        point(4, 0.0),
        point(4, 1.0),
        point(8, 1.0),
        point(8, 0.0),
    ];
    project.automation = vec![Lane::new(ParamAddress::Bypass(reverb_id), steps)];
    project
}

/// Modulators, from version 18. On a track, an LFO and an Envelope move
/// its synth, its fader and each other through a routing with each curve,
/// one scaled by the other and one with a lag. On the master, a Macro with
/// a knob and both kinds of button reaches the track's LFO and a plugin, by
/// an amount in the plugin's own units, through a bypassed routing. Lanes
/// move the knob and the LFO's skew, and a modulator and a control were
/// deleted.
fn modulation() -> Project {
    let mut project = Project::new();
    let lead = project.add_track("Lead", TrackKind::Midi);
    let synth = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Synth)));
    let synth_id = synth.id;
    let plugin = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "com.example.filter".into(),
        name: "Filter".into(),
        instrument: false,
        inputs: Vec::new(),
        settings: PluginSettings::default(),
    }));
    let plugin_id = plugin.id;
    project.track_mut(lead).unwrap().devices = vec![synth];
    project.master.devices = vec![plugin];

    let mut wobble = project.new_modulator(ModulatorKind::Lfo, "Wobble");
    wobble.values[lfo::SHAPE] = 1.0;
    wobble.values[lfo::SKEW] = 25.0;
    wobble.values[lfo::SYNC] = 1.0;
    wobble.values[lfo::NOTE] = 5.0;
    wobble.values[lfo::PHASE] = 90.0;
    wobble.values[lfo::START] = 1.0;
    wobble.values[lfo::FADE_IN] = 250.0;
    wobble.values[lfo::UNIPOLAR] = 1.0;
    // Deleted.
    project.new_modulator(ModulatorKind::Lfo, "LFO 2");
    let mut pluck = project.new_modulator(ModulatorKind::Envelope, "Envelope 1");
    pluck.values[envelope::ATTACK] = 2.5;
    pluck.values[envelope::DECAY] = 1_200.0;
    pluck.values[envelope::SUSTAIN] = 0.0;
    pluck.values[envelope::ONE_SHOT] = 1.0;
    pluck.values[envelope::DECAY_CURVE] = 60.0;
    pluck.values[envelope::RELEASE_CURVE] = -40.0;
    pluck.values[envelope::NOTE_TRIGGER] = 2.0;
    let (wobble_id, pluck_id) = (wobble.id, pluck.id);
    let (wobble_out, pluck_out) = (Output::new(wobble_id, 0), Output::new(pluck_id, 0));
    let synth_param = |n: usize| ParamAddress::Device(synth_id, n as u32);
    let cutoff = project.new_routing(wobble_out, synth_param(synth::CUTOFF), 0.25);
    let mut fader = project.new_routing(pluck_out, ParamAddress::Volume(Chain::Track(lead)), -0.5);
    (fader.curve, fader.lag) = (Curve::Exponential, 20.0);
    let mut resonance = project.new_routing(pluck_out, synth_param(synth::RESONANCE), 0.4);
    (resonance.curve, resonance.scale) = (Curve::SCurve, Some(wobble_out));
    let mut attack = project.new_routing(
        wobble_out,
        ParamAddress::Modulator(pluck_id, envelope::ATTACK as u32),
        0.1,
    );
    attack.curve = Curve::Logarithmic;
    let mut level = project.new_routing(wobble_out, synth_param(synth::OSC2_LEVEL), -1.0);
    level.curve = Curve::Absolute;
    let mut noise = project.new_routing(pluck_out, synth_param(synth::NOISE), 1.0);
    noise.curve = Curve::Positive;
    let track = project.track_mut(lead).unwrap();
    track.modulation.modulators = vec![wobble, pluck];
    track.modulation.routings = vec![cutoff, fader, resonance, attack, level, noise];

    let mut macros = project.new_modulator(ModulatorKind::Macro, "Macro 1");
    macros.controls[0].name = "Brightness".into();
    macros.controls[0].value = 0.5;
    let mut toggle = macros.new_control(ControlKind::Toggle);
    toggle.value = 1.0;
    let toggle_out = Output::new(macros.id, toggle.id);
    macros.controls.push(toggle);
    let momentary = macros.new_control(ControlKind::Momentary);
    let momentary_out = Output::new(macros.id, momentary.id);
    macros.controls.push(momentary);
    // Deleted.
    macros.new_control(ControlKind::Knob);
    let knob = Output::new(macros.id, macros.controls[0].id);
    let macro_id = macros.id;
    let mut rate = project.new_routing(
        knob,
        ParamAddress::Modulator(wobble_id, lfo::RATE as u32),
        0.5,
    );
    rate.curve = Curve::Stepped(4);
    let mut sweep = project.new_routing(toggle_out, ParamAddress::Device(plugin_id, 7), 12.5);
    sweep.bypassed = true;
    let mut pan = project.new_routing(momentary_out, ParamAddress::Pan(Chain::Master), 0.75);
    (pan.curve, pan.scale) = (Curve::Negative, Some(knob));
    project.master.modulation.modulators = vec![macros];
    project.master.modulation.routings = vec![rate, sweep, pan];

    let point = |at, value| automation::Point::new(Beats::new(at), value);
    project.automation = vec![
        Lane::new(
            ParamAddress::Modulator(macro_id, knob.n),
            vec![point(0, 0.0), point(8, 1.0)],
        ),
        Lane::new(
            ParamAddress::Modulator(wobble_id, lfo::SKEW as u32),
            vec![point(0, 50.0), point(4, 10.0)],
        ),
    ];
    project
}

/// Sidechains, from version 19: a compressor on a bass keyed by the kick's
/// Post FX, with its Sidechain on and its key filtered, a gate on a pad
/// keyed by the kick's Pre FX and by the bass after its compressor, summed,
/// a compressor on the master keyed by a group's Post Fader, and a lane on
/// a key's gain.
fn sidechain() -> Project {
    let mut project = Project::new();
    let kick = project.add_track("Kick", TrackKind::Audio);
    let bass = project.add_track("Bass", TrackKind::Audio);
    let pad = project.add_track("Pad", TrackKind::Midi);
    let group = project.add_track("Drums", TrackKind::Group);
    project.track_mut(kick).unwrap().output = Bus::Group(group);
    let native = |project: &mut Project, kind| {
        project.new_device(DeviceKind::Native(NativeDevice::new(kind)))
    };
    let mut ducker = native(&mut project, NativeKind::Compressor);
    let settings = ducker.kind.native_mut().unwrap();
    settings.set(compressor::SIDECHAIN, 1.0);
    settings.set(compressor::KEY_HIGH_PASS, 60.0);
    settings.set(compressor::KEY_LOW_PASS, 4000.0);
    let synth = native(&mut project, NativeKind::Synth);
    let mut gate = native(&mut project, NativeKind::Gate);
    gate.kind.native_mut().unwrap().set(gate::SIDECHAIN, 1.0);
    let glue = native(&mut project, NativeKind::Compressor);
    let (ducker_id, gate_id, glue_id) = (ducker.id, gate.id, glue.id);
    project.track_mut(bass).unwrap().devices = vec![ducker];
    project.track_mut(pad).unwrap().devices = vec![synth, gate];
    project.master.devices = vec![glue];
    let wire = |project: &mut Project, from, to, gain| {
        let mut wire = project.new_wire(from, Port::Sidechain(to));
        wire.gain = gain;
        project.wires.push(wire);
        wire.id
    };
    let key = wire(
        &mut project,
        Tap::PostFx(Chain::Track(kick)),
        ducker_id,
        0.5,
    );
    wire(&mut project, Tap::PreFx(kick), gate_id, 1.0);
    wire(&mut project, Tap::Device(ducker_id), gate_id, 2.0);
    let drums = Tap::PostFader(Chain::Track(group));
    wire(&mut project, drums, glue_id, 1.0);
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    let swell = vec![point(0, 0.25), point(8, 1.0)];
    project.automation = vec![Lane::new(ParamAddress::Wire(key), swell)];
    project
}

/// Plugins' side inputs, from version 20: a VST3 vocoder on a pad with two,
/// its carrier keyed by a synth's Post FX with a lane on the key's gain,
/// and a CLAP ducker on the master with one it calls nothing, keyed by the
/// vocoder, and an instrument with none.
fn plugin_inputs() -> Project {
    let mut project = Project::new();
    let synth = project.add_track("Synth", TrackKind::Midi);
    let pad = project.add_track("Pad", TrackKind::Audio);
    let input = |id, name: &str| PluginInput {
        id,
        name: name.to_owned(),
    };
    let plugin = |format, id: &str, name: &str, instrument, inputs| PluginDevice {
        format,
        id: id.to_owned(),
        name: name.to_owned(),
        instrument,
        inputs,
        settings: PluginSettings::default(),
    };
    let vocoder = plugin(
        PluginFormat::Vst3,
        "0123456789ABCDEF0123456789ABCDEF",
        "Vocoder",
        false,
        vec![input(1, "Carrier"), input(3, "Modulator \"B\"")],
    );
    let ducker = plugin(
        PluginFormat::Clap,
        "com.example.ducker",
        "Ducker",
        false,
        vec![input(u32::MAX, "")],
    );
    let keys = plugin(PluginFormat::Clap, "com.example.keys", "Keys", true, vec![]);
    let keys = project.new_device(DeviceKind::Plugin(keys));
    let vocoder = project.new_device(DeviceKind::Plugin(vocoder));
    let ducker = project.new_device(DeviceKind::Plugin(ducker));
    let (vocoder_id, ducker_id) = (vocoder.id, ducker.id);
    project.track_mut(synth).unwrap().devices = vec![keys];
    project.track_mut(pad).unwrap().devices = vec![vocoder];
    project.master.devices = vec![ducker];
    let from_synth = project.new_wire(
        Tap::PostFx(Chain::Track(synth)),
        Port::PluginInput(vocoder_id, 1),
    );
    let mut from_vocoder = project.new_wire(
        Tap::Device(vocoder_id),
        Port::PluginInput(ducker_id, u32::MAX),
    );
    from_vocoder.gain = 0.75;
    project.wires = vec![from_synth, from_vocoder];
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    let swell = vec![point(0, 0.5), point(4, 2.0)];
    project.automation = vec![Lane::new(ParamAddress::Wire(from_synth.id), swell)];
    project
}

/// Tracks taking their inputs from other tracks, from version 21: a print
/// of a kick's Post Fader monitoring In, one of what the kick's EQ puts out,
/// a ghost kick on Sends Only, a track resampling the master, armed, and a
/// MIDI track doubling another's notes.
fn tracks_from_tracks() -> Project {
    let mut project = Project::new();
    let kick = project.add_track("Kick", TrackKind::Audio);
    let print = project.add_track("Kick Print", TrackKind::Audio);
    let after_eq = project.add_track("Kick EQ", TrackKind::Audio);
    let ghost = project.add_track("Ghost Kick", TrackKind::Audio);
    let resample = project.add_track("Resample", TrackKind::Audio);
    let pad = project.add_track("Pad", TrackKind::Midi);
    let double = project.add_track("Double", TrackKind::Midi);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    let eq = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
    let eq_id = eq.id;
    project.track_mut(kick).unwrap().devices.push(eq);
    let take = |project: &mut Project, track, tap| {
        let track = project.track_mut(track).unwrap();
        track.input = Some(Input::Tap(tap));
        track.monitor = Monitor::In;
    };
    take(&mut project, print, Tap::PostFader(Chain::Track(kick)));
    take(&mut project, after_eq, Tap::Device(eq_id));
    take(&mut project, double, Tap::PreFx(pad));
    let send = project.new_send(reverb);
    let ghost = project.track_mut(ghost).unwrap();
    ghost.output = Bus::SendsOnly;
    ghost.sends.push(send);
    let resample = project.track_mut(resample).unwrap();
    resample.input = Some(Input::RESAMPLING);
    resample.armed = true;
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

/// The parameters of each kind of built-in device, or of modulator, as
/// `docs/project-format.md` lists them: a table for each kind, a row for
/// each parameter.
fn parameter_tables<'a>(
    kinds: impl IntoIterator<Item = (&'a str, &'a str, &'a [ParamInfo])>,
) -> String {
    let mut text = String::new();
    for (name, key, params) in kinds {
        text += &format!("**{name}** (`\"{key}\"`):\n\n");
        text += "| # | Key | Control | Values | Default |\n|---|---|---|---|---|\n";
        // The first parameter with each list of choices.
        let mut listed: Vec<(&[&str], &str)> = Vec::new();
        for (number, p) in params.iter().enumerate() {
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
                Unit::Number | Unit::Fraction => range,
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
                        Unit::Degrees => "degrees",
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
    let devices = NativeKind::ALL.map(|kind| (kind.name(), kind.key(), kind.params()));
    let tables = parameter_tables(devices);
    assert!(
        page.contains(&tables),
        "docs/project-format.md doesn't list the built-in devices' parameters as they \
         are. Put this in its place:\n\n{tables}"
    );
    let shapes = [ModulatorKind::Lfo, ModulatorKind::Envelope];
    let tables = parameter_tables(shapes.map(|kind| (kind.name(), kind.key(), kind.params())));
    assert!(
        page.contains(&tables),
        "docs/project-format.md doesn't list the modulators' parameters as they are. Put \
         this in its place:\n\n{tables}"
    );
    // A Macro's controls are its parameters.
    assert!(ModulatorKind::Macro.params().is_empty());
}
