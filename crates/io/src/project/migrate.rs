//! Upgrading files saved in older versions of the format.
//!
//! Each step takes a file up one version, working on the parsed JSON, so old
//! versions need no Rust types of their own. Steps are kept forever: a file
//! from any version goes through every step from its version to the current
//! one, and then reads like a file of the current version.

use serde_json::{Map, Value, json};

use super::{Invalid, VERSION};

/// Upgrades a file from one version to the next. It gets the whole file and
/// leaves its `version` field to [`upgrade`]. It returns an error only for a
/// file that is already broken, naming the field.
type Step = fn(&mut Map<String, Value>) -> Result<(), Invalid>;

/// One step for each version after the first, oldest first: `STEPS[0]` takes
/// version 1 files to version 2. Raising [`VERSION`] without adding its step
/// doesn't compile.
const STEPS: [Step; VERSION as usize - 1] = [
    mixer,
    clips,
    locators,
    plugins,
    recording,
    controllers,
    native,
];

/// Upgrades `document`, a file of version `from`, to [`VERSION`].
pub(super) fn upgrade(document: &mut Value, from: u32) -> Result<(), Invalid> {
    run(&STEPS, document, from)
}

fn run(steps: &[Step], document: &mut Value, from: u32) -> Result<(), Invalid> {
    let file = document
        .as_object_mut()
        .expect("the header was checked, so the file is an object");
    let first = from as usize - 1;
    for (step, to) in steps[first..].iter().zip(from + 1..) {
        step(file)?;
        file.insert("version".to_owned(), to.into());
    }
    Ok(())
}

/// Version 2 added the mixer: pan, solo, solo-safe, arm, routing and sends
/// on each track, device lists, the master channel, and the hard and soft
/// clip types. Version 1 files get what a new project has: tracks centred,
/// feeding the master with no sends or devices, return tracks solo-safe,
/// and the master at unity.
fn mixer(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "next_send_id", json!(0))?;
    add(file, "", "next_device_id", json!(0))?;
    let master = json!({
        "gain": 1.0,
        "pan": 0.0,
        "devices": [],
        "clip_type": "off",
        "console": {},
    });
    add(file, "", "master", master)?;
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        let Some(track) = track.as_object_mut() else {
            // Reading the upgraded file says what's wrong with it.
            continue;
        };
        let path = format!("tracks[{i}]");
        let is_return = track.get("kind").and_then(Value::as_str) == Some("return");
        add(track, &path, "pan", json!(0.0))?;
        add(track, &path, "solo", json!(false))?;
        add(track, &path, "solo_safe", json!(is_return))?;
        add(track, &path, "armed", json!(false))?;
        add(track, &path, "output", Value::Null)?;
        add(track, &path, "sends", json!([]))?;
        add(track, &path, "devices", json!([]))?;
    }
    Ok(())
}

/// Version 3 added clips, audio and MIDI, and the test instrument. Version 2
/// files have no clips.
fn clips(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "next_clip_id", json!(0))?;
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        if let Some(track) = track.as_object_mut() {
            add(track, &format!("tracks[{i}]"), "clips", json!([]))?;
        }
    }
    Ok(())
}

/// Version 4 added locators. Version 3 files have none.
fn locators(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "next_locator_id", json!(0))?;
    add(file, "", "locators", json!([]))
}

/// Version 5 added plugin devices. Version 4 files have none, so they need
/// nothing.
fn plugins(_: &mut Map<String, Value>) -> Result<(), Invalid> {
    Ok(())
}

/// Version 6 added recording: each track's input and monitoring, takes,
/// and punching in and out. Version 5 tracks get what a new track has: an
/// audio track records from the default input's first two channels, a MIDI
/// track from every MIDI input, a bus from nothing, all with automatic
/// monitoring and no takes. Punching in and out is off.
fn recording(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "punch_in", json!(false))?;
    add(file, "", "punch_out", json!(false))?;
    add(file, "", "next_take_id", json!(0))?;
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        let Some(track) = track.as_object_mut() else {
            continue;
        };
        let path = format!("tracks[{i}]");
        let input = match track.get("kind").and_then(Value::as_str) {
            Some("audio") => json!({
                "audio": {"device": "", "name": "", "channel": 0, "stereo": true},
            }),
            Some("midi") => json!({"midi": {"device": "", "name": "", "channel": null}}),
            _ => Value::Null,
        };
        add(track, &path, "input", input)?;
        add(track, &path, "monitor", json!("auto"))?;
        add(track, &path, "takes", json!([]))?;
    }
    Ok(())
}

/// Version 7 added controllers to MIDI clips, such as the sustain pedal.
/// Version 6 clips move none, on tracks and in takes.
fn controllers(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        let path = format!("tracks[{i}]");
        no_controllers(track.get_mut("clips"), &format!("{path}.clips"))?;
        let takes = track.get_mut("takes").and_then(Value::as_array_mut);
        for (j, take) in takes.into_iter().flatten().enumerate() {
            no_controllers(take.get_mut("clips"), &format!("{path}.takes[{j}].clips"))?;
        }
    }
    Ok(())
}

/// Gives each MIDI clip in `clips`, a list at `path`, no controllers.
fn no_controllers(clips: Option<&mut Value>, path: &str) -> Result<(), Invalid> {
    let clips = clips.and_then(Value::as_array_mut);
    for (k, clip) in clips.into_iter().flatten().enumerate() {
        let midi = clip
            .get_mut("content")
            .and_then(|c| c.get_mut("midi"))
            .and_then(Value::as_object_mut);
        if let Some(midi) = midi {
            add(
                midi,
                &format!("{path}[{k}].content.midi"),
                "controllers",
                json!([]),
            )?;
        }
    }
    Ok(())
}

/// Version 8 added Musicable's own devices. Version 7 files have none, so
/// they need nothing.
fn native(_: &mut Map<String, Value>) -> Result<(), Invalid> {
    Ok(())
}

/// Adds a field the file's version didn't have to `object`, at `path`.
/// Refuses one that's there already, rather than lose its value.
fn add(
    object: &mut Map<String, Value>,
    path: &str,
    key: &str,
    value: Value,
) -> Result<(), Invalid> {
    if object.contains_key(key) {
        let path = if path.is_empty() {
            key.to_owned()
        } else {
            format!("{path}.{key}")
        };
        return Err(Invalid::new(path, "unknown field for the file's version"));
    }
    object.insert(key.to_owned(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A made-up version 2 that renamed `metronome` to `click`.
    fn rename_metronome(file: &mut Map<String, Value>) -> Result<(), Invalid> {
        let on = file
            .remove("metronome")
            .ok_or_else(|| Invalid::new("metronome", "is missing"))?;
        file.insert("click".to_owned(), on);
        Ok(())
    }

    /// And a made-up version 3 that added a swing amount to every track.
    fn add_swing(file: &mut Map<String, Value>) -> Result<(), Invalid> {
        let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
        for track in tracks.into_iter().flatten() {
            if let Some(track) = track.as_object_mut() {
                track.insert("swing".to_owned(), 0.into());
            }
        }
        Ok(())
    }

    const MADE_UP: [Step; 2] = [rename_metronome, add_swing];

    #[test]
    fn files_go_through_every_step_after_their_version() {
        let mut v1 = json!({"version": 1, "metronome": true, "tracks": [{"id": 0}]});
        run(&MADE_UP, &mut v1, 1).unwrap();
        assert_eq!(
            v1,
            json!({"version": 3, "click": true, "tracks": [{"id": 0, "swing": 0}]})
        );

        let mut v2 = json!({"version": 2, "click": false, "tracks": [{"id": 4}]});
        run(&MADE_UP, &mut v2, 2).unwrap();
        assert_eq!(
            v2,
            json!({"version": 3, "click": false, "tracks": [{"id": 4, "swing": 0}]})
        );

        let v3 = json!({"version": 3, "click": false, "tracks": []});
        let mut unchanged = v3.clone();
        run(&MADE_UP, &mut unchanged, 3).unwrap();
        assert_eq!(unchanged, v3);
    }

    #[test]
    fn a_step_can_refuse_a_broken_file() {
        let mut broken = json!({"version": 1, "tracks": []});
        assert_eq!(
            run(&MADE_UP, &mut broken, 1),
            Err(Invalid::new("metronome", "is missing"))
        );
    }

    #[test]
    fn version_1_files_get_a_new_projects_mixer() {
        let mut file = json!({
            "version": 1,
            "tracks": [{"kind": "audio"}, {"kind": "return"}, "not a track"],
        });
        run(&STEPS[..1], &mut file, 1).unwrap();
        let track = |solo_safe| {
            json!({
                "pan": 0.0,
                "solo": false,
                "solo_safe": solo_safe,
                "armed": false,
                "output": null,
                "sends": [],
                "devices": [],
            })
        };
        let mut audio = track(false);
        audio["kind"] = "audio".into();
        let mut reverb = track(true);
        reverb["kind"] = "return".into();
        assert_eq!(file["tracks"], json!([audio, reverb, "not a track"]));
        assert_eq!(file["master"]["gain"], json!(1.0));
        assert_eq!(file["next_send_id"], json!(0));
        assert_eq!(file["version"], json!(2));
    }

    #[test]
    fn version_2_files_have_no_clips() {
        let mut file = json!({"version": 2, "tracks": [{"kind": "midi"}, "not a track"]});
        run(&STEPS[..2], &mut file, 2).unwrap();
        assert_eq!(
            file["tracks"],
            json!([{"kind": "midi", "clips": []}, "not a track"])
        );
        assert_eq!(file["next_clip_id"], json!(0));
        assert_eq!(file["version"], json!(3));
        let mut file = json!({"version": 2, "tracks": [{"clips": [{}]}]});
        assert_eq!(
            run(&STEPS[..2], &mut file, 2).unwrap_err().path,
            "tracks[0].clips",
            "clips in a version 2 file"
        );
    }

    #[test]
    fn version_3_files_have_no_locators() {
        let mut file = json!({"version": 3, "tracks": []});
        run(&STEPS[..3], &mut file, 3).unwrap();
        assert_eq!(
            file,
            json!({"version": 4, "tracks": [], "locators": [], "next_locator_id": 0})
        );
        let mut file = json!({"version": 3, "locators": [{}]});
        assert_eq!(
            run(&STEPS[..3], &mut file, 3).unwrap_err().path,
            "locators",
            "locators in a version 3 file"
        );
    }

    #[test]
    fn version_5_tracks_record_from_the_default_inputs() {
        let mut file = json!({
            "version": 5,
            "tracks": [{"kind": "audio"}, {"kind": "midi"}, {"kind": "group"}, "not a track"],
        });
        run(&STEPS[..5], &mut file, 5).unwrap();
        let track = |kind: &str, input| json!({"kind": kind, "input": input, "monitor": "auto", "takes": []});
        let audio = json!({"audio": {"device": "", "name": "", "channel": 0, "stereo": true}});
        let midi = json!({"midi": {"device": "", "name": "", "channel": null}});
        assert_eq!(
            file["tracks"],
            json!([
                track("audio", audio),
                track("midi", midi),
                track("group", Value::Null),
                "not a track",
            ])
        );
        assert_eq!(
            (&file["punch_in"], &file["punch_out"], &file["next_take_id"]),
            (&json!(false), &json!(false), &json!(0))
        );
        assert_eq!(file["version"], json!(6));
        let mut file = json!({"version": 5, "tracks": [{"kind": "audio", "takes": []}]});
        assert_eq!(
            run(&STEPS[..5], &mut file, 5).unwrap_err().path,
            "tracks[0].takes",
            "takes in a version 5 file"
        );
    }

    #[test]
    fn version_6_midi_clips_move_no_controllers() {
        let midi = || json!({"content": {"midi": {"notes": []}}});
        let audio = json!({"content": {"audio": {"file": "Take.wav"}}});
        let mut file = json!({
            "version": 6,
            "tracks": [
                {"clips": [midi(), audio.clone()], "takes": [{"clips": [midi()]}]},
                {"clips": [audio]},
                "not a track",
            ],
        });
        run(&STEPS[..6], &mut file, 6).unwrap();
        let moving = json!({"content": {"midi": {"notes": [], "controllers": []}}});
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], moving);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], moving);
        assert_eq!(
            tracks[0]["clips"][1]["content"]["audio"],
            json!({"file": "Take.wav"})
        );
        assert_eq!(
            tracks[1]["clips"][0]["content"]["audio"],
            json!({"file": "Take.wav"})
        );
        assert_eq!(file["version"], json!(7));
        let mut file = json!({
            "version": 6,
            "tracks": [{"takes": [{"clips": [{"content": {"midi": {"controllers": []}}}]}]}],
        });
        assert_eq!(
            run(&STEPS[..6], &mut file, 6).unwrap_err().path,
            "tracks[0].takes[0].clips[0].content.midi.controllers",
            "controllers in a version 6 file"
        );
    }

    #[test]
    fn version_1_files_cant_have_the_new_fields() {
        let mut file = json!({"version": 1, "tracks": [{"kind": "midi", "solo": true}]});
        assert_eq!(
            upgrade(&mut file, 1),
            Err(Invalid::new(
                "tracks[0].solo",
                "unknown field for the file's version"
            ))
        );
        let mut file = json!({"version": 1, "master": {}, "tracks": []});
        assert_eq!(
            upgrade(&mut file, 1).unwrap_err().path,
            "master",
            "the master"
        );
    }

    #[test]
    fn current_files_need_no_steps() {
        let current = json!({"version": VERSION, "tracks": []});
        let mut upgraded = current.clone();
        upgrade(&mut upgraded, VERSION).unwrap();
        assert_eq!(upgraded, current);
    }
}
