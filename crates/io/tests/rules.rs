//! The format's rules: what loading refuses and where it says the problem
//! is, what it accepts, and what saving refuses.

use musicable_io::project::{self, Invalid, LoadError, MAX_TICKS, SaveError, VERSION};
use musicable_model::clip::{AudioClip, ClipContent, ClipId, MidiClip, NoteId, Point};
use musicable_model::mixer::Bus;
use musicable_model::time::{BeatRange, Beats, TimeSignature};
use musicable_model::{DeviceKind, LocatorId, Project, TestTone, TrackId, TrackKind};
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
/// two notes, the second with expression, played by a test instrument, and
/// a group track. The tracks' ids are their places in the list, and the
/// clips' ids are their tracks'.
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
    let range = BeatRange::new(Beats::ZERO, Beats::new(4));
    let clip = project.new_clip("Notes", range, ClipContent::Midi(notes));
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
    let track = project.track_mut(midi).unwrap();
    track.clips.push(clip);
    track.devices.push(instrument);
    project
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
    for field in ["tone", "output"] {
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
            f["tracks"][0]["devices"][0]["kind"] = json!({"plugin": {}});
        }),
        "tracks[0].devices[0].kind",
        "unknown variant `plugin`",
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
