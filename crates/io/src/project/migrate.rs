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
    automation,
    workspace,
    warping,
    transients,
    slicing,
    spectral,
    high_quality,
    granular,
    bypass,
    modulation,
    wires,
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

/// Version 9 added automation. Version 8 files have none.
fn automation(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "automation", json!([]))
}

/// Version 10 added the workspace a project was saved with. Version 9 files
/// name none.
fn workspace(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "workspace", Value::Null)
}

/// Version 11 added warping, and curves for an audio clip's own parameters,
/// such as its gain. Version 10 audio clips play at their files' own speed
/// and have no curves, on tracks and in takes.
fn warping(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        let path = format!("tracks[{i}]");
        unwarped(track.get_mut("clips"), &format!("{path}.clips"))?;
        let takes = track.get_mut("takes").and_then(Value::as_array_mut);
        for (j, take) in takes.into_iter().flatten().enumerate() {
            unwarped(take.get_mut("clips"), &format!("{path}.takes[{j}].clips"))?;
        }
    }
    Ok(())
}

/// Leaves each audio clip in `clips`, a list at `path`, unwarped and
/// without curves.
fn unwarped(clips: Option<&mut Value>, path: &str) -> Result<(), Invalid> {
    let clips = clips.and_then(Value::as_array_mut);
    for (k, clip) in clips.into_iter().flatten().enumerate() {
        let audio = clip
            .get_mut("content")
            .and_then(|c| c.get_mut("audio"))
            .and_then(Value::as_object_mut);
        if let Some(audio) = audio {
            let path = format!("{path}[{k}].content.audio");
            add(audio, &path, "warp", Value::Null)?;
            add(audio, &path, "automation", json!([]))?;
        }
    }
    Ok(())
}

/// Version 12 added the transients a warped clip keeps. Version 11 warped
/// clips have none yet, so they take them from analysis.
fn transients(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    each_warp(file, &mut |warp, path| {
        add(warp, path, "onsets", Value::Null)
    })
}

/// Version 13 added the Slice mode, with its settings, and the transpose and
/// detune of every mode but Re-Pitch. Version 12 warped clips play Re-Pitch,
/// so they're untransposed, and Slice's settings are Live's.
fn slicing(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    each_warp(file, &mut |warp, path| {
        add(warp, path, "transpose", json!(0))?;
        add(warp, path, "detune", json!(0))?;
        let slice =
            json!({"preserve": "transients", "transient_loop": "forward", "envelope": 100.0});
        add(warp, path, "slice", slice)
    })
}

/// Version 14 added the Spectral mode, with its Formants. Version 13 warped
/// clips keep their formants in place, should they turn Spectral.
fn spectral(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    each_warp(file, &mut |warp, path| {
        add(warp, path, "spectral", json!({"formants": 100.0}))
    })
}

/// Version 15 added Spectral's High quality, with its Envelope. Version 14
/// warped clips keep rendering with Signalsmith Stretch, with the envelope
/// as Live's Complex Pro has it, should High quality turn on.
fn high_quality(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    each_warp(file, &mut |warp, path| {
        // Settings that aren't there, or aren't an object, are the
        // schema's to report.
        let Some(spectral) = warp.get_mut("spectral").and_then(Value::as_object_mut) else {
            return Ok(());
        };
        let path = format!("{path}.spectral");
        add(spectral, &path, "high_quality", json!(false))?;
        add(spectral, &path, "envelope", json!(128.0))
    })
}

/// Version 16 added the Granular mode, with its Follow pitch, Grain Size
/// and Fluctuation. Version 15 warped clips follow the audio's pitch with
/// 30 ms grains, should they turn Granular.
fn granular(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    each_warp(file, &mut |warp, path| {
        let granular = json!({"follow_pitch": true, "grain_size": 30.0, "fluctuation": 25.0});
        add(warp, path, "granular", granular)
    })
}

/// Version 17 added Bypass and Deactivate. Version 16 devices all play, on
/// tracks and on the master.
fn bypass(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        playing(track.get_mut("devices"), &format!("tracks[{i}].devices"))?;
    }
    let master = file.get_mut("master").and_then(|m| m.get_mut("devices"));
    playing(master, "master.devices")
}

/// Leaves each device in `devices`, a list at `path`, active and not
/// bypassed.
fn playing(devices: Option<&mut Value>, path: &str) -> Result<(), Invalid> {
    let devices = devices.and_then(Value::as_array_mut);
    for (j, device) in devices.into_iter().flatten().enumerate() {
        if let Some(device) = device.as_object_mut() {
            let path = format!("{path}[{j}]");
            add(device, &path, "bypassed", json!(false))?;
            add(device, &path, "active", json!(true))?;
        }
    }
    Ok(())
}

/// Version 18 added modulators, with their routings, on tracks and on the
/// master. Version 17 channels have none.
fn modulation(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "next_modulator_id", json!(0))?;
    add(file, "", "next_routing_id", json!(0))?;
    let none = || json!({"modulators": [], "routings": []});
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        if let Some(track) = track.as_object_mut() {
            add(track, &format!("tracks[{i}]"), "modulation", none())?;
        }
    }
    if let Some(master) = file.get_mut("master").and_then(Value::as_object_mut) {
        add(master, "master", "modulation", none())?;
    }
    Ok(())
}

/// Version 19 added wires between channels, such as the keys of
/// compressors' sidechains, and lanes for their gains. Version 18 files have
/// none.
fn wires(file: &mut Map<String, Value>) -> Result<(), Invalid> {
    add(file, "", "next_wire_id", json!(0))?;
    add(file, "", "wires", json!([]))
}

/// A change to a warp in a file, given the warp and its path.
type WarpChange<'a> = dyn FnMut(&mut Map<String, Value>, &str) -> Result<(), Invalid> + 'a;

/// Calls `change` with the warp of each warped audio clip, on tracks and in
/// takes, and its path.
fn each_warp(file: &mut Map<String, Value>, change: &mut WarpChange<'_>) -> Result<(), Invalid> {
    let tracks = file.get_mut("tracks").and_then(Value::as_array_mut);
    for (i, track) in tracks.into_iter().flatten().enumerate() {
        let path = format!("tracks[{i}]");
        warps_in(track.get_mut("clips"), &format!("{path}.clips"), change)?;
        let takes = track.get_mut("takes").and_then(Value::as_array_mut);
        for (j, take) in takes.into_iter().flatten().enumerate() {
            let path = format!("{path}.takes[{j}].clips");
            warps_in(take.get_mut("clips"), &path, change)?;
        }
    }
    Ok(())
}

/// Calls `change` with the warp of each warped audio clip in `clips`, a
/// list at `path`.
fn warps_in(
    clips: Option<&mut Value>,
    path: &str,
    change: &mut WarpChange<'_>,
) -> Result<(), Invalid> {
    let clips = clips.and_then(Value::as_array_mut);
    for (k, clip) in clips.into_iter().flatten().enumerate() {
        let warp = clip
            .get_mut("content")
            .and_then(|c| c.get_mut("audio"))
            .and_then(|a| a.get_mut("warp"))
            .and_then(Value::as_object_mut);
        if let Some(warp) = warp {
            change(warp, &format!("{path}[{k}].content.audio.warp"))?;
        }
    }
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
    fn version_8_files_have_no_automation() {
        let mut file = json!({"version": 8, "tracks": []});
        run(&STEPS[..8], &mut file, 8).unwrap();
        assert_eq!(file, json!({"version": 9, "tracks": [], "automation": []}));
        let mut file = json!({"version": 8, "automation": [{}]});
        assert_eq!(
            run(&STEPS[..8], &mut file, 8).unwrap_err().path,
            "automation",
            "automation in a version 8 file"
        );
    }

    #[test]
    fn version_9_files_name_no_workspace() {
        let mut file = json!({"version": 9, "tracks": []});
        run(&STEPS[..9], &mut file, 9).unwrap();
        assert_eq!(
            file,
            json!({"version": 10, "tracks": [], "workspace": null})
        );
        let mut file = json!({"version": 9, "workspace": "Mixing"});
        assert_eq!(
            run(&STEPS[..9], &mut file, 9).unwrap_err().path,
            "workspace",
            "a workspace in a version 9 file"
        );
    }

    #[test]
    fn version_10_audio_clips_are_unwarped() {
        let audio = || json!({"content": {"audio": {"file": "Loop.wav"}}});
        let midi = json!({"content": {"midi": {"notes": []}}});
        let mut file = json!({
            "version": 10,
            "tracks": [
                {"clips": [audio(), midi.clone()], "takes": [{"clips": [audio()]}]},
                {"clips": [midi]},
                "not a track",
            ],
        });
        run(&STEPS[..10], &mut file, 10).unwrap();
        let unwarped = json!({
            "content": {"audio": {"file": "Loop.wav", "warp": null, "automation": []}},
        });
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], unwarped);
        assert_eq!(
            tracks[1]["clips"][0],
            json!({"content": {"midi": {"notes": []}}})
        );
        assert_eq!(file["version"], json!(11));
        let mut file = json!({
            "version": 10,
            "tracks": [{"clips": [{"content": {"audio": {"warp": null}}}]}],
        });
        assert_eq!(
            run(&STEPS[..10], &mut file, 10).unwrap_err().path,
            "tracks[0].clips[0].content.audio.warp",
            "warping in a version 10 file"
        );
    }

    #[test]
    fn version_11_warped_clips_have_no_transients_yet() {
        let warped = || json!({"content": {"audio": {"warp": {"tempo": 120.0}}}});
        let unwarped = json!({"content": {"audio": {"warp": null}}});
        let mut file = json!({
            "version": 11,
            "tracks": [
                {"clips": [warped(), unwarped.clone()], "takes": [{"clips": [warped()]}]},
                "not a track",
            ],
        });
        run(&STEPS[..11], &mut file, 11).unwrap();
        let taken = json!({
            "content": {"audio": {"warp": {"tempo": 120.0, "onsets": null}}},
        });
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], taken);
        assert_eq!(tracks[0]["clips"][1], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], taken);
        assert_eq!(file["version"], json!(12));
        let mut file = json!({
            "version": 11,
            "tracks": [{"clips": [{"content": {"audio": {"warp": {"onsets": []}}}}]}],
        });
        assert_eq!(
            run(&STEPS[..11], &mut file, 11).unwrap_err().path,
            "tracks[0].clips[0].content.audio.warp.onsets",
            "transients in a version 11 file"
        );
    }

    #[test]
    fn version_12_warped_clips_play_re_pitch_untransposed() {
        let warped = || json!({"content": {"audio": {"warp": {"mode": "re_pitch"}}}});
        let unwarped = json!({"content": {"audio": {"warp": null}}});
        let mut file = json!({
            "version": 12,
            "tracks": [
                {"clips": [warped(), unwarped.clone()], "takes": [{"clips": [warped()]}]},
            ],
        });
        run(&STEPS[..12], &mut file, 12).unwrap();
        let sliced = json!({
            "content": {"audio": {"warp": {
                "mode": "re_pitch",
                "transpose": 0,
                "detune": 0,
                "slice": {"preserve": "transients", "transient_loop": "forward", "envelope": 100.0},
            }}},
        });
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], sliced);
        assert_eq!(tracks[0]["clips"][1], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], sliced);
        assert_eq!(file["version"], json!(13));
        let mut file = json!({
            "version": 12,
            "tracks": [{"takes": [{"clips": [{"content": {"audio": {"warp": {"detune": 3}}}}]}]}],
        });
        assert_eq!(
            run(&STEPS[..12], &mut file, 12).unwrap_err().path,
            "tracks[0].takes[0].clips[0].content.audio.warp.detune",
            "a detune in a version 12 file"
        );
    }

    #[test]
    fn version_13_warped_clips_keep_their_formants() {
        let warped = || json!({"content": {"audio": {"warp": {"mode": "slice"}}}});
        let unwarped = json!({"content": {"audio": {"warp": null}}});
        let mut file = json!({
            "version": 13,
            "tracks": [
                {"clips": [warped(), unwarped.clone()], "takes": [{"clips": [warped()]}]},
            ],
        });
        run(&STEPS[..13], &mut file, 13).unwrap();
        let kept = json!({
            "content": {"audio": {"warp": {"mode": "slice", "spectral": {"formants": 100.0}}}},
        });
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], kept);
        assert_eq!(tracks[0]["clips"][1], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], kept);
        assert_eq!(file["version"], json!(14));
        let mut file = json!({
            "version": 13,
            "tracks": [{"clips": [{"content": {"audio": {"warp": {"spectral": {}}}}}]}],
        });
        assert_eq!(
            run(&STEPS[..13], &mut file, 13).unwrap_err().path,
            "tracks[0].clips[0].content.audio.warp.spectral",
            "Spectral's settings in a version 13 file"
        );
    }

    #[test]
    fn version_14_warped_clips_render_with_signalsmith_stretch() {
        let warped = || json!({"content": {"audio": {"warp": {"spectral": {"formants": 40.0}}}}});
        let unwarped = json!({"content": {"audio": {"warp": null}}});
        let mut file = json!({
            "version": 14,
            "tracks": [
                {"clips": [warped(), unwarped.clone()], "takes": [{"clips": [warped()]}]},
            ],
        });
        run(&STEPS[..14], &mut file, 14).unwrap();
        let spectral = json!({"high_quality": false, "formants": 40.0, "envelope": 128.0});
        let kept = json!({"content": {"audio": {"warp": {"spectral": spectral}}}});
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], kept);
        assert_eq!(tracks[0]["clips"][1], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], kept);
        assert_eq!(file["version"], json!(15));
        let mut file = json!({
            "version": 14,
            "tracks": [{"clips": [{"content": {"audio": {"warp": {"spectral": {"envelope": 64.0}}}}}]}],
        });
        assert_eq!(
            run(&STEPS[..14], &mut file, 14).unwrap_err().path,
            "tracks[0].clips[0].content.audio.warp.spectral.envelope",
            "Spectral's Envelope in a version 14 file"
        );
    }

    #[test]
    fn version_15_warped_clips_follow_the_pitch_should_they_turn_granular() {
        let warped = || json!({"content": {"audio": {"warp": {"mode": "spectral"}}}});
        let unwarped = json!({"content": {"audio": {"warp": null}}});
        let mut file = json!({
            "version": 15,
            "tracks": [
                {"clips": [warped(), unwarped.clone()], "takes": [{"clips": [warped()]}]},
            ],
        });
        run(&STEPS[..15], &mut file, 15).unwrap();
        let granular = json!({"follow_pitch": true, "grain_size": 30.0, "fluctuation": 25.0});
        let kept =
            json!({"content": {"audio": {"warp": {"mode": "spectral", "granular": granular}}}});
        let tracks = &file["tracks"];
        assert_eq!(tracks[0]["clips"][0], kept);
        assert_eq!(tracks[0]["clips"][1], unwarped);
        assert_eq!(tracks[0]["takes"][0]["clips"][0], kept);
        assert_eq!(file["version"], json!(16));
        let mut file = json!({
            "version": 15,
            "tracks": [{"clips": [{"content": {"audio": {"warp": {"granular": {}}}}}]}],
        });
        assert_eq!(
            run(&STEPS[..15], &mut file, 15).unwrap_err().path,
            "tracks[0].clips[0].content.audio.warp.granular",
            "Granular's settings in a version 15 file"
        );
    }

    #[test]
    fn version_17_channels_have_no_modulators() {
        let mut file = json!({
            "version": 17,
            "tracks": [{"devices": []}, {"devices": []}],
            "master": {"devices": []},
        });
        run(&STEPS[..17], &mut file, 17).unwrap();
        let none = json!({"modulators": [], "routings": []});
        assert_eq!(file["tracks"][0]["modulation"], none);
        assert_eq!(file["tracks"][1]["modulation"], none);
        assert_eq!(file["master"]["modulation"], none);
        assert_eq!(file["next_modulator_id"], json!(0));
        assert_eq!(file["next_routing_id"], json!(0));
        assert_eq!(file["version"], json!(18));
        let mut file = json!({"version": 17, "tracks": [{"modulation": {}}]});
        assert_eq!(
            run(&STEPS[..17], &mut file, 17).unwrap_err().path,
            "tracks[0].modulation",
            "a track's modulation in a version 17 file"
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
