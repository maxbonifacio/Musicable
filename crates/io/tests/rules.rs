//! The format's rules: what loading refuses and where it says the problem
//! is, what it accepts, and what saving refuses.

use musicable_io::project::{self, Invalid, LoadError, MAX_TICKS, SaveError, VERSION};
use musicable_model::automation::{self, Lane};
use musicable_model::clip::{AudioClip, ClipContent, ClipId, Controller, MidiClip, NoteId, Point};
use musicable_model::mixer::Bus;
use musicable_model::native::{NativeDevice, NativeKind, eq, sampler};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings, PluginState};
use musicable_model::time::{BeatRange, Beats, TimeSignature};
use musicable_model::{
    AudioInput, Chain, DeviceKind, Input, LocatorId, Monitor, PluginFormat, Project, TakeId,
    TestTone, TrackId, TrackKind,
};
use serde_json::{Value, json};

/// A small project with two tempos, two time signatures and two tracks.
fn sample() -> Project {
    let mut project = Project::new();
    project.tempo.insert_tempo(Beats::new(8), 90.0);
    project
        .tempo
        .insert_signature(2, TimeSignature::new(6, 8).unwrap());
    let a = project.add_track("A", TrackKind::Audio);
    project.add_track("B", TrackKind::Midi);
    project.track_mut(a).unwrap().tone = Some(TestTone {
        frequency: 440.0,
        level: 0.5,
    });
    project
}

/// A mix: an audio track feeding a group inside another group and sending
/// to a return track, a MIDI track, and test devices on the audio track and
/// the master. The tracks' ids are their places in the list.
fn mix() -> Project {
    let mut project = Project::new();
    let audio = project.add_track("Audio", TrackKind::Audio);
    project.add_track("MIDI", TrackKind::Midi);
    let inner = project.add_track("Inner", TrackKind::Group);
    let outer = project.add_track("Outer", TrackKind::Group);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    project.track_mut(audio).unwrap().output = Bus::Group(inner);
    project.track_mut(inner).unwrap().output = Bus::Group(outer);
    let send = project.new_send(reverb);
    project.track_mut(audio).unwrap().sends.push(send);
    let device = project.new_device(DeviceKind::TestGain { gain: 0.5 });
    project.track_mut(audio).unwrap().devices.push(device);
    let device = project.new_device(DeviceKind::TestGain { gain: 2.0 });
    project.master.devices.push(device);
    project
}

/// Clips: an audio track with a clip of a file, a MIDI track with a clip of
/// two notes, the second with expression, and the sustain pedal down for
/// the first, played by a test instrument, and a group track. The tracks'
/// ids are their places in the list, and the clips' ids are their tracks'.
fn clips() -> Project {
    let mut project = Project::new();
    let audio = project.add_track("Audio", TrackKind::Audio);
    let midi = project.add_track("MIDI", TrackKind::Midi);
    project.add_track("Group", TrackKind::Group);
    let mut take = AudioClip::new("Takes/Take 1.wav");
    take.fade_in = Beats::new(1);
    take.fade_out = Beats::new(1);
    let range = BeatRange::new(Beats::new(4), Beats::new(8));
    let clip = project.new_clip("Take 1", range, ClipContent::Audio(take));
    project.track_mut(audio).unwrap().clips.push(clip);
    let mut notes = MidiClip::default();
    notes.add_note(Beats::ZERO, Beats::new(1), 60, 0.5);
    let bent = notes.add_note(Beats::new(1), Beats::new(2), 64, 1.0);
    bent.expression.pitch_bend = vec![
        Point::new(Beats::ZERO, 0.0),
        Point::new(Beats::from_ratio(1, 2), 2.0),
    ];
    bent.expression.pressure = vec![Point::new(Beats::ZERO, 0.25)];
    let pedal = vec![Point::new(Beats::ZERO, 1.0), Point::new(Beats::new(1), 0.0)];
    notes.set_controller(Controller::SUSTAIN, pedal);
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    let clip = project.new_clip("Notes", range, ClipContent::Midi(notes));
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let track = project.track_mut(midi).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    project
}

/// Plugins: a MIDI track with a CLAP instrument, set by a state and two
/// parameters after it, and a VST3 effect as it starts on the master.
fn plugged() -> Project {
    let mut project = Project::new();
    let midi = project.add_track("MIDI", TrackKind::Midi);
    let synth = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "org.example.synth".into(),
        name: "Synth".into(),
        instrument: true,
        settings: PluginSettings {
            state: Some(PluginState::new(*b"preset")),
            params: vec![
                PluginParam { id: 2, value: 0.5 },
                PluginParam { id: 9, value: -3.0 },
            ],
        },
    }));
    project.track_mut(midi).unwrap().devices.push(synth);
    let effect = project.new_device(DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Vst3,
        id: "0123456789ABCDEF0123456789ABCDEF".into(),
        name: "Effect".into(),
        instrument: false,
        settings: PluginSettings::default(),
    }));
    project.master.devices.push(effect);
    project
}

/// Where the instrument's settings are in [`plugged`]'s file.
const PLUGIN: &str = "tracks[0].devices[0].kind.plugin";

/// [`plugged`]'s file, changed by `edit`, which gets the instrument's
/// settings.
fn edited_plugin(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&plugged(), |f| {
        edit(&mut f["tracks"][0]["devices"][0]["kind"]["plugin"]);
    })
}

/// Built-in devices: a sampler with a sample on a MIDI track, and an EQ
/// with a band turned down on the master.
fn built_in() -> Project {
    let mut project = Project::new();
    let midi = project.add_track("MIDI", TrackKind::Midi);
    let mut kick = NativeDevice::new(NativeKind::Sampler);
    kick.sample = Some("Samples/Kick.wav".into());
    kick.set(sampler::ROOT, 36.0);
    let kick = project.new_device(DeviceKind::Native(kick));
    project.track_mut(midi).unwrap().devices.push(kick);
    let mut tone = NativeDevice::new(NativeKind::Eq);
    tone.set(eq::GAIN, -3.0);
    let tone = project.new_device(DeviceKind::Native(tone));
    project.master.devices.push(tone);
    project
}

/// Where the sampler's settings are in [`built_in`]'s file.
const NATIVE: &str = "tracks[0].devices[0].kind.native";

/// [`built_in`]'s file, changed by `edit`, which gets the sampler's
/// settings.
fn edited_native(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&built_in(), |f| {
        edit(&mut f["tracks"][0]["devices"][0]["kind"]["native"]);
    })
}

/// Where the audio clip's fields are in [`clips`]'s file.
const AUDIO: &str = "tracks[0].clips[0].content.audio";

/// Where the MIDI clip's fields are in [`clips`]'s file.
const MIDI: &str = "tracks[1].clips[0].content.midi";

/// [`clips`]'s file, changed by `edit`, which gets the audio clip's
/// content.
fn edited_audio(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&clips(), |f| {
        edit(&mut f["tracks"][0]["clips"][0]["content"]["audio"])
    })
}

/// [`clips`]'s file, changed by `edit`, which gets the MIDI clip's notes.
fn edited_notes(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&clips(), |f| {
        edit(&mut f["tracks"][1]["clips"][0]["content"]["midi"]["notes"]);
    })
}

/// [`clips`]'s file, changed by `edit`, which gets the MIDI clip's
/// controller curves.
fn edited_controllers(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&clips(), |f| {
        edit(&mut f["tracks"][1]["clips"][0]["content"]["midi"]["controllers"]);
    })
}

/// The sample's file, changed by `edit`.
fn edited(edit: impl FnOnce(&mut Value)) -> Value {
    edited_from(&sample(), edit)
}

/// `project`'s file, changed by `edit`.
fn edited_from(project: &Project, edit: impl FnOnce(&mut Value)) -> Value {
    let mut file = serde_json::from_str(&project::to_string(project).unwrap()).unwrap();
    edit(&mut file);
    file
}

fn load(file: &Value) -> Result<Project, LoadError> {
    project::from_str(&file.to_string())
}

/// Loading `file` fails at `path`, with a message that includes `message`.
#[track_caller]
fn refused(file: &Value, path: &str, message: &str) {
    match load(file) {
        Err(LoadError::Invalid(invalid)) => {
            assert_eq!(invalid.path, path, "{invalid}");
            assert!(invalid.message.contains(message), "{invalid}");
        }
        other => panic!("expected {path} to be refused, got {other:?}"),
    }
}

/// Saving `project` fails at `path`, with a message that includes `message`.
#[track_caller]
fn refused_saving(project: &Project, path: &str, message: &str) {
    match project::to_string(project) {
        Err(SaveError::Invalid(invalid)) => {
            assert_eq!(invalid.path, path, "{invalid}");
            assert!(invalid.message.contains(message), "{invalid}");
        }
        other => panic!("expected {path} to be refused, got {other:?}"),
    }
}

#[test]
fn the_samples_load() {
    assert_eq!(load(&edited(|_| {})).unwrap(), sample());
    assert_eq!(load(&edited_from(&mix(), |_| {})).unwrap(), mix());
    assert_eq!(load(&edited_from(&clips(), |_| {})).unwrap(), clips());
}

#[test]
fn files_that_arent_projects() {
    assert!(matches!(
        project::from_str(r#"{"format": "#),
        Err(LoadError::Syntax(_))
    ));
    // UTF-16, as some Windows tools save text.
    assert!(matches!(
        project::from_slice(b"\xFF\xFE{\0}\0"),
        Err(LoadError::Syntax(_))
    ));
    for text in [
        "[]",
        "{}",
        r#""musicable-project""#,
        r#"{"format": "musicable-keymap", "version": 1}"#,
    ] {
        assert!(
            matches!(project::from_str(text), Err(LoadError::NotAProject)),
            "{text}"
        );
    }
}

#[test]
fn versions_are_whole_numbers_up_to_this_builds() {
    refused(
        &edited(|f| {
            f.as_object_mut().unwrap().remove("version");
        }),
        "version",
        "whole number",
    );
    for version in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null)] {
        refused(
            &edited(|f| f["version"] = version),
            "version",
            "whole number",
        );
    }
    for newer in [u64::from(VERSION) + 1, u64::from(u32::MAX) + 1, u64::MAX] {
        match load(&edited(|f| f["version"] = newer.into())) {
            Err(LoadError::NewerVersion { version }) => assert_eq!(version, newer),
            other => panic!("expected version {newer} to be too new, got {other:?}"),
        }
    }
}

#[test]
fn byte_order_marks_and_windows_line_endings_are_fine() {
    let text = project::to_string(&sample()).unwrap().replace('\n', "\r\n");
    let bytes = [b"\xEF\xBB\xBF", text.as_bytes()].concat();
    assert_eq!(project::from_slice(&bytes).unwrap(), sample());
}

#[test]
fn every_field_is_needed_and_no_others_are_allowed() {
    refused(
        &edited(|f| f["tempi"] = json!([])),
        "tempi",
        "unknown field",
    );
    refused(
        &edited(|f| f["tracks"][1]["volume"] = json!(1.0)),
        "tracks[1].volume",
        "unknown field",
    );
    refused(
        &edited(|f| f["master"]["width"] = json!(1.0)),
        "master.width",
        "unknown field",
    );
    refused(
        &edited_from(&mix(), |f| f["tracks"][0]["sends"][0]["post"] = json!(true)),
        "tracks[0].sends[0].post",
        "unknown field",
    );
    refused(
        &edited_from(&mix(), |f| {
            f["tracks"][0]["devices"][0]["kind"]["test_gain"]["drive"] = json!(1.0);
        }),
        "tracks[0].devices[0].kind.test_gain.drive",
        "unknown field",
    );
    refused(
        &edited(|f| f["tracks"][0]["console"]["seed"] = json!(1)),
        "tracks[0].console.seed",
        "unknown field",
    );
    refused(
        &edited(|f| {
            f.as_object_mut().unwrap().remove("looping");
        }),
        "",
        "missing field `looping`",
    );
    refused(
        &edited(|f| {
            f["tracks"][0].as_object_mut().unwrap().remove("gain");
        }),
        "tracks[0]",
        "missing field `gain`",
    );
    // Even ones that can be null.
    for field in ["tone", "output", "input"] {
        refused(
            &edited(|f| {
                f["tracks"][1].as_object_mut().unwrap().remove(field);
            }),
            "tracks[1]",
            &format!("missing field `{field}`"),
        );
    }
    let file = edited(|f| f["tracks"][0]["tone"] = Value::Null);
    assert_eq!(load(&file).unwrap().tracks[0].tone, None);
}

#[test]
fn values_must_have_the_right_type() {
    let cases = [
        ("tracks", 0, "gain", json!("loud"), "expected f32"),
        ("tracks", 0, "kind", json!("bus"), "unknown variant `bus`"),
        (
            "tracks",
            1,
            "clip_type",
            json!("tape"),
            "unknown variant `tape`",
        ),
        ("tracks", 1, "output", json!("master"), "expected u32"),
        ("tracks", 1, "solo", json!(null), "expected a boolean"),
        ("tracks", 1, "muted", json!(1), "expected a boolean"),
        ("tracks", 0, "gain", json!(1e39), "out of range"),
        ("tracks", 0, "id", json!(-1), "expected u32"),
    ];
    for (list, i, field, value, message) in cases {
        let path = format!("{list}[{i}].{field}");
        refused(&edited(|f| f[list][i][field] = value), &path, message);
    }
    refused(
        &edited(|f| f["tempo"]["tempos"][0]["at"] = json!(0.5)),
        "tempo.tempos[0].at",
        "expected i64",
    );
    refused(
        &edited(|f| f["tempo"]["signatures"][1]["numerator"] = json!(256)),
        "tempo.signatures[1].numerator",
        "expected u8",
    );
    refused(
        &edited(|f| f["tracks"] = json!({})),
        "tracks",
        "expected a sequence",
    );
    refused(
        &edited_from(&mix(), |f| {
            f["tracks"][0]["devices"][0]["kind"] = json!({"vst2": {}});
        }),
        "tracks[0].devices[0].kind",
        "unknown variant `vst2`",
    );
    refused(
        &edited_from(&mix(), |f| {
            f["master"]["devices"][0]["kind"]["test_gain"]["gain"] = json!("loud");
        }),
        "master.devices[0].kind.test_gain.gain",
        "expected f32",
    );
}

#[test]
fn a_field_given_twice_is_refused() {
    let text = project::to_string(&sample()).unwrap().replacen(
        "\"looping\": false,",
        "\"looping\": false,\n  \"looping\": true,",
        1,
    );
    match project::from_str(&text) {
        Err(LoadError::Invalid(invalid)) => {
            assert!(
                invalid.message.contains("duplicate field `looping`"),
                "{invalid}"
            );
        }
        other => panic!("expected the repeated field to be refused, got {other:?}"),
    }
}

#[test]
fn errors_say_where_the_problem_is() {
    let text =
        project::to_string(&sample())
            .unwrap()
            .replacen("\"gain\": 1.0", "\"gain\": true", 1);
    let line = 1 + text.lines().position(|l| l.contains("true")).unwrap();
    let error = project::from_str(&text).unwrap_err().to_string();
    assert!(
        error.starts_with(&format!(
            "tracks[0].gain: invalid type: boolean `true`, expected f32 at line {line} column"
        )),
        "{error}"
    );
}

#[test]
fn ticks_per_beat_is_fixed() {
    refused(
        &edited(|f| f["ticks_per_beat"] = json!(960)),
        "ticks_per_beat",
        "must be 322560",
    );
}

#[test]
fn tempo_changes_start_at_zero_and_go_in_order() {
    refused(
        &edited(|f| f["tempo"]["tempos"] = json!([])),
        "tempo.tempos",
        "at least one",
    );
    refused(
        &edited(|f| f["tempo"]["tempos"][0]["at"] = json!(10)),
        "tempo.tempos[0].at",
        "must be 0",
    );
    refused(
        &edited(|f| f["tempo"]["tempos"][1]["at"] = json!(0)),
        "tempo.tempos[1].at",
        "after the tempo change before it",
    );
    refused(
        &edited(|f| {
            let tempos = f["tempo"]["tempos"].as_array_mut().unwrap();
            tempos.push(json!({"at": 100, "bpm": 100.0}));
        }),
        "tempo.tempos[2].at",
        "after the tempo change before it",
    );
    refused(
        &edited(|f| f["tempo"]["tempos"][1]["at"] = json!(MAX_TICKS + 1)),
        "tempo.tempos[1].at",
        "within",
    );
    for bpm in [19.99, 999.01, -120.0] {
        refused(
            &edited(|f| f["tempo"]["tempos"][1]["bpm"] = json!(bpm)),
            "tempo.tempos[1].bpm",
            "must be from 20 to 999",
        );
    }
    let edge = edited(|f| {
        f["tempo"]["tempos"][0]["bpm"] = json!(20.0);
        f["tempo"]["tempos"][1]["bpm"] = json!(999.0);
        f["tempo"]["tempos"][1]["at"] = json!(MAX_TICKS);
    });
    assert!(load(&edge).is_ok());
}

#[test]
fn time_signatures_start_at_bar_zero_and_go_in_order() {
    refused(
        &edited(|f| f["tempo"]["signatures"] = json!([])),
        "tempo.signatures",
        "at least one",
    );
    refused(
        &edited(|f| f["tempo"]["signatures"][0]["bar"] = json!(1)),
        "tempo.signatures[0].bar",
        "must be 0",
    );
    refused(
        &edited(|f| f["tempo"]["signatures"][1]["bar"] = json!(0)),
        "tempo.signatures[1].bar",
        "after the time signature change before it",
    );
    for (numerator, denominator) in [(0, 4), (4, 0), (4, 3), (4, 128)] {
        let file = edited(|f| {
            f["tempo"]["signatures"][1]["numerator"] = json!(numerator);
            f["tempo"]["signatures"][1]["denominator"] = json!(denominator);
        });
        refused(&file, "tempo.signatures[1]", "isn't a time signature");
    }

    // Bar 2 starts at beat 8, and then each bar of 6/8 is three beats long.
    let furthest = 2 + (MAX_TICKS - Beats::new(8).ticks()) / Beats::new(3).ticks();
    let third_at = |bar: i64| {
        edited(|f| {
            let signatures = f["tempo"]["signatures"].as_array_mut().unwrap();
            signatures.push(json!({"bar": bar, "numerator": 4, "denominator": 4}));
        })
    };
    assert!(load(&third_at(furthest)).is_ok());
    for bar in [furthest + 1, i64::MAX] {
        refused(
            &third_at(bar),
            "tempo.signatures[2].bar",
            "starts more than",
        );
    }
}

#[test]
fn positions_stay_within_reach() {
    for (field, ticks) in [
        ("start", MAX_TICKS + 1),
        ("end", -MAX_TICKS - 1),
        ("start", i64::MIN),
    ] {
        let file = edited(|f| f["loop_region"][field] = json!(ticks));
        refused(&file, &format!("loop_region.{field}"), "within");
    }
    // A backwards loop region doesn't loop, but it's allowed.
    let file = edited(|f| f["loop_region"] = json!({"start": MAX_TICKS, "end": -MAX_TICKS}));
    assert_eq!(
        load(&file).unwrap().loop_region,
        BeatRange::new(Beats::from_ticks(MAX_TICKS), Beats::from_ticks(-MAX_TICKS))
    );
}

#[test]
fn track_ids_are_unique_and_below_the_next_id() {
    refused(
        &edited(|f| f["tracks"][1]["id"] = json!(0)),
        "tracks[1].id",
        "tracks[0] has id 0 too",
    );
    refused(
        &edited(|f| f["next_track_id"] = json!(1)),
        "next_track_id",
        "a track has id 1",
    );
    refused(
        &edited(|f| f["tracks"][1]["id"] = json!(u32::MAX)),
        "next_track_id",
        "a track has id 4294967295",
    );
    // Gaps and any order are fine.
    let file = edited(|f| {
        f["tracks"][0]["id"] = json!(7);
        f["next_track_id"] = json!(9);
    });
    let project = load(&file).unwrap();
    assert_eq!(project.tracks[0].id, TrackId(7));
    assert_eq!(project.tracks[1].id, TrackId(1));
    assert_eq!(project.next_track_id, 9);
}

#[test]
fn pan_is_from_minus_one_to_one() {
    for pan in [1.0001, -1.5, 1e30] {
        refused(
            &edited(|f| f["tracks"][1]["pan"] = json!(pan)),
            "tracks[1].pan",
            "must be from -1 to 1",
        );
        refused(
            &edited(|f| f["master"]["pan"] = json!(pan)),
            "master.pan",
            "must be from -1 to 1",
        );
    }
    let edges = edited(|f| {
        f["tracks"][0]["pan"] = json!(-1.0);
        f["tracks"][1]["pan"] = json!(1.0);
    });
    let project = load(&edges).unwrap();
    assert_eq!((project.tracks[0].pan, project.tracks[1].pan), (-1.0, 1.0));
}

#[test]
fn outputs_feed_group_tracks_without_loops() {
    let cases = [
        (0, json!(99), "there's no track with id 99"),
        (0, json!(1), "track 1 isn't a group track"),
        (0, json!(4), "track 4 isn't a group track"),
        (2, json!(2), "can't be the track's own id"),
        (4, json!(2), "must be null: return tracks feed the master"),
    ];
    for (track, output, message) in cases {
        let file = edited_from(&mix(), |f| f["tracks"][track]["output"] = output);
        refused(&file, &format!("tracks[{track}].output"), message);
    }
    // The outer group back into the inner one.
    refused(
        &edited_from(&mix(), |f| f["tracks"][3]["output"] = json!(2)),
        "tracks[2].output",
        "makes a loop: group track 3 feeds this track",
    );
    // Any track can feed a group, even one further up the list.
    let file = edited_from(&mix(), |f| f["tracks"][1]["output"] = json!(2));
    assert_eq!(
        load(&file).unwrap().tracks[1].output,
        Bus::Group(TrackId(2))
    );
}

#[test]
fn sends_feed_return_tracks_once_each() {
    let send = |f: &mut Value, track: usize, id: u32, to: u32| {
        let send = json!({"id": id, "to": to, "level": 1.0, "pre_fader": true});
        f["tracks"][track]["sends"]
            .as_array_mut()
            .unwrap()
            .push(send);
        f["next_send_id"] = json!(9);
    };
    let cases: [(usize, u32, &str, &str); 3] = [
        (
            2,
            99,
            "tracks[2].sends[0].to",
            "there's no track with id 99",
        ),
        (
            2,
            3,
            "tracks[2].sends[0].to",
            "track 3 isn't a return track",
        ),
        (
            0,
            4,
            "tracks[0].sends[1].to",
            "sends[0] goes to track 4 too",
        ),
    ];
    for (track, to, path, message) in cases {
        refused(
            &edited_from(&mix(), |f| send(f, track, 5, to)),
            path,
            message,
        );
    }
    refused(
        &edited_from(&mix(), |f| send(f, 4, 5, 4)),
        "tracks[4].sends",
        "must be empty: return tracks have no sends",
    );
    refused(
        &edited_from(&mix(), |f| send(f, 3, 0, 4)),
        "tracks[3].sends[0].id",
        "tracks[0].sends[0] has id 0 too",
    );
    refused(
        &edited_from(&mix(), |f| f["next_send_id"] = json!(0)),
        "next_send_id",
        "a send has id 0",
    );
    // Groups and MIDI tracks send too.
    let file = edited_from(&mix(), |f| {
        send(f, 1, 5, 4);
        send(f, 3, 6, 4);
    });
    let project = load(&file).unwrap();
    assert_eq!(project.tracks[3].sends[0].to, TrackId(4));
}

#[test]
fn device_ids_are_unique_across_tracks_and_the_master() {
    refused(
        &edited_from(&mix(), |f| f["master"]["devices"][0]["id"] = json!(0)),
        "master.devices[0].id",
        "tracks[0].devices[0] has id 0 too",
    );
    refused(
        &edited_from(&mix(), |f| f["next_device_id"] = json!(1)),
        "next_device_id",
        "a device has id 1",
    );
}

#[test]
fn plugins_name_their_plugin_and_keep_its_settings_in_order() {
    assert_eq!(load(&edited_plugin(|_| {})).unwrap(), plugged());
    let at = |field: &str| format!("{PLUGIN}.{field}");
    refused(
        &edited_plugin(|p| p["format"] = json!("au")),
        &at("format"),
        "unknown variant `au`",
    );
    refused(
        &edited_plugin(|p| p["id"] = json!("")),
        &at("id"),
        "must not be empty",
    );
    refused(
        &edited_plugin(|p| p["instrument"] = json!("yes")),
        &at("instrument"),
        "expected a boolean",
    );
    refused(
        &edited_plugin(|p| p["state"] = json!("not base64!")),
        &at("state"),
        "must be base64",
    );
    refused(
        &edited_plugin(|p| p["state"] = json!([1, 2, 3])),
        &at("state"),
        "expected a string",
    );
    refused(
        &edited_plugin(|p| p["bypassed"] = json!(true)),
        &at("bypassed"),
        "unknown field",
    );
    // Even the state, which can be null.
    refused(
        &edited_plugin(|p| {
            p.as_object_mut().unwrap().remove("state");
        }),
        PLUGIN,
        "missing field `state`",
    );
    for params in [json!([[5, 0.5], [5, 0.25]]), json!([[5, 0.5], [2, 0.25]])] {
        refused(
            &edited_plugin(|p| p["params"] = params),
            &at("params[1][0]"),
            "must be after the parameter before it: in order of id, once each",
        );
    }
    refused(
        &edited_plugin(|p| p["params"] = json!([[4_294_967_296_u64, 0.5]])),
        &at("params[0][0]"),
        "expected u32",
    );
    refused(
        &edited_plugin(|p| p["params"] = json!([[1, "loud"]])),
        &at("params[0][1]"),
        "expected f64",
    );
    refused(
        &edited_plugin(|p| p["params"] = json!([[1]])),
        &at("params[0]"),
        "invalid length 1",
    );

    // Any name, and a state of no bytes.
    let file = edited_plugin(|p| {
        p["name"] = json!("");
        p["state"] = json!("");
    });
    let project = load(&file).unwrap();
    let Some(plugin) = project.tracks[0].devices[0].kind.plugin() else {
        panic!("a plugin");
    };
    assert_eq!(plugin.name, "");
    assert_eq!(plugin.settings.state, Some(PluginState::new([])));
}

#[test]
fn built_in_devices_set_their_parameters_by_key() {
    assert_eq!(load(&edited_native(|_| {})).unwrap(), built_in());
    let at = |field: &str| format!("{NATIVE}.{field}");
    refused(
        &edited_native(|n| n["kind"] = json!("flanger")),
        &at("kind"),
        "unknown variant `flanger`",
    );
    refused(
        &edited_native(|n| n["params"]["loudness"] = json!(1.0)),
        &at("params.loudness"),
        "isn't one of the Sampler's parameters",
    );
    for root in [json!(128), json!(60.5), json!(-1)] {
        refused(
            &edited_native(|n| n["params"]["root"] = root),
            &at("params.root"),
            "must be a whole number from 0 to 127",
        );
    }
    refused(
        &edited_native(|n| n["params"]["attack"] = json!(0.0)),
        &at("params.attack"),
        "must be a number from 0.1 to 20000",
    );
    refused(
        &edited_native(|n| n["params"]["attack"] = json!("fast")),
        &at("params.attack"),
        "expected f64",
    );
    refused(
        &edited_native(|n| n["params"] = json!([1.0])),
        &at("params"),
        "expected an object of parameter values",
    );
    refused(
        &edited_native(|n| n["sample"] = json!("")),
        &at("sample"),
        "must name a file",
    );
    refused(
        &edited_native(|n| {
            n.as_object_mut().unwrap().remove("sample");
        }),
        NATIVE,
        "missing field `sample`",
    );
    refused(
        &edited_from(&built_in(), |f| {
            f["master"]["devices"][0]["kind"]["native"]["sample"] = json!("Kick.wav");
        }),
        "master.devices[0].kind.native.sample",
        "must be null: only samplers have samples",
    );
    // JSON objects can hold a key twice, but a device can't.
    let text = project::to_string(&built_in()).unwrap();
    let twice = text.replacen("\"root\": 36.0,", "\"root\": 36.0, \"root\": 40.0,", 1);
    match project::from_str(&twice) {
        Err(LoadError::Invalid(invalid)) => {
            assert_eq!(invalid.path, at("params.root"));
            assert!(invalid.message.contains("is given twice"), "{invalid}");
        }
        other => panic!("a parameter given twice loaded: {other:?}"),
    }

    // A parameter that isn't there takes its default, so kinds can gain
    // parameters without a new version of the format.
    let file = edited_native(|n| {
        let params = n["params"].as_object_mut().unwrap();
        params.remove("volume");
        params.remove("root");
    });
    let project = load(&file).unwrap();
    let kick = project.tracks[0].devices[0].kind.native().unwrap();
    let defaults = NativeDevice::new(NativeKind::Sampler);
    assert_eq!(kick.values, defaults.values);

    // Saving checks the same rules.
    let mut project = built_in();
    let kick = project.tracks[0].devices[0].kind.native_mut().unwrap();
    kick.values[sampler::ROOT] = 200.0;
    refused_saving(&project, &at("params.root"), "from 0 to 127");
    let mut project = built_in();
    let tone = project.master.devices[0].kind.native_mut().unwrap();
    tone.values[eq::FREQ] = f64::NAN;
    refused_saving(
        &project,
        "master.devices[0].kind.native.params.band1_freq",
        "must be a number from 10 to 22000",
    );
}

#[test]
fn only_audio_and_midi_tracks_record() {
    let mut project = mix();
    project.tracks[0].armed = true;
    project.tracks[1].armed = true;
    assert_eq!(load(&edited_from(&project, |_| {})).unwrap(), project);
    for track in [2, 4] {
        refused(
            &edited_from(&mix(), |f| f["tracks"][track]["armed"] = json!(true)),
            &format!("tracks[{track}].armed"),
            "must be false: only audio and MIDI tracks record",
        );
    }
}

#[test]
fn clips_and_notes_have_every_field_and_no_others() {
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][0]["clips"][0]["warp"] = json!(true)
        }),
        "tracks[0].clips[0].warp",
        "unknown field",
    );
    refused(
        &edited_audio(|a| a["loop"] = json!(true)),
        &format!("{AUDIO}.loop"),
        "unknown field",
    );
    refused(
        &edited_notes(|n| n[0]["channel"] = json!(1)),
        &format!("{MIDI}.notes[0].channel"),
        "unknown field",
    );
    refused(
        &edited_notes(|n| n[1]["expression"]["brightness"] = json!([])),
        &format!("{MIDI}.notes[1].expression.brightness"),
        "unknown field",
    );
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][1]["clips"][0]["content"]["midi"]
                .as_object_mut()
                .unwrap()
                .remove("next_note_id");
        }),
        MIDI,
        "missing field `next_note_id`",
    );
    refused(
        &edited_notes(|n| {
            n[0].as_object_mut().unwrap().remove("release_velocity");
        }),
        &format!("{MIDI}.notes[0]"),
        "missing field `release_velocity`",
    );
    // Even curves without points.
    refused(
        &edited_notes(|n| {
            n[0]["expression"].as_object_mut().unwrap().remove("timbre");
        }),
        &format!("{MIDI}.notes[0].expression"),
        "missing field `timbre`",
    );
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][1]["clips"][0]["content"]["midi"]
                .as_object_mut()
                .unwrap()
                .remove("controllers");
        }),
        MIDI,
        "missing field `controllers`",
    );
    refused(
        &edited_controllers(|c| c[0]["channel"] = json!(0)),
        &format!("{MIDI}.controllers[0].channel"),
        "unknown field",
    );
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][1]["devices"][0]["kind"]["test_instrument"] = json!({});
        }),
        "tracks[1].devices[0].kind.test_instrument",
        "missing field `level`",
    );
}

#[test]
fn clip_values_must_have_the_right_type() {
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][0]["clips"][0]["content"] = json!({"video": {}});
        }),
        "tracks[0].clips[0].content",
        "unknown variant `video`",
    );
    refused(
        &edited_audio(|a| a["offset"] = json!(-1)),
        &format!("{AUDIO}.offset"),
        "expected u64",
    );
    refused(
        &edited_notes(|n| n[0]["key"] = json!(256)),
        &format!("{MIDI}.notes[0].key"),
        "expected u8",
    );
    refused(
        &edited_notes(|n| n[1]["expression"]["pitch_bend"][1] = json!({"at": 0, "value": 1.0})),
        &format!("{MIDI}.notes[1].expression.pitch_bend[1]"),
        "expected a tuple of size 2",
    );
    refused(
        &edited_notes(|n| n[1]["expression"]["pitch_bend"][1] = json!([0])),
        &format!("{MIDI}.notes[1].expression.pitch_bend[1]"),
        "expected a tuple of size 2",
    );
    refused(
        &edited_notes(|n| n[1]["expression"]["pressure"][0][0] = json!(0.5)),
        &format!("{MIDI}.notes[1].expression.pressure[0][0]"),
        "expected i64",
    );
}

#[test]
fn clip_ids_are_unique_across_tracks_and_below_the_next_id() {
    refused(
        &edited_from(&clips(), |f| f["tracks"][1]["clips"][0]["id"] = json!(0)),
        "tracks[1].clips[0].id",
        "tracks[0].clips[0] has id 0 too",
    );
    refused(
        &edited_from(&clips(), |f| f["next_clip_id"] = json!(1)),
        "next_clip_id",
        "a clip has id 1",
    );
    // Gaps and any order are fine.
    let file = edited_from(&clips(), |f| {
        f["tracks"][0]["clips"][0]["id"] = json!(7);
        f["next_clip_id"] = json!(9);
    });
    let project = load(&file).unwrap();
    assert_eq!(project.tracks[0].clips[0].id, ClipId(7));
    assert_eq!(project.next_clip_id, 9);
}

/// Recording: an audio track with a take of a clip, a MIDI track with a take
/// of a clip, and a return track. The tracks' ids are their places in the
/// list, and the takes' ids their tracks'.
fn recorded() -> Project {
    let mut project = Project::new();
    let audio = project.add_track("Audio", TrackKind::Audio);
    let midi = project.add_track("MIDI", TrackKind::Midi);
    project.add_track("Reverb", TrackKind::Return);
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    for (track, content) in [
        (
            audio,
            ClipContent::Audio(AudioClip::new("Recordings/Audio 1.wav")),
        ),
        (midi, ClipContent::Midi(MidiClip::default())),
    ] {
        let mut take = project.new_take("Take 1");
        take.clips.push(project.new_clip("Take 1", range, content));
        project.track_mut(track).unwrap().takes.push(take);
    }
    project
}

#[test]
fn inputs_are_of_their_tracks_kind_and_buses_have_none() {
    let mut project = recorded();
    project.tracks[0].input = Some(Input::Audio(AudioInput {
        device: "loopback:{0.0.0.00000000}.{speakers}".into(),
        name: "Speakers (loopback)".into(),
        channel: 65534,
        stereo: true,
    }));
    project.tracks[1].input = None;
    project.tracks[1].monitor = Monitor::Off;
    project.tracks[2].monitor = Monitor::In;
    assert_eq!(load(&edited_from(&project, |_| {})).unwrap(), project);

    let audio = json!({"audio": {"device": "", "name": "", "channel": 0, "stereo": false}});
    let midi = json!({"midi": {"device": "", "name": "", "channel": 9}});
    refused(
        &edited_from(&recorded(), |f| f["tracks"][1]["input"] = audio.clone()),
        "tracks[1].input",
        "must be a MIDI input: MIDI tracks record MIDI",
    );
    refused(
        &edited_from(&recorded(), |f| f["tracks"][0]["input"] = midi.clone()),
        "tracks[0].input",
        "must be an audio input: audio tracks record audio",
    );
    refused(
        &edited_from(&recorded(), |f| f["tracks"][2]["input"] = audio.clone()),
        "tracks[2].input",
        "must be null: buses record nothing",
    );
    refused(
        &edited_from(&recorded(), |f| {
            f["tracks"][0]["input"]["audio"]["channel"] = json!(65535);
        }),
        "tracks[0].input.audio.channel",
        "must be below 65535 for a stereo input",
    );
    refused(
        &edited_from(&recorded(), |f| {
            f["tracks"][1]["input"]["midi"]["channel"] = json!(16);
        }),
        "tracks[1].input.midi.channel",
        "must be from 0 to 15, or null for every channel",
    );
    refused(
        &edited_from(&recorded(), |f| {
            f["tracks"][1]["input"]["midi"]["port"] = json!(1);
        }),
        "tracks[1].input.midi.port",
        "unknown field",
    );
    refused(
        &edited_from(&recorded(), |f| {
            f["tracks"][0]["monitor"] = json!("sometimes")
        }),
        "tracks[0].monitor",
        "unknown variant",
    );
}

#[test]
fn takes_hold_clips_of_their_tracks_kind_with_unique_ids() {
    refused(
        &edited_from(&recorded(), |f| f["tracks"][1]["takes"][0]["id"] = json!(0)),
        "tracks[1].takes[0].id",
        "tracks[0].takes[0] has id 0 too",
    );
    refused(
        &edited_from(&recorded(), |f| f["next_take_id"] = json!(1)),
        "next_take_id",
        "a take has id 1",
    );
    // A take's clips' ids are clip ids like any other's.
    refused(
        &edited_from(&recorded(), |f| {
            f["tracks"][1]["takes"][0]["clips"][0]["id"] = json!(0);
        }),
        "tracks[1].takes[0].clips[0].id",
        "tracks[0].takes[0].clips[0] has id 0 too",
    );
    refused(
        &edited_from(&recorded(), |f| f["next_clip_id"] = json!(1)),
        "next_clip_id",
        "a clip has id 1",
    );
    refused(
        &edited_from(&recorded(), |f| {
            let mut take = f["tracks"][0]["takes"][0].clone();
            take["id"] = json!(2);
            take["clips"] = json!([]);
            f["tracks"][2]["takes"] = json!([take]);
            f["next_take_id"] = json!(3);
        }),
        "tracks[2].takes",
        "must be empty: only audio and MIDI tracks record",
    );
    refused(
        &edited_from(&recorded(), |f| {
            let clip = f["tracks"][1]["takes"][0]["clips"][0]["content"].clone();
            f["tracks"][0]["takes"][0]["clips"][0]["content"] = clip;
        }),
        "tracks[0].takes[0].clips[0].content",
        "must be an audio clip: audio tracks hold audio clips",
    );
    // Gaps and any order are fine.
    let file = edited_from(&recorded(), |f| {
        f["tracks"][0]["takes"][0]["id"] = json!(5);
        f["next_take_id"] = json!(9);
    });
    let project = load(&file).unwrap();
    assert_eq!(project.tracks[0].takes[0].id, TakeId(5));
    assert_eq!(project.next_take_id, 9);
}

/// The sample with two locators, the second at the furthest place a file
/// reaches.
fn located() -> Project {
    let mut project = sample();
    for (name, at) in [
        ("Verse", Beats::new(8)),
        ("End", Beats::from_ticks(MAX_TICKS)),
    ] {
        let locator = project.new_locator(name, at);
        project.locators.push(locator);
    }
    project
}

#[test]
fn locators_have_unique_ids_and_places_within_reach() {
    let file = edited_from(&located(), |_| ());
    assert_eq!(load(&file).unwrap(), located());
    refused(
        &edited_from(&located(), |f| f["locators"][1]["id"] = json!(0)),
        "locators[1].id",
        "locators[0] has id 0 too",
    );
    refused(
        &edited_from(&located(), |f| f["next_locator_id"] = json!(1)),
        "next_locator_id",
        "a locator has id 1",
    );
    refused(
        &edited_from(&located(), |f| {
            f["locators"][1]["at"] = json!(MAX_TICKS + 1)
        }),
        "locators[1].at",
        "within",
    );
    refused(
        &edited_from(&located(), |f| f["locators"][0]["at"] = json!(1.5)),
        "locators[0].at",
        "expected i64",
    );
    refused(
        &edited_from(&located(), |f| f["locators"][0]["colour"] = json!("red")),
        "locators[0].colour",
        "unknown field",
    );
    refused(
        &edited_from(&located(), |f| {
            f["locators"][0].as_object_mut().unwrap().remove("name");
        }),
        "locators[0]",
        "missing field `name`",
    );
    // Gaps, any order, and two in one place are fine.
    let file = edited_from(&located(), |f| {
        f["locators"][0]["id"] = json!(7);
        f["locators"][1]["at"] = f["locators"][0]["at"].clone();
        f["next_locator_id"] = json!(9);
    });
    let project = load(&file).unwrap();
    assert_eq!(project.locators[0].id, LocatorId(7));
    assert_eq!(project.locators[0].at, project.locators[1].at);
    assert_eq!(project.next_locator_id, 9);
    // Saving checks the same.
    let mut project = located();
    project.next_locator_id = 1;
    refused_saving(&project, "next_locator_id", "a locator has id 1");
}

#[test]
fn note_ids_are_unique_in_their_clip_and_below_its_next_note_id() {
    refused(
        &edited_notes(|n| n[1]["id"] = json!(0)),
        &format!("{MIDI}.notes[1].id"),
        &format!("{MIDI}.notes[0] has id 0 too"),
    );
    refused(
        &edited_from(&clips(), |f| {
            f["tracks"][1]["clips"][0]["content"]["midi"]["next_note_id"] = json!(1);
        }),
        &format!("{MIDI}.next_note_id"),
        "a note has id 1",
    );
    // Notes in different clips can have the same ids.
    let mut project = clips();
    let mut copy = project.tracks[1].clips[0].clone();
    copy.id = ClipId(project.next_clip_id);
    project.next_clip_id += 1;
    project.tracks[1].clips.push(copy);
    assert_eq!(load(&edited_from(&project, |_| {})).unwrap(), project);
    let file = edited_notes(|n| {
        n[0]["id"] = json!(1);
        n[1]["id"] = json!(0);
    });
    let notes = &load(&file).unwrap().tracks[1].clips[0];
    let ClipContent::Midi(midi) = &notes.content else {
        panic!("a MIDI clip");
    };
    assert_eq!(midi.notes[0].id, NoteId(1));
    assert_eq!(midi.note(NoteId(0)).unwrap().key, 64);
}

#[test]
fn clips_go_on_tracks_of_their_kind() {
    let file = edited_from(&clips(), |_| {});
    let audio = file["tracks"][0]["clips"][0]["content"].clone();
    let midi = file["tracks"][1]["clips"][0]["content"].clone();
    refused(
        &edited_from(&clips(), |f| f["tracks"][1]["clips"][0]["content"] = audio),
        "tracks[1].clips[0].content",
        "must be a MIDI clip: MIDI tracks hold MIDI clips",
    );
    refused(
        &edited_from(&clips(), |f| f["tracks"][0]["clips"][0]["content"] = midi),
        "tracks[0].clips[0].content",
        "must be an audio clip: audio tracks hold audio clips",
    );
    for kind in ["group", "return"] {
        let file = edited_from(&clips(), |f| {
            f["tracks"][1]["kind"] = json!(kind);
        });
        refused(
            &file,
            "tracks[1].clips",
            "must be empty: only audio and MIDI tracks hold clips",
        );
    }
}

#[test]
fn clips_start_in_the_song_and_end_within_reach() {
    let clip = |f: &mut Value, field: &str, ticks: i64| {
        f["tracks"][1]["clips"][0][field] = json!(ticks);
    };
    let cases = [
        ("start", -1, "must be 0 or more: clips start in the song"),
        ("start", MAX_TICKS + 1, "within"),
        ("length", 0, "must be more than 0"),
        ("length", -Beats::new(4).ticks(), "must be more than 0"),
        ("length", MAX_TICKS + 1, "must end within"),
    ];
    for (field, ticks, message) in cases {
        let file = edited_from(&clips(), |f| clip(f, field, ticks));
        refused(&file, &format!("tracks[1].clips[0].{field}"), message);
    }
    // The clip is four beats long.
    let length = Beats::new(4).ticks();
    let file = edited_from(&clips(), |f| clip(f, "start", MAX_TICKS - length + 1));
    refused(&file, "tracks[1].clips[0].length", "must end within");
    let file = edited_from(&clips(), |f| clip(f, "start", MAX_TICKS - length));
    assert_eq!(
        load(&file).unwrap().tracks[1].clips[0].end(),
        Beats::from_ticks(MAX_TICKS)
    );
}

#[test]
fn audio_clips_name_a_file_and_their_fades_fit() {
    refused(
        &edited_audio(|a| a["file"] = json!("")),
        &format!("{AUDIO}.file"),
        "must name a file",
    );
    refused(
        &edited_audio(|a| a["offset"] = json!(MAX_TICKS + 1)),
        &format!("{AUDIO}.offset"),
        "or less",
    );
    // The clip is four beats long.
    let length = Beats::new(4).ticks();
    for (field, ticks) in [("fade_in", -1), ("fade_out", length + 1)] {
        refused(
            &edited_audio(|a| a[field] = json!(ticks)),
            &format!("{AUDIO}.{field}"),
            "must be from 0 to the clip's length",
        );
    }
    refused(
        &edited_audio(|a| {
            a["fade_in"] = json!(length / 2 + 1);
            a["fade_out"] = json!(length / 2);
        }),
        &format!("{AUDIO}.fade_out"),
        "must fit in the clip's length with fade_in",
    );
    let file = edited_audio(|a| {
        a["fade_in"] = json!(length / 2);
        a["fade_out"] = json!(length / 2);
        a["offset"] = json!(MAX_TICKS);
        a["file"] = json!("C:/Samples/Take.wav");
    });
    let ClipContent::Audio(audio) = &load(&file).unwrap().tracks[0].clips[0].content else {
        panic!("an audio clip");
    };
    assert_eq!(audio.fade_in + audio.fade_out, Beats::from_ticks(length));
    assert_eq!(audio.offset, MAX_TICKS as u64);
    assert_eq!(audio.file, "C:/Samples/Take.wav");
}

#[test]
fn notes_have_keys_velocities_and_lengths() {
    let cases = [
        ("key", json!(128), "must be from 0 to 127"),
        ("velocity", json!(1.01), "must be from 0 to 1"),
        ("velocity", json!(-0.5), "must be from 0 to 1"),
        ("release_velocity", json!(2.0), "must be from 0 to 1"),
        ("length", json!(0), "must be more than 0"),
        ("start", json!(-MAX_TICKS - 1), "within"),
        ("length", json!(MAX_TICKS), "must end within"),
    ];
    for (field, value, message) in cases {
        refused(
            &edited_notes(|n| n[1][field] = value),
            &format!("{MIDI}.notes[1].{field}"),
            message,
        );
    }
    // Notes before the clip's offset don't play, but they're kept.
    let file = edited_notes(|n| {
        n[0]["start"] = json!(-MAX_TICKS);
        n[0]["key"] = json!(0);
        n[0]["velocity"] = json!(0.0);
        n[1]["key"] = json!(127);
        n[1]["release_velocity"] = json!(1.0);
    });
    let project = load(&file).unwrap();
    let ClipContent::Midi(midi) = &project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    assert_eq!(midi.notes[0].start, Beats::from_ticks(-MAX_TICKS));
    assert_eq!((midi.notes[0].key, midi.notes[1].key), (0, 127));
}

#[test]
fn expression_points_go_in_order_within_range() {
    let curve = |n: &mut Value, name: &str, points: Value| {
        n[1]["expression"][name] = points;
    };
    let cases = [
        (
            "pitch_bend",
            json!([[-1, 0.0]]),
            "pitch_bend[0][0]",
            "must be 0 or more: points start from the note's start",
        ),
        (
            "pitch_bend",
            json!([[10, 0.0], [5, 1.0]]),
            "pitch_bend[1][0]",
            "must be at or after the point before it",
        ),
        (
            "pitch_bend",
            json!([[0, 0.0], [10, 120.5]]),
            "pitch_bend[1][1]",
            "must be from -120 to 120",
        ),
        (
            "pressure",
            json!([[0, 1.5]]),
            "pressure[0][1]",
            "must be from 0 to 1",
        ),
        (
            "timbre",
            json!([[0, -0.25]]),
            "timbre[0][1]",
            "must be from 0 to 1",
        ),
        (
            "timbre",
            json!([[MAX_TICKS + 1, 0.0]]),
            "timbre[0][0]",
            "within",
        ),
    ];
    for (name, points, path, message) in cases {
        refused(
            &edited_notes(|n| curve(n, name, points)),
            &format!("{MIDI}.notes[1].expression.{path}"),
            message,
        );
    }
    // Points at the same time are fine: the later one wins. So are points
    // after the note's end, which don't play.
    let file = edited_notes(|n| {
        curve(
            n,
            "pitch_bend",
            json!([[0, -120.0], [0, 120.0], [MAX_TICKS, 0.0]]),
        );
    });
    let project = load(&file).unwrap();
    let ClipContent::Midi(midi) = &project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    let bend = &midi.notes[1].expression.pitch_bend;
    assert_eq!(bend.len(), 3);
    assert_eq!(bend[1], Point::new(Beats::ZERO, 120.0));
}

#[test]
fn controllers_are_midi_s_each_moved_once_with_points_in_order() {
    let cases = [
        (
            json!([{"controller": 120, "points": [[0, 1.0]]}]),
            "controllers[0].controller",
            "must be from 0 to 119",
        ),
        (
            json!([
                {"controller": 64, "points": [[0, 1.0]]},
                {"controller": 64, "points": [[0, 0.0]]},
            ]),
            "controllers[1].controller",
            "the clip moves controller 64 twice",
        ),
        (
            json!([{"controller": 1, "points": []}]),
            "controllers[0].points",
            "must have a point",
        ),
        (
            json!([{"controller": 1, "points": [[10, 0.0], [5, 1.0]]}]),
            "controllers[0].points[1][0]",
            "must be at or after the point before it",
        ),
        (
            json!([{"controller": 1, "points": [[0, 1.5]]}]),
            "controllers[0].points[0][1]",
            "must be from 0 to 1",
        ),
        (
            json!([{"controller": 1, "points": [[-MAX_TICKS - 1, 0.0]]}]),
            "controllers[0].points[0][0]",
            "within",
        ),
    ];
    for (controllers, path, message) in cases {
        refused(
            &edited_controllers(|c| *c = controllers),
            &format!("{MIDI}.{path}"),
            message,
        );
    }
    // Points can be before the clip's own time starts, as notes can, and
    // on the same tick, where the later one wins. Curves can come in any
    // order.
    let file = edited_controllers(|c| {
        *c = json!([
            {"controller": 64, "points": [[-MAX_TICKS, 0.0], [0, 0.0], [0, 1.0]]},
            {"controller": 0, "points": [[MAX_TICKS, 0.5]]},
        ]);
    });
    let project = load(&file).unwrap();
    let ClipContent::Midi(midi) = &project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    assert_eq!(
        midi.controller_at(Controller::SUSTAIN, Beats::ZERO),
        Some(1.0)
    );
    let lowest = Controller::new(0).unwrap();
    assert_eq!(
        midi.controller(lowest),
        [Point::new(Beats::from_ticks(MAX_TICKS), 0.5)]
    );
    assert_eq!(
        midi.controllers.keys().copied().collect::<Vec<_>>(),
        [lowest, Controller::SUSTAIN]
    );
}

#[test]
fn saving_refuses_what_loading_would() {
    let mut project = sample();
    project.tracks[1].gain = f32::NAN;
    refused_saving(&project, "tracks[1].gain", "must be a finite number");

    let mut project = sample();
    project.tracks[0].tone.as_mut().unwrap().frequency = f64::INFINITY;
    refused_saving(
        &project,
        "tracks[0].tone.frequency",
        "must be a finite number",
    );

    let mut project = sample();
    project.tracks[0].tone.as_mut().unwrap().level = f32::NEG_INFINITY;
    refused_saving(&project, "tracks[0].tone.level", "must be a finite number");

    let mut project = sample();
    project.tracks[1].id = project.tracks[0].id;
    refused_saving(&project, "tracks[1].id", "tracks[0] has id 0 too");

    let mut project = sample();
    project.next_track_id = 1;
    refused_saving(&project, "next_track_id", "a track has id 1");

    let mut project = sample();
    project.loop_region.end = Beats::from_ticks(i64::MAX);
    refused_saving(&project, "loop_region.end", "within");

    let mut project = sample();
    project
        .tempo
        .insert_tempo(Beats::from_ticks(MAX_TICKS + 1), 120.0);
    refused_saving(&project, "tempo.tempos[2].at", "within");

    let mut project = mix();
    project.tracks[0].pan = f32::NAN;
    refused_saving(&project, "tracks[0].pan", "must be from -1 to 1");

    let mut project = mix();
    project.tracks[0].sends[0].level = f32::INFINITY;
    refused_saving(
        &project,
        "tracks[0].sends[0].level",
        "must be a finite number",
    );

    let mut project = mix();
    project.master.devices[0].kind = DeviceKind::TestGain { gain: f32::NAN };
    refused_saving(
        &project,
        "master.devices[0].kind.test_gain.gain",
        "must be a finite number",
    );

    let mut project = mix();
    project.tracks[3].output = Bus::Group(TrackId(2));
    refused_saving(&project, "tracks[2].output", "makes a loop");

    let mut project = mix();
    project.tracks.remove(4);
    refused_saving(
        &project,
        "tracks[0].sends[0].to",
        "there's no track with id 4",
    );

    let mut project = clips();
    project.tracks[1].devices[0].kind = DeviceKind::TestInstrument {
        level: f32::INFINITY,
    };
    refused_saving(
        &project,
        "tracks[1].devices[0].kind.test_instrument.level",
        "must be a finite number",
    );

    let mut project = clips();
    let ClipContent::Audio(audio) = &mut project.tracks[0].clips[0].content else {
        panic!("an audio clip");
    };
    audio.gain = f32::NAN;
    refused_saving(
        &project,
        &format!("{AUDIO}.gain"),
        "must be a finite number",
    );

    let mut project = clips();
    let ClipContent::Midi(midi) = &mut project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    midi.notes[1].velocity = f32::NAN;
    refused_saving(
        &project,
        &format!("{MIDI}.notes[1].velocity"),
        "must be from 0 to 1",
    );

    let mut project = clips();
    let ClipContent::Midi(midi) = &mut project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    midi.notes[1].expression.pressure[0].value = f32::NAN;
    refused_saving(
        &project,
        &format!("{MIDI}.notes[1].expression.pressure[0][1]"),
        "must be from 0 to 1",
    );

    let mut project = clips();
    let ClipContent::Midi(midi) = &mut project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    midi.controllers.get_mut(&Controller::SUSTAIN).unwrap()[1].value = f32::NAN;
    refused_saving(
        &project,
        &format!("{MIDI}.controllers[0].points[1][1]"),
        "must be from 0 to 1",
    );
    let mut project = clips();
    let ClipContent::Midi(midi) = &mut project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    midi.controllers.insert(Controller::MOD_WHEEL, Vec::new());
    refused_saving(
        &project,
        &format!("{MIDI}.controllers[0].points"),
        "must have a point",
    );

    let mut project = clips();
    let ClipContent::Midi(midi) = &mut project.tracks[1].clips[0].content else {
        panic!("a MIDI clip");
    };
    midi.next_note_id = 0;
    refused_saving(&project, &format!("{MIDI}.next_note_id"), "a note has id 1");

    let mut project = clips();
    project.tracks[0].clips[0].length = Beats::ZERO;
    refused_saving(&project, "tracks[0].clips[0].length", "must be more than 0");

    let mut project = clips();
    let clip = project.tracks[1].clips.remove(0);
    project.tracks[2].clips.push(clip);
    refused_saving(&project, "tracks[2].clips", "only audio and MIDI tracks");

    let mut project = clips();
    project.next_clip_id = 0;
    refused_saving(&project, "next_clip_id", "a clip has id 1");

    fn settings(project: &mut Project) -> &mut PluginSettings {
        &mut project.tracks[0].devices[0]
            .kind
            .plugin_mut()
            .unwrap()
            .settings
    }
    let mut project = plugged();
    settings(&mut project).params[1].value = f64::NAN;
    refused_saving(
        &project,
        &format!("{PLUGIN}.params[1][1]"),
        "must be a finite number",
    );

    let mut project = plugged();
    settings(&mut project).params.swap(0, 1);
    refused_saving(
        &project,
        &format!("{PLUGIN}.params[1][0]"),
        "in order of id",
    );

    let mut project = plugged();
    let plugin = project.tracks[0].devices[0].kind.plugin_mut().unwrap();
    plugin.id.clear();
    refused_saving(&project, &format!("{PLUGIN}.id"), "must not be empty");
}

/// [`mix`] with its fader, its send and its test gain automated, and the
/// master's pan, in that order.
fn automated() -> Project {
    let mut project = mix();
    let send = project.tracks[0].sends[0].id;
    let gain = project.tracks[0].devices[0].id;
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    project.automation = vec![
        Lane::new(
            ParamAddress::Volume(Chain::Track(TrackId(0))),
            vec![point(0, 1.0), point(4, 0.5), point(4, 0.25)],
        ),
        Lane::new(ParamAddress::Send(send), vec![point(2, 0.5)]),
        Lane::new(ParamAddress::Device(gain, 0), vec![point(0, -1.0)]),
        Lane::new(ParamAddress::Pan(Chain::Master), vec![point(8, 1.0)]),
    ];
    project
}

#[test]
fn lanes_automate_parameters_the_file_has_once_each() {
    let file = edited_from(&automated(), |_| ());
    assert_eq!(load(&file).unwrap(), automated());
    let lane = |f: &mut Value, i: usize, param| f["automation"][i]["param"] = param;
    refused(
        &edited_from(&automated(), |f| {
            lane(f, 1, json!({"volume": {"track": 0}}))
        }),
        "automation[1].param",
        "automation[0] automates it too",
    );
    refused(
        &edited_from(&automated(), |f| lane(f, 0, json!({"pan": {"track": 9}}))),
        "automation[0].param",
        "names track 9, which the file hasn't",
    );
    refused(
        &edited_from(&automated(), |f| lane(f, 1, json!({"send": 4}))),
        "automation[1].param",
        "names send 4, which the file hasn't",
    );
    refused(
        &edited_from(&automated(), |f| {
            lane(f, 2, json!({"device": {"id": 7, "param": 0}}));
        }),
        "automation[2].param",
        "names device 7, which the file hasn't",
    );
    refused(
        &edited_from(&automated(), |f| {
            lane(f, 2, json!({"device": {"id": 0, "param": 1}}));
        }),
        "automation[2].param",
        "names parameter 1 of device 0, which has only parameter 0",
    );
    refused(
        &edited_from(&automated(), |f| lane(f, 3, json!({"pan": "left"}))),
        "automation[3].param.pan",
        "unknown variant `left`",
    );
    refused(
        &edited_from(&automated(), |f| lane(f, 3, json!({"width": "master"}))),
        "automation[3].param",
        "unknown variant `width`",
    );
    refused(
        &edited_from(&automated(), |f| {
            f["automation"][0]
                .as_object_mut()
                .unwrap()
                .remove("overridden");
        }),
        "automation[0]",
        "missing field `overridden`",
    );
    // A plugin's parameters are the plugin's to say, so any will do.
    let mut project = plugged();
    let synth = project.tracks[0].devices[0].id;
    let param = ParamAddress::Device(synth, u32::MAX);
    let point = automation::Point::new(Beats::ZERO, 1e9);
    project.automation = vec![Lane::new(param, vec![point])];
    assert_eq!(load(&edited_from(&project, |_| ())).unwrap(), project);
    // Saving checks the same.
    let mut project = automated();
    project.automation[1].param = ParamAddress::Volume(Chain::Track(TrackId(0)));
    refused_saving(
        &project,
        "automation[1].param",
        "automation[0] automates it too",
    );
}

#[test]
fn lanes_of_built_in_devices_take_the_values_their_parameters_do() {
    let mut project = Project::new();
    let vocal = project.add_track("Vocal", TrackKind::Audio);
    let device = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
    let id = device.id;
    project.track_mut(vocal).unwrap().devices.push(device);
    let point = |at, value| automation::Point::new(Beats::new(at), value);
    let param = |n: usize| ParamAddress::Device(id, n as u32);
    project.automation = vec![
        Lane::new(param(eq::FREQ), vec![point(0, 100.0), point(4, 2500.0)]),
        Lane::new(
            param(eq::SHAPE),
            vec![point(0, 4.0), point(4, 4.0), point(4, 5.0)],
        ),
    ];
    assert_eq!(load(&edited_from(&project, |_| ())).unwrap(), project);
    let lane = |f: &mut Value, i: usize, param| f["automation"][i]["param"] = param;
    refused(
        &edited_from(&project, |f| {
            lane(f, 0, json!({"device": {"id": id.0, "param": 41}}));
        }),
        "automation[0].param",
        "names parameter 41 of device 0, which has parameters 0 to 40",
    );
    let points = |f: &mut Value, i: usize, points| f["automation"][i]["points"] = points;
    refused(
        &edited_from(&project, |f| points(f, 0, json!([[0, 5.0]]))),
        "automation[0].points[0][1]",
        "must be a number from 10 to 22000",
    );
    refused(
        &edited_from(&project, |f| points(f, 1, json!([[0, 4.0], [4, 4.5]]))),
        "automation[1].points[1][1]",
        "must be a whole number from 0 to 9",
    );
    // Saving checks the same.
    project.automation[0].points[1].value = 30_000.0;
    refused_saving(
        &project,
        "automation[0].points[1][1]",
        "must be a number from 10 to 22000",
    );
}

#[test]
fn lane_points_are_in_order_from_the_start_of_the_song() {
    let points = |f: &mut Value, i: usize, points| f["automation"][i]["points"] = points;
    refused(
        &edited_from(&automated(), |f| points(f, 1, json!([]))),
        "automation[1].points",
        "needs at least one point",
    );
    refused(
        &edited_from(&automated(), |f| points(f, 1, json!([[-1, 0.5]]))),
        "automation[1].points[0][0]",
        "can't be before the start of the song",
    );
    refused(
        &edited_from(&automated(), |f| {
            points(f, 1, json!([[MAX_TICKS + 1, 0.5]]));
        }),
        "automation[1].points[0][0]",
        "within",
    );
    refused(
        &edited_from(&automated(), |f| points(f, 1, json!([[8, 0.5], [4, 0.5]]))),
        "automation[1].points[1][0]",
        "must be at or after the point before it",
    );
    refused(
        &edited_from(&automated(), |f| {
            points(f, 0, json!([[0, 1.0], [4, 0.5], [4, 0.25], [4, 0.0]]));
        }),
        "automation[0].points[3][0]",
        "makes three points at one place",
    );
    refused(
        &edited_from(&automated(), |f| points(f, 3, json!([[0, 1.5]]))),
        "automation[3].points[0][1]",
        "must be from -1 to 1",
    );
    refused(
        &edited_from(&automated(), |f| points(f, 1, json!([[0, "loud"]]))),
        "automation[1].points[0][1]",
        "expected f64",
    );
    refused(
        &edited_from(&automated(), |f| points(f, 1, json!([[0, 0.5, 1]]))),
        "automation[1].points[0]",
        "trailing",
    );
    // Faders, sends and test gains can go negative, inverting, as their
    // own fields can.
    let file = edited_from(&automated(), |f| points(f, 1, json!([[0, -2.0]])));
    assert_eq!(load(&file).unwrap().automation[1].points[0].value, -2.0);
    // Saving checks the same.
    let mut project = automated();
    project.automation[3].points[0].value = f64::INFINITY;
    refused_saving(
        &project,
        "automation[3].points[0][1]",
        "must be a finite number",
    );
}

#[test]
fn errors_read_well() {
    assert_eq!(
        LoadError::NewerVersion { version: 99 }.to_string(),
        format!(
            "a newer version of Musicable saved this project, in format version 99, and this \
             version reads up to {VERSION}"
        )
    );
    assert_eq!(
        LoadError::NotAProject.to_string(),
        "the file isn't a Musicable project"
    );
    let invalid = Invalid {
        path: "tracks[2].gain".to_owned(),
        message: "must be a finite number".to_owned(),
    };
    assert_eq!(
        SaveError::Invalid(invalid).to_string(),
        "can't save this project: tracks[2].gain: must be a finite number"
    );
    let syntax = project::from_str("{").unwrap_err().to_string();
    assert!(
        syntax.starts_with("the file isn't valid JSON: ") && syntax.contains("line 1"),
        "{syntax}"
    );
}
