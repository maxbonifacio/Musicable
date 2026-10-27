//! What the read tools give back, as JSON: the commands, the project, a
//! clip, a device, a lane of automation, the editor's state and the undo
//! history. Positions and lengths are numbers of beats, gains are linear,
//! and ids are whole numbers, as commands take them.

use musicable_commands::editor::Lane as PianoRollLane;
use musicable_commands::{
    Command, CommandId, FromValue, Keymap, Param, Registry, Scope, Value, device, json, param_text,
};
use musicable_model::automation::Lane;
use musicable_model::clip::{Clip, ClipContent, ClipId, Controller, Note, Offset, Point};
use musicable_model::mixer::Bus;
use musicable_model::native::{NativeDevice, Unit};
use musicable_model::param::ParamAddress;
use musicable_model::time::Beats;
use musicable_model::{
    Chain, ClipType, Device, DeviceId, DeviceKind, Master, Monitor, PluginFormat, Track, TrackKind,
};
use serde_json::{Map, Value as Json, json};

use crate::Window;

/// Every command agents can run, as `musicable api` prints them: each one's
/// id, title, arguments as JSON Schema and keys in `keymap`, and whether it
/// runs alone, never in a batch. For agents, and for generating docs,
/// without a running Musicable.
pub fn catalogue(commands: &Registry, keymap: &Keymap) -> Json {
    let commands: Vec<Json> = (commands.iter())
        .filter(|command| command.agents)
        .map(|command| {
            let mut entry = Map::new();
            entry.insert("id".into(), command.id.as_str().into());
            entry.insert("title".into(), command.title.as_str().into());
            entry.insert("input_schema".into(), json::schema(&command.params).into());
            add_keys(&mut entry, keymap, &command.id);
            if command.alone {
                entry.insert("alone".into(), true.into());
            }
            Json::Object(entry)
        })
        .collect();
    json!({
        "musicable": env!("CARGO_PKG_VERSION"),
        "commands": commands,
    })
}

/// The commands agents can run, or those whose ids or titles hold
/// `filter`, whatever its case.
pub(crate) fn commands(window: &impl Window, filter: Option<&str>) -> Json {
    let filter = filter.map(str::to_lowercase);
    let wanted = |command: &Command| {
        filter.as_deref().is_none_or(|filter| {
            command.id.as_str().contains(filter) || command.title.to_lowercase().contains(filter)
        })
    };
    let commands: Vec<Json> = (window.commands().iter())
        .filter(|command| command.agents && wanted(command))
        .map(|command| {
            let mut entry = Map::new();
            entry.insert("id".into(), command.id.as_str().into());
            entry.insert("title".into(), command.title.as_str().into());
            if !command.params.is_empty() {
                let args = command.params.iter().map(arg).collect();
                entry.insert("args".into(), Json::Array(args));
            }
            add_applies(&mut entry, window, command);
            add_keys(&mut entry, window.keymap(), &command.id);
            Json::Object(entry)
        })
        .collect();
    json!({ "commands": commands })
}

/// A parameter as `list_commands` lists it: its name and type, and whether
/// it can be left out.
fn arg(param: &Param) -> Json {
    let mut arg = json!({
        "name": param.name.as_ref(),
        "type": json::type_text(param.ty),
    });
    if param.optional && !param.asked {
        arg["optional"] = true.into();
    }
    arg
}

/// One command, with its arguments as JSON Schema.
pub(crate) fn command(window: &impl Window, id: &CommandId) -> Result<Json, String> {
    let command = (window.commands().get(id))
        .filter(|command| command.agents)
        .ok_or_else(|| no_command(window.commands(), id))?;
    let mut entry = Map::new();
    entry.insert("id".into(), command.id.as_str().into());
    entry.insert("title".into(), command.title.as_str().into());
    entry.insert("input_schema".into(), json::schema(&command.params).into());
    add_applies(&mut entry, window, command);
    add_keys(&mut entry, window.keymap(), &command.id);
    if command.alone {
        entry.insert("alone".into(), true.into());
    }
    Ok(Json::Object(entry))
}

/// Why there's no command `id` for agents to run.
pub(crate) fn no_command(commands: &Registry, id: &CommandId) -> String {
    match commands.get(id) {
        Some(_) => format!("Agents can't run {id}: it's left to the user."),
        None => format!("There's no command called {id}: list_commands lists them."),
    }
}

/// Whether `command` applies now without arguments, unless it needs some.
fn add_applies(entry: &mut Map<String, Json>, window: &impl Window, command: &Command) {
    if !command.needs_arguments() {
        let applies = (command.when).applies(window.project(), window.editor());
        entry.insert("applies".into(), applies.into());
    }
}

/// The keys that run command `id`, if any do: "Ctrl+D", or "Delete in
/// piano_roll" for a panel's own.
fn add_keys(entry: &mut Map<String, Json>, keymap: &Keymap, id: &CommandId) {
    let keys: Vec<Json> = (keymap.bindings_for(id))
        .map(|(scope, chord)| match scope {
            Scope::Everywhere => chord.to_string().into(),
            Scope::Panel(panel) => format!("{chord} in {panel}").into(),
        })
        .collect();
    if !keys.is_empty() {
        entry.insert("keys".into(), Json::Array(keys));
    }
}

/// The project, with everything's id: the tempo map, the loop, the tracks
/// with their mixer settings, devices and clips, the master, the locators,
/// and which parameters are automated. Notes, devices' parameters and
/// automation points come from their own reads.
pub(crate) fn project(window: &impl Window) -> Json {
    let project = window.project();
    let tempo = &project.tempo;
    let tempos: Vec<Json> = (tempo.tempos().iter())
        .map(|change| json!({ "at": beats(change.at), "bpm": change.bpm }))
        .collect();
    let signatures: Vec<Json> = (tempo.signatures().iter())
        .map(|change| {
            let signature = change.signature;
            json!({
                "at": beats(change.at()),
                "bar": change.bar,
                "signature": format!("{}/{}", signature.numerator(), signature.denominator()),
            })
        })
        .collect();
    let mut locators: Vec<_> = project.locators.iter().collect();
    locators.sort_by_key(|locator| (locator.at, locator.id));
    let locators: Vec<Json> = (locators.into_iter())
        .map(|locator| json!({ "id": locator.id.0, "name": locator.name, "at": beats(locator.at) }))
        .collect();
    let automation: Vec<Json> = (project.automation.iter())
        .map(|lane| {
            json!({
                "param": param_text(lane.param),
                "points": lane.points.len(),
                "overridden": lane.overridden,
            })
        })
        .collect();
    let end = (project.tracks.iter())
        .flat_map(|track| &track.clips)
        .map(Clip::end)
        .max()
        .unwrap_or(Beats::ZERO);
    json!({
        "file": window.file().map(|file| file.display().to_string()),
        "modified": window.is_modified(),
        "end": beats(end),
        "tempo": tempos,
        "time_signatures": signatures,
        "loop": {
            "start": beats(project.loop_region.start),
            "end": beats(project.loop_region.end),
            "on": project.looping,
        },
        "metronome": project.metronome,
        "punch_in": project.punch_in,
        "punch_out": project.punch_out,
        "tracks": project.tracks.iter().map(track).collect::<Vec<_>>(),
        "master": master(&project.master),
        "locators": locators,
        "automation": automation,
    })
}

fn track(track: &Track) -> Json {
    let kind = match track.kind {
        TrackKind::Audio => "audio",
        TrackKind::Midi => "midi",
        TrackKind::Return => "return",
        TrackKind::Group => "group",
    };
    let output = match track.output {
        Bus::Master => json!("master"),
        Bus::Group(group) => json!({ "group": group.0 }),
    };
    let monitor = match track.monitor {
        Monitor::In => "in",
        Monitor::Auto => "auto",
        Monitor::Off => "off",
    };
    let sends: Vec<Json> = (track.sends.iter())
        .map(|send| {
            json!({
                "id": send.id.0,
                "to": send.to.0,
                "level": short(send.level),
                "pre_fader": send.pre_fader,
            })
        })
        .collect();
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by_key(|clip| (clip.start, clip.id));
    let mut entry = json!({
        "id": track.id.0,
        "name": track.name,
        "kind": kind,
        "gain": short(track.gain),
        "pan": short(track.pan),
        "muted": track.muted,
        "solo": track.solo,
        "solo_safe": track.solo_safe,
        "armed": track.armed,
        "monitor": monitor,
        "output": output,
        "sends": sends,
        "clip_type": clip_type(track.clip_type),
        "devices": track.devices.iter().map(device_entry).collect::<Vec<_>>(),
        "clips": clips.into_iter().map(clip_entry).collect::<Vec<_>>(),
    });
    if !track.takes.is_empty() {
        let takes: Vec<Json> = (track.takes.iter())
            .map(|take| {
                let mut clips: Vec<&Clip> = take.clips.iter().collect();
                clips.sort_by_key(|clip| (clip.start, clip.id));
                json!({
                    "id": take.id.0,
                    "name": take.name,
                    "clips": clips.into_iter().map(clip_entry).collect::<Vec<_>>(),
                })
            })
            .collect();
        entry["takes"] = takes.into();
    }
    entry
}

fn master(master: &Master) -> Json {
    json!({
        "gain": short(master.gain),
        "pan": short(master.pan),
        "clip_type": clip_type(master.clip_type),
        "devices": master.devices.iter().map(device_entry).collect::<Vec<_>>(),
    })
}

fn clip_type(clip_type: ClipType) -> &'static str {
    musicable_commands::mixer::clip_type_name(clip_type)
}

/// A device as the project lists it: its id, name and kind, and whether
/// it's bypassed or deactivated, if it is.
fn device_entry(device: &Device) -> Json {
    let mut entry = json!({
        "id": device.id.0,
        "name": device::device_name(&device.kind),
        "kind": device_kind(&device.kind),
    });
    if device.kind.is_instrument() {
        entry["instrument"] = true.into();
    }
    if device.bypassed {
        entry["bypassed"] = true.into();
    }
    if !device.active {
        entry["active"] = false.into();
    }
    entry
}

/// What a device is: a built-in device's key, such as "eq", or "plugin".
fn device_kind(kind: &DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Native(native) => native.kind.key(),
        DeviceKind::Plugin(_) => "plugin",
        DeviceKind::TestGain { .. } => "test_gain",
        DeviceKind::TestInstrument { .. } => "test_instrument",
    }
}

/// A clip as the project lists it: where it is, and what it plays, without
/// its notes.
fn clip_entry(clip: &Clip) -> Json {
    let mut entry = json!({
        "id": clip.id.0,
        "name": clip.name,
        "start": beats(clip.start),
        "length": beats(clip.length),
    });
    match &clip.content {
        ClipContent::Midi(midi) => {
            entry["kind"] = "midi".into();
            entry["notes"] = midi.notes.len().into();
        }
        ClipContent::Audio(audio) => {
            entry["kind"] = "audio".into();
            entry["file"] = audio.file.as_str().into();
        }
    }
    entry
}

/// One clip: where it is, and its notes with their expression and its
/// controllers' curves, in the clip's own time, or its audio file.
pub(crate) fn clip(window: &impl Window, id: ClipId) -> Result<Json, String> {
    let project = window.project();
    let found = project.tracks.iter().find_map(|track| {
        if let Some(clip) = track.clip(id) {
            return Some((track, None, clip));
        }
        (track.takes.iter()).find_map(|take| {
            let clip = take.clips.iter().find(|clip| clip.id == id)?;
            Some((track, Some(take.id.0), clip))
        })
    });
    let Some((track, take, clip)) = found else {
        return Err(format!("There's no clip {}.", id.0));
    };
    let mut entry = json!({
        "id": clip.id.0,
        "name": clip.name,
        "track": track.id.0,
        "take": take,
        "start": beats(clip.start),
        "length": beats(clip.length),
        "end": beats(clip.end()),
    });
    match &clip.content {
        ClipContent::Midi(midi) => {
            let mut notes: Vec<&Note> = midi.notes.iter().collect();
            notes.sort_by_key(|note| (note.start, note.key, note.id));
            let controllers: Vec<Json> = (midi.controllers.iter())
                .map(|(&controller, curve)| controller_entry(controller, curve))
                .collect();
            entry["kind"] = "midi".into();
            entry["offset"] = beats(midi.offset);
            entry["notes"] = notes.into_iter().map(note).collect::<Vec<_>>().into();
            entry["controllers"] = controllers.into();
        }
        ClipContent::Audio(audio) => {
            entry["kind"] = "audio".into();
            entry["file"] = audio.file.as_str().into();
            match (audio.offset, &audio.warp) {
                (Offset::Beats(offset), Some(warp)) => {
                    let rate = window.editor().sample_rate_of(&audio.file);
                    let markers: Vec<Json> = (warp.markers.iter())
                        .map(|m| json!({ "frame": m.frame, "beat": beats(m.beat) }))
                        .collect();
                    let slice = &warp.slice;
                    entry["offset"] = beats(offset);
                    entry["warp"] = json!({
                        "mode": warp.mode.key(),
                        "tempo": json::to_json(&Value::Number(warp.clip_tempo(rate))),
                        "markers": markers,
                        "transpose": warp.transpose,
                        "detune": warp.detune,
                        "slice": {
                            "preserve": slice.preserve.key(),
                            "transient_loop": slice.transient_loop.key(),
                            "envelope": json::to_json(&Value::Number(slice.envelope)),
                        },
                        "spectral": {
                            "high_quality": warp.spectral.high_quality,
                            "formants": json::to_json(&Value::Number(warp.spectral.formants)),
                            "envelope": json::to_json(&Value::Number(warp.spectral.envelope)),
                        },
                        "granular": {
                            "follow_pitch": warp.granular.follow_pitch,
                            "grain_size": json::to_json(&Value::Number(warp.granular.grain_size)),
                            "fluctuation": json::to_json(&Value::Number(warp.granular.fluctuation)),
                        },
                    });
                }
                (offset, _) => entry["offset_frames"] = offset.frames().unwrap_or(0).into(),
            }
            entry["gain"] = short(audio.gain);
            // Each curve in the clip's own time, by its parameter's key.
            let curves: Map<String, Json> = (audio.automation.iter())
                .map(|lane| {
                    let points: Vec<Json> = (lane.points.iter())
                        .map(|p| json!({ "at": beats(p.at), "value": json::to_json(&Value::Number(p.value)) }))
                        .collect();
                    (lane.param.key().to_owned(), points.into())
                })
                .collect();
            if !curves.is_empty() {
                entry["curves"] = curves.into();
            }
            entry["fade_in"] = beats(audio.fade_in);
            entry["fade_out"] = beats(audio.fade_out);
            if let Some(file) = window.editor().files.get(&audio.file) {
                entry["sample_rate"] = file.sample_rate.into();
                entry["frames"] = file.frames.into();
                entry["channels"] = file.channels.into();
                if let Some(tempo) = file.tempo {
                    entry["file_tempo"] = json::to_json(&Value::Number(tempo));
                }
            }
        }
    }
    Ok(entry)
}

/// A note: its key, where it starts and how long it is, how hard it starts
/// and, where it isn't the usual, how fast it's let go, and its expression,
/// each point's time from the note's start.
fn note(note: &Note) -> Json {
    let mut entry = json!({
        "id": note.id.0,
        "key": note.key,
        "start": beats(note.start),
        "length": beats(note.length),
        "velocity": short(note.velocity),
    });
    if note.release_velocity != Note::RELEASE_VELOCITY {
        entry["release_velocity"] = short(note.release_velocity);
    }
    let expression = &note.expression;
    for (name, curve) in [
        ("pitch_bend", &expression.pitch_bend),
        ("pressure", &expression.pressure),
        ("timbre", &expression.timbre),
    ] {
        if !curve.is_empty() {
            entry[name] = points(curve);
        }
    }
    entry
}

fn controller_entry(controller: Controller, curve: &[Point]) -> Json {
    let mut entry = json!({ "controller": controller.number() });
    if let Some(name) = controller.name() {
        entry["name"] = name.into();
    }
    entry["points"] = points(curve);
    entry
}

fn points(curve: &[Point]) -> Json {
    let points: Vec<Json> = (curve.iter())
        .map(|point| json!({ "at": beats(point.at), "value": short(point.value) }))
        .collect();
    points.into()
}

/// One device: what it is, and each of its parameters' number, name,
/// range, unit and value.
pub(crate) fn device(window: &impl Window, id: DeviceId) -> Result<Json, String> {
    let project = window.project();
    let Some((chain, index, device)) = project.find_device(id) else {
        return Err(format!("There's no device {}.", id.0));
    };
    let mut entry = device_entry(device);
    entry["chain"] = chain_entry(chain);
    entry["position"] = index.into();
    match &device.kind {
        DeviceKind::Native(native) => {
            entry["params"] = native_params(native);
            if let Some(sample) = &native.sample {
                entry["sample"] = sample.as_str().into();
            }
        }
        DeviceKind::Plugin(plugin) => {
            let format = match plugin.format {
                PluginFormat::Clap => "clap",
                PluginFormat::Vst3 => "vst3",
            };
            entry["format"] = format.into();
            entry["plugin"] = plugin.id.as_str().into();
            let set = |id: u32| plugin.settings.param(id);
            entry["params"] = match window.plugin_params(id) {
                Some(params) => (params.iter())
                    .map(|(param, spec)| {
                        let mut entry = json!({
                            "param": param,
                            "name": spec.name,
                            "min": spec.min,
                            "max": spec.max,
                            "default": spec.default,
                        });
                        if let Some(value) = set(*param) {
                            entry["value"] = value.into();
                        }
                        entry
                    })
                    .collect::<Vec<_>>()
                    .into(),
                // Only what the project sets, while the plugin isn't loaded.
                None => (plugin.settings.params.iter())
                    .map(|param| json!({ "param": param.id, "value": param.value }))
                    .collect::<Vec<_>>()
                    .into(),
            };
            if window.plugin_params(id).is_none() {
                entry["loaded"] = false.into();
            }
        }
        DeviceKind::TestGain { gain } => entry["gain"] = short(*gain),
        DeviceKind::TestInstrument { level } => entry["level"] = short(*level),
    }
    Ok(entry)
}

/// A device chain: a track's, by its id, or "master".
fn chain_entry(chain: Chain) -> Json {
    match chain {
        Chain::Track(track) => json!({ "track": track.0 }),
        Chain::Master => "master".into(),
    }
}

/// A built-in device's parameters, each with its number, which
/// `device.set_param` and automation take, its range, its unit and its
/// value, also as the device panel shows it.
fn native_params(native: &NativeDevice) -> Json {
    let params: Vec<Json> = (native.params().iter().enumerate())
        .map(|(number, param)| {
            let value = native.value(number).unwrap_or(param.default);
            let mut entry = json!({
                "param": number,
                "name": param.full_name(),
                "value": value,
                "shown": param.format(value),
                "min": param.min,
                "max": param.max,
                "default": param.default,
                "unit": unit(param.unit),
            });
            if let Unit::Choice(choices) = param.unit {
                entry["choices"] = choices.to_vec().into();
            }
            if param.stepped {
                entry["stepped"] = true.into();
            }
            entry
        })
        .collect();
    params.into()
}

fn unit(unit: Unit) -> &'static str {
    match unit {
        Unit::Number => "number",
        Unit::Decibels => "dB",
        Unit::Gain => "linear gain",
        Unit::Hertz => "Hz",
        Unit::Milliseconds => "ms",
        Unit::Seconds => "s",
        Unit::Percent => "%",
        Unit::Semitones => "semitones",
        Unit::Cents => "cents",
        Unit::Ratio => "ratio",
        Unit::Pan => "pan, from -1 (left) to 1 (right)",
        Unit::Note => "MIDI note number",
        Unit::Switch => "0 off, 1 on",
        Unit::Choice(_) => "choice, by its number from 0",
    }
}

/// The automation of the parameter `param`: its points, each a position
/// and a value in the parameter's units.
pub(crate) fn automation(window: &impl Window, param: ParamAddress) -> Result<Json, String> {
    let project = window.project();
    if !project.has_param(param) {
        return Err(format!("There's no parameter {}.", param_text(param)));
    }
    let Some(lane) = project.lane(param) else {
        return Ok(json!({ "param": param_text(param), "points": [] }));
    };
    Ok(lane_entry(lane))
}

fn lane_entry(lane: &Lane) -> Json {
    let points: Vec<Json> = (lane.points.iter())
        .map(|point| json!({ "at": beats(point.at), "value": point.value }))
        .collect();
    json!({
        "param": param_text(lane.param),
        "points": points,
        "overridden": lane.overridden,
    })
}

/// `text` as a parameter, as `param_text` writes them.
pub(crate) fn parse_param(text: &str) -> Option<ParamAddress> {
    ParamAddress::from_value(&Value::Text(text.to_owned()))
}

/// The editor's state: the selection, the selected track, the focus, the
/// start marker, the grid, what's in sight, and the piano roll's clip and
/// selected notes.
pub(crate) fn editor(window: &impl Window) -> Json {
    let editor = window.editor();
    let selection = &editor.selection;
    let ids = |ids: &mut dyn Iterator<Item = u32>| ids.collect::<Vec<_>>();
    let time = selection.time.as_ref().map(|time| {
        json!({
            "start": beats(time.range.start),
            "end": beats(time.range.end),
            "tracks": ids(&mut time.tracks.iter().map(|t| t.0)),
            "master": time.master,
        })
    });
    let selected_track = editor.selected_track.map(chain_entry);
    let grid = &editor.grid;
    let viewport = &editor.viewport;
    let width = f64::from(viewport.width) / viewport.zoom.max(f64::MIN_POSITIVE);
    let roll = &editor.piano_roll;
    let sample = &editor.sample_editor;
    let lane = match roll.lane {
        PianoRollLane::Velocity => json!("velocity"),
        PianoRollLane::Controller(controller) => controller_entry(controller, &[]),
    };
    json!({
        "selection": {
            "clips": ids(&mut selection.clips.iter().map(|c| c.0)),
            "tracks": ids(&mut selection.tracks.iter().map(|t| t.0)),
            "time": time,
        },
        "selected_track": selected_track,
        "focus": editor.focus.panel(),
        "start_marker": beats(editor.start_marker),
        "grid": {
            "step": grid.fixed.map(|step| step.name(grid.triplet)),
            "triplet": grid.triplet,
            "snap": grid.snap,
        },
        "in_sight": {
            "start": beats(viewport.start),
            "end": viewport.start.to_f64() + width,
        },
        "follow": editor.follow,
        "piano_roll": {
            "clip": roll.clip.map(|clip| clip.0),
            "notes": ids(&mut roll.notes.iter().map(|n| n.0)),
            "draw": roll.draw,
            "lane": lane,
        },
        "sample_editor": {
            "clip": sample.clip.map(|clip| clip.0),
            "markers": sample.markers.iter().map(|&beat| beats(beat)).collect::<Vec<_>>(),
            "lane": sample.lane.key(),
        },
    })
}

/// The undo history: each step's name, oldest first, and how many are
/// done.
pub(crate) fn history(window: &impl Window) -> Json {
    let steps = window.history();
    let names: Vec<&str> = (0..steps.len()).filter_map(|i| steps.name(i)).collect();
    json!({
        "steps": names,
        "done": steps.done(),
        "undo": steps.undo_name(),
        "redo": steps.redo_name(),
    })
}

fn beats(beats: Beats) -> Json {
    json::to_json(&Value::Beats(beats))
}

/// A gain, a pan or a velocity, as short as it reads back the same:
/// `0.7`, not `0.699999988079071`.
fn short(value: f32) -> Json {
    let value: f64 = value.to_string().parse().unwrap_or(f64::NAN);
    json::to_json(&Value::Number(value))
}
