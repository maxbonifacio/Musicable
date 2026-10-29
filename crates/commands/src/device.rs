//! The device commands: adding Musicable's own devices, plugins and presets
//! to a track's devices or the master's, setting their parameters,
//! bypassing and deactivating them, selecting, moving and copying them,
//! giving samplers their samples, showing and closing plugins' windows,
//! saving devices as presets, and deleting devices.
//!
//! While the devices panel has the focus, the edit commands act on the
//! devices it has selected through the handlers here: Cut, Copy and Paste
//! with the panel's own clipboard, Duplicate, Delete and Select All.

use std::path::Path;

use musicable_model::automation::Lane;
use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginKind, PluginSettings};
use musicable_model::routing::{Tap, Wire};
use musicable_model::{
    Chain, Device, DeviceId, DeviceKind, Misplaced, PluginFormat, Project, TrackId, TrackKind,
};

use crate::editor::{DeviceClipboard, Editor, Focus, Selection, TakenDevice};
use crate::{
    Args, Command, CommandError, CommandId, Context, Loop, Param, Registry, Request, Transaction,
    Type, Value, When, wire,
};

/// Adds one of Musicable's own devices to a track's devices, or the
/// master's.
pub const ADD: CommandId = CommandId::new("device.add");
/// Adds a plugin to a track's devices, or the master's.
pub const ADD_PLUGIN: CommandId = CommandId::new("device.add_plugin");
/// Adds the device a preset holds to a track's devices, or the master's.
pub const ADD_PRESET: CommandId = CommandId::new("device.add_preset");
/// Saves a device as a preset, for the browser.
pub const SAVE_PRESET: CommandId = CommandId::new("device.save_preset");
/// Sets a device's parameter, or leaves a plugin's to its state.
pub const SET_PARAM: CommandId = CommandId::new("device.set_param");
/// Bypasses devices, or brings them back: a bypassed device stops, and what
/// comes into it plays on, as late as its latency would make it.
pub const SET_BYPASS: CommandId = CommandId::new("device.set_bypass");
/// Deactivates devices, which takes them out of the signal, latency and
/// all, or activates them.
pub const SET_ACTIVE: CommandId = CommandId::new("device.set_active");
/// Bypasses the devices selected in the devices panel, or brings them back
/// if every one is bypassed.
pub const TOGGLE_BYPASS: CommandId = CommandId::new("device.toggle_bypass");
/// Selects devices in the devices panel, for the edit commands, and gives
/// the panel the focus.
pub const SELECT: CommandId = CommandId::new("device.select");
/// Moves devices to a track's devices, or the master's, side by side in the
/// order they play. They keep their ids, so their lanes, their plugins and
/// their sound go with them.
pub const MOVE: CommandId = CommandId::new("device.move");
/// Moves each run of the selected devices one place towards the start of
/// the chain, past the device before it.
pub const MOVE_LEFT: CommandId = CommandId::new("device.move_left");
/// Moves each run of the selected devices one place towards the end of the
/// chain, past the device after it.
pub const MOVE_RIGHT: CommandId = CommandId::new("device.move_right");
/// Puts copies of devices on a track's devices, or the master's, side by
/// side in the order they play, each plugin's state taken afresh.
pub const COPY: CommandId = CommandId::new("device.copy");
/// Gives a sampler a sample, or takes its sample away.
pub const SET_SAMPLE: CommandId = CommandId::new("device.set_sample");
/// Asks for an audio file for a sampler to play: see [`SET_SAMPLE`].
pub const LOAD_SAMPLE: CommandId = CommandId::new("device.load_sample");
/// Shows a plugin's own window.
pub const SHOW_WINDOW: CommandId = CommandId::new("device.show_window");
/// Closes a plugin's own window.
pub const HIDE_WINDOW: CommandId = CommandId::new("device.hide_window");
/// Deletes a device.
pub const DELETE: CommandId = CommandId::new("device.delete");

const DEVICE: Param = Param::required("device", Type::Device, "The device.");
const DEVICES: Param = Param::required("devices", Type::DEVICES, "The devices.");
const TRACK: Param = Param::optional(
    "track",
    Type::Track,
    "The track it goes on: the selected track without it.",
);
const MASTER: Param = Param::optional("master", Type::Bool, "Puts it on the master instead.");
const INDEX: Param = Param::optional(
    "index",
    Type::Int,
    "Where it goes in the chain: the end without it.",
);
const NEW_TRACK: Param = Param::optional(
    "new_track",
    Type::Bool,
    "Puts it on a new track named after it instead: a MIDI track for an instrument, an \
     audio track for an effect.",
);
/// Where `device.move` and `device.copy` put devices: a place as
/// `device.add` takes it.
const TO_TRACK: Param = Param::optional(
    "track",
    Type::Track,
    "The track they go on: the selected track without it.",
);
const TO_MASTER: Param = Param::optional("master", Type::Bool, "Puts them on the master instead.");
const TO_NEW_TRACK: Param = Param::optional(
    "new_track",
    Type::Bool,
    "Puts them on a new track named after the first instead: a MIDI track if one is an \
     instrument, an audio track if none is.",
);

/// The keys of Musicable's own devices, as `device.add` takes them.
const KINDS: &str = "eq, compressor, gate, delay, reverb, utility, sampler or synth";

/// The kinds of plugin, as `device.add_plugin` takes them.
pub(crate) const PLUGIN_KINDS: &[&str] = &["instrument", "midi_effect", "audio_effect"];

/// The `format` argument: a plugin's format.
pub(crate) const FORMAT: Param =
    Param::required("format", Type::Text, "The plugin's format: clap or vst3.");

/// The plugin format the `format` argument names.
pub(crate) fn plugin_format(args: &Args) -> Result<PluginFormat, CommandError> {
    match args.get::<String>("format")?.as_str() {
        "clap" => Ok(PluginFormat::Clap),
        "vst3" => Ok(PluginFormat::Vst3),
        other => Err(CommandError::invalid(format!(
            "there's no plugin format {other:?}: it's clap or vst3"
        ))),
    }
}

/// Registers the device commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(ADD, "Add Device").with_params(&[
            Param::required(
                "kind",
                Type::Text,
                "Which of Musicable's own devices: eq, compressor, gate, delay, reverb, \
                 utility, sampler or synth.",
            ),
            TRACK,
            MASTER,
            INDEX,
            NEW_TRACK,
        ]),
        |ctx, args| {
            let key: String = args.get("kind")?;
            let Some(kind) = NativeKind::from_key(&key) else {
                return Err(CommandError::invalid(format!(
                    "there's no built-in device {key:?}: it's {KINDS}"
                )));
            };
            let native = DeviceKind::Native(NativeDevice::new(kind));
            add(ctx, args, native, &format!("Add {}", kind.name()))
        },
    );
    registry.add(
        Command::new(ADD_PLUGIN, "Add Plugin").with_params(&[
            FORMAT,
            Param::required(
                "plugin",
                Type::Text,
                "The plugin's id in its format, such as org.surge-synth-team.surge-xt.",
            ),
            Param::required("name", Type::Text, "Its name, to show while it's missing."),
            Param::optional(
                "kind",
                Type::Choice(PLUGIN_KINDS),
                "What it does with notes: an instrument plays them, and a MIDI effect, such \
                 as an arpeggiator, plays them on changed, before the instrument on a MIDI \
                 track. An audio effect without it.",
            ),
            TRACK,
            MASTER,
            INDEX,
            NEW_TRACK,
        ]),
        |ctx, args| {
            let format = plugin_format(args)?;
            let id: String = args.get("plugin")?;
            if id.is_empty() {
                return Err(CommandError::invalid("a plugin's id can't be empty"));
            }
            // An audio effect without one: the choices are checked.
            let kind = (args.opt::<String>("kind")?)
                .and_then(|key| PluginKind::from_key(&key))
                .unwrap_or_default();
            let kind = DeviceKind::Plugin(PluginDevice {
                format,
                id,
                name: args.get("name")?,
                kind,
                inputs: Vec::new(),
                settings: PluginSettings::default(),
            });
            let name = format!("Add {}", kind.name());
            add(ctx, args, kind, &name)
        },
    );
    registry.add(
        Command::new(ADD_PRESET, "Add Preset").with_params(&[
            Param::required(
                "preset",
                Type::Text,
                "The preset file's path: Musicable's own, or a VST3 preset.",
            ),
            TRACK,
            MASTER,
            INDEX,
            NEW_TRACK,
        ]),
        |ctx, args| {
            let preset: String = args.get("preset")?;
            let preset = Path::new(&preset);
            let kind = ctx
                .library
                .load_preset(preset)
                .map_err(CommandError::invalid)?;
            let name = match preset.file_stem() {
                Some(stem) => format!("Add {}", stem.to_string_lossy()),
                None => "Add Preset".to_owned(),
            };
            add(ctx, args, kind, &name)
        },
    );
    registry.add(
        Command::new(SAVE_PRESET, "Save Device as Preset").with_params(&[
            DEVICE,
            Param::optional(
                "name",
                Type::Text,
                "The preset's name: the device's and a number without it.",
            ),
        ]),
        |ctx, args| {
            let device = args.get("device")?;
            let name: Option<String> = args.opt("name")?;
            let name = match name.as_deref().map(str::trim) {
                Some("") => return Err(CommandError::invalid("a preset's name can't be empty")),
                name => name,
            };
            let Some((_, _, found)) = ctx.document.project().find_device(device) else {
                return Err(no_device(device));
            };
            let kind = found.kind.clone();
            let saved = ctx
                .library
                .save_preset(device, &kind, device_name(&kind), name)
                .map_err(CommandError::invalid)?;
            Ok(Value::Text(saved.to_string_lossy().into_owned()))
        },
    );
    registry.add(
        Command::new(SET_PARAM, "Set Device Parameter").with_params(&[
            DEVICE,
            Param::required(
                "param",
                Type::Int,
                "The parameter: its place in a built-in device's table, from 0, or the \
                 plugin's id for it.",
            ),
            Param::optional(
                "value",
                Type::Number,
                "Its value, in its own units, such as Hz or dB. A built-in device keeps it \
                 in the parameter's range. Without it, a plugin's parameter is left to the \
                 plugin's state.",
            ),
        ]),
        |ctx, args| {
            let device = args.get("device")?;
            let param = u32::try_from(args.get::<i64>("param")?)
                .map_err(|_| CommandError::invalid("a parameter's id is from 0 to 4294967295"))?;
            let value: Option<f64> = args.opt("value")?;
            let found = find(ctx, device)?;
            let name = format!("Change {}", device_name(&found.kind));
            match found.kind {
                DeviceKind::Native(ref native) => {
                    let Some(info) = native.params().get(param as usize) else {
                        return Err(CommandError::invalid(format!(
                            "the {} has no parameter {param}: they're 0 to {}",
                            native.kind.name(),
                            native.params().len() - 1
                        )));
                    };
                    let Some(value) = value else {
                        return Err(CommandError::invalid(format!(
                            "give {} a value",
                            info.full_name()
                        )));
                    };
                    ctx.document.transact(&name, |tx| {
                        // As if by hand: automation stops moving it.
                        tx.override_automation(ParamAddress::Device(device, param));
                        tx.set_native_param(device, param, value);
                    });
                }
                DeviceKind::Plugin(_) => {
                    ctx.document.transact(&name, |tx| {
                        tx.override_automation(ParamAddress::Device(device, param));
                        tx.set_plugin_param(device, param, value);
                    });
                }
                _ => {
                    return Err(CommandError::invalid(format!(
                        "device {} has no parameters",
                        device.0
                    )));
                }
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_BYPASS, "Set Device Bypass").with_params(&[
            DEVICES,
            Param::required(
                "bypassed",
                Type::Bool,
                "Whether they're bypassed: each stops, and what comes into it plays on, as \
                 late as its latency would make it, so the mix keeps its timing.",
            ),
        ]),
        |ctx, args| {
            let devices = devices(ctx, args)?;
            set_bypass(ctx, &devices, args.get("bypassed")?);
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_ACTIVE, "Set Device Active").with_params(&[
            DEVICES,
            Param::required(
                "active",
                Type::Bool,
                "Whether they're active. A deactivated device is out of the signal, latency \
                 and all, though it keeps its place and its settings, and a plugin stays \
                 loaded.",
            ),
        ]),
        |ctx, args| {
            let devices = devices(ctx, args)?;
            let active = args.get("active")?;
            let verb = if active { "Activate" } else { "Deactivate" };
            let name = step_name(ctx, &devices, verb);
            ctx.document.transact(&name, |tx| {
                for &device in &devices {
                    tx.set_device_active(device, active);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(TOGGLE_BYPASS, "Toggle Device Bypass").when(When::DevicesSelected),
        |ctx, _| {
            let devices = selected(ctx);
            let project = ctx.document.project();
            let bypassed = |&device: &DeviceId| {
                (project.find_device(device)).is_some_and(|(_, _, found)| found.bypassed)
            };
            let all = devices.iter().all(bypassed);
            set_bypass(ctx, &devices, !all);
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SELECT, "Select Devices").with_params(&[
            Param::required(
                "devices",
                Type::DEVICES,
                "The devices to select, all of one chain: a track's, or the master's. The \
                 devices panel shows it. With none, no devices are selected.",
            ),
            Param::optional(
                "toggle",
                Type::Bool,
                "Adds the devices to the selection, or takes away those already in it, \
                 instead of selecting just them.",
            ),
        ]),
        |ctx, args| {
            let devices: Vec<DeviceId> = args.get("devices")?;
            let toggle = args.opt("toggle")?.unwrap_or(false);
            let project = ctx.document.project();
            let mut chain = None;
            for &device in &devices {
                let Some((found, _, _)) = project.find_device(device) else {
                    return Err(no_device(device));
                };
                if chain.is_some_and(|chain| chain != found) {
                    return Err(CommandError::invalid(
                        "give devices of one chain: a track's, or the master's",
                    ));
                }
                chain = Some(found);
            }
            let editor = ctx.document.editor_mut();
            if let Some(chain) = chain {
                show_chain(editor, chain);
            }
            let selected = &mut editor.devices.selected;
            if !toggle {
                selected.clear();
            }
            for device in devices {
                if !toggle || !selected.remove(&device) {
                    selected.insert(device);
                }
            }
            editor.focus = Focus::DEVICES;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(MOVE, "Move Devices").with_params(&[
            DEVICES,
            TO_TRACK,
            TO_MASTER,
            Param::optional(
                "index",
                Type::Int,
                "Where the first of them goes in the chain, counting it without them: the \
                 end without it.",
            ),
            TO_NEW_TRACK,
        ]),
        |ctx, args| {
            let devices = devices(ctx, args)?;
            let (chain, index) = place(ctx, args, "devices")?;
            let name = step_name(ctx, &devices, "Move");
            let moved = ctx.document.transact(&name, |tx| {
                let moved = tx.at_once(|tx| {
                    let chain = chain.unwrap_or_else(|| new_track(tx, &devices));
                    move_devices(tx, &devices, chain, index)?;
                    Ok(chain)
                });
                let chain = moved.map_err(|refused| wire::refused(tx.project(), refused))?;
                keep_order(tx, chain)?;
                Ok::<_, CommandError>(chain)
            });
            let chain = moved?;
            select_devices(ctx, chain, &devices);
            Ok(Value::None)
        },
    );
    for (id, title, left) in [
        (MOVE_LEFT, "Move Devices Left", true),
        (MOVE_RIGHT, "Move Devices Right", false),
    ] {
        registry.add(
            Command::new(id, title).when(When::DevicesSelected),
            move |ctx, _| {
                nudge(ctx, left)?;
                Ok(Value::None)
            },
        );
    }
    registry.add(
        Command::new(COPY, "Copy Devices").with_params(&[
            DEVICES,
            TO_TRACK,
            TO_MASTER,
            Param::optional(
                "index",
                Type::Int,
                "Where the first copy goes in the chain: the end without it.",
            ),
            TO_NEW_TRACK,
        ]),
        |ctx, args| {
            let devices = devices(ctx, args)?;
            let (chain, index) = place(ctx, args, "devices")?;
            let name = step_name(ctx, &devices, "Copy");
            let taken = taken(ctx, &devices);
            let lanes = ctx.document.editor().behaviour.copies_take_automation;
            let (chain, copies) = ctx.document.transact(&name, |tx| {
                let chain = chain.unwrap_or_else(|| new_track(tx, &devices));
                (chain, add_copies(tx, taken, chain, index, lanes))
            });
            select_devices(ctx, chain, &copies);
            Ok(copies.into())
        },
    );
    registry.add(
        Command::new(SET_SAMPLE, "Set Sample").with_params(&[
            DEVICE,
            Param::optional(
                "file",
                Type::Text,
                "The audio file's path, as clips name files: relative to the project's \
                 folder, with / between names, or absolute. Without it, the sampler has no \
                 sample.",
            ),
        ]),
        |ctx, args| {
            let device = args.get("device")?;
            let file: Option<String> = args.opt("file")?;
            if file.as_ref().is_some_and(String::is_empty) {
                return Err(CommandError::invalid("a sample's path can't be empty"));
            }
            sampler(ctx, device)?;
            let name = match file {
                Some(_) => "Load Sample",
                None => "Remove Sample",
            };
            ctx.document
                .transact(name, |tx| tx.set_native_sample(device, file));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(LOAD_SAMPLE, "Load Sample...")
            .with_params(&[DEVICE])
            .not_for_agents(),
        |ctx, args| {
            let device = args.get("device")?;
            sampler(ctx, device)?;
            ctx.requests.push(Request::LoadSample(device));
            Ok(Value::None)
        },
    );
    for (id, title, show) in [
        (SHOW_WINDOW, "Show Plugin Window", true),
        (HIDE_WINDOW, "Hide Plugin Window", false),
    ] {
        registry.add(
            Command::new(id, title).with_params(&[DEVICE]),
            move |ctx, args| {
                let device = args.get("device")?;
                plugin(ctx, device)?;
                ctx.requests.push(Request::PluginWindow { device, show });
                Ok(Value::None)
            },
        );
    }
    registry.add(
        Command::new(DELETE, "Delete Device").with_params(&[DEVICE]),
        |ctx, args| {
            let device = args.get("device")?;
            let name = format!("Delete {}", device_name(&find(ctx, device)?.kind));
            ctx.document.transact(&name, |tx| {
                tx.remove_device(device);
            });
            Ok(Value::None)
        },
    );
}

/// Adds a device of `kind` where `args` say, as the change `name`, and
/// returns its id. The track it goes on, new or not, or the master, is
/// selected, and the app is asked to show the device there, so it doesn't
/// go out of sight: behind the piano roll, or on a track the devices panel
/// doesn't show.
fn add(
    ctx: &mut Context<'_>,
    args: &Args,
    kind: DeviceKind,
    name: &str,
) -> Result<Value, CommandError> {
    let what = match kind {
        DeviceKind::Plugin(_) => "plugin",
        _ => "device",
    };
    let (chain, index) = place(ctx, args, what)?;
    let (track_name, track) = track_for(&[&kind]);
    if let Some(chain) = chain
        && ctx
            .document
            .project()
            .fit_device(chain, index, &kind)
            .is_none()
    {
        return Err(no_notes(&kind));
    }
    let (device, chain) = ctx.document.transact(name, |tx| {
        let chain = match chain {
            Some(chain) => chain,
            None => Chain::Track(crate::track::add_track(tx, track_name, track)),
        };
        // A MIDI effect goes before the instrument, and an instrument after
        // the MIDI effects, so the notes pass through them in order.
        let index = tx
            .project()
            .fit_device(chain, index, &kind)
            .unwrap_or(index);
        let device = tx.new_device(kind);
        let id = device.id;
        tx.add_device(chain, index, device);
        (id, chain)
    });
    show_chain(ctx.document.editor_mut(), chain);
    ctx.requests.push(Request::ShowDevice(device));
    Ok(device.into())
}

/// Where `args` put devices: in a chain, or on a new track without one,
/// at an index in it. `what` names the devices in the error without a
/// track.
fn place(
    ctx: &Context<'_>,
    args: &Args,
    what: &str,
) -> Result<(Option<Chain>, usize), CommandError> {
    let index = args
        .opt::<i64>("index")?
        .map_or(usize::MAX, |index| usize::try_from(index).unwrap_or(0));
    let chain = match args.opt("new_track")?.unwrap_or(false) {
        true => None,
        false => Some(chain(
            ctx,
            args.opt("track")?,
            args.opt("master")?.unwrap_or(false),
            what,
        )?),
    };
    Ok((chain, index))
}

/// What a new track for devices of `kinds` is called, after the first of
/// them, and what it is: a MIDI track if one is an instrument or a MIDI
/// effect, an audio track if none is.
fn track_for(kinds: &[&DeviceKind]) -> (String, TrackKind) {
    let name = kinds.first().map_or("Device", |kind| device_name(kind));
    let notes = (kinds.iter()).any(|kind| kind.is_instrument() || kind.is_midi_effect());
    let kind = match notes {
        true => TrackKind::Midi,
        false => TrackKind::Audio,
    };
    (name.to_owned(), kind)
}

/// The error for a device of `kind`, a MIDI effect, put where there are no
/// notes for it.
fn no_notes(kind: &DeviceKind) -> CommandError {
    CommandError::invalid(format!(
        "{} is a MIDI effect, which goes on a MIDI track, before its instrument",
        device_name(kind)
    ))
}

/// The error for a chain whose devices `misplaced` says are out of order,
/// in `project` as the edit left it, before it was taken back.
fn misplaced(project: &Project, misplaced: Misplaced) -> CommandError {
    let name = |device| {
        project
            .find_device(device)
            .map_or("The device", |(_, _, found)| device_name(&found.kind))
            .to_owned()
    };
    match misplaced {
        Misplaced::NoNotes(device) => CommandError::invalid(format!(
            "{} is a MIDI effect, which goes on a MIDI track, before its instrument",
            name(device)
        )),
        Misplaced::AfterInstrument(device) => {
            let chain = project.find_device(device).map(|(chain, ..)| chain);
            let instrument = chain
                .and_then(|chain| project.devices(chain))
                .and_then(|devices| devices.iter().find(|d| d.kind.is_instrument()))
                .map_or("the instrument", |found| device_name(&found.kind));
            CommandError::invalid(format!(
                "{} is a MIDI effect, which goes before {instrument}, the instrument that \
                 plays the notes it plays on",
                name(device)
            ))
        }
    }
}

/// Adds a new track for `devices` (see [`track_for`]), where `device.add`
/// puts one, and returns its chain.
fn new_track(tx: &mut Transaction<'_>, devices: &[DeviceId]) -> Chain {
    let project = tx.project();
    let kinds: Vec<&DeviceKind> = (devices.iter())
        .filter_map(|&device| project.find_device(device))
        .map(|(_, _, found)| &found.kind)
        .collect();
    let (name, kind) = track_for(&kinds);
    Chain::Track(crate::track::add_track(tx, name, kind))
}

/// Makes `chain` the selected track, for the devices panel to show it. If
/// it's another, its track is selected, as a click on its header selects
/// it, and the master leaves nothing selected.
fn show_chain(editor: &mut Editor, chain: Chain) {
    if editor.selected_track != Some(chain) {
        editor.selection = Selection {
            tracks: chain.track().into_iter().collect(),
            ..Selection::default()
        };
        editor.select_track(chain);
    }
}

/// The chain a new device goes in: the master's with `master`, else
/// `track`'s, else the selected track's, which can be the master's. `what`
/// names the device in the error without a track.
pub(crate) fn chain(
    ctx: &Context<'_>,
    track: Option<TrackId>,
    master: bool,
    what: &str,
) -> Result<Chain, CommandError> {
    if master {
        return Ok(Chain::Master);
    }
    let track = match track
        .map(Chain::Track)
        .or(ctx.document.editor().selected_track)
    {
        Some(Chain::Track(track)) => track,
        Some(Chain::Master) => return Ok(Chain::Master),
        None => {
            return Err(CommandError::invalid(format!(
                "select a track for the {what} to go on"
            )));
        }
    };
    match ctx.document.project().track(track) {
        Some(_) => Ok(Chain::Track(track)),
        None => Err(CommandError::invalid(format!(
            "there's no track {}",
            track.0
        ))),
    }
}

/// The device with id `device`, if the project has it.
fn find<'a>(ctx: &'a Context<'_>, device: DeviceId) -> Result<&'a Device, CommandError> {
    match ctx.document.project().find_device(device) {
        Some((_, _, found)) => Ok(found),
        None => Err(no_device(device)),
    }
}

/// The devices `args` give, each once and in the order they play, if the
/// project has every one.
fn devices(ctx: &Context<'_>, args: &Args) -> Result<Vec<DeviceId>, CommandError> {
    let devices: Vec<DeviceId> = args.get("devices")?;
    if devices.is_empty() {
        return Err(CommandError::invalid("give at least one device"));
    }
    for &device in &devices {
        find(ctx, device)?;
    }
    Ok(in_order(ctx.document.project(), &devices))
}

/// `devices` in the order they play: chain by chain, the tracks' in order
/// and then the master's.
fn in_order(project: &Project, devices: &[DeviceId]) -> Vec<DeviceId> {
    (project.chains())
        .flat_map(|(_, chain)| chain)
        .map(|device| device.id)
        .filter(|id| devices.contains(id))
        .collect()
}

/// The devices the devices panel has selected, in the order they play.
fn selected(ctx: &Context<'_>) -> Vec<DeviceId> {
    let (project, editor) = (ctx.document.project(), ctx.document.editor());
    editor.selected_devices(project)
}

/// What the Edit menu calls `verb` done to `devices`: "Bypass EQ" for one,
/// and "Bypass 3 Devices" for several.
fn step_name(ctx: &Context<'_>, devices: &[DeviceId], verb: &str) -> String {
    let project = ctx.document.project();
    let kinds: Vec<&DeviceKind> = (devices.iter())
        .filter_map(|&device| project.find_device(device))
        .map(|(_, _, found)| &found.kind)
        .collect();
    named(verb, &kinds)
}

/// What the Edit menu calls `verb` done to devices of `kinds`, as
/// [`step_name`] has it.
fn named(verb: &str, kinds: &[&DeviceKind]) -> String {
    match kinds {
        [kind] => format!("{verb} {}", device_name(kind)),
        _ => format!("{verb} {} Devices", kinds.len()),
    }
}

/// Bypasses `devices`, or brings them back, as one step.
fn set_bypass(ctx: &mut Context<'_>, devices: &[DeviceId], bypassed: bool) {
    let verb = if bypassed { "Bypass" } else { "Bring Back" };
    let name = step_name(ctx, devices, verb);
    ctx.document.transact(&name, |tx| {
        for &device in devices {
            // As if by hand: its lane stops moving it.
            tx.override_automation(ParamAddress::Bypass(device));
            tx.set_device_bypass(device, bypassed);
        }
    });
}

/// Moves `devices`, in the order they play, to `index` in `chain`, counting
/// the chain without them, side by side, unless a move closes a loop
/// through a wire: run it [at once](Transaction::at_once), so that's
/// checked once they've all moved.
fn move_devices(
    tx: &mut Transaction<'_>,
    devices: &[DeviceId],
    chain: Chain,
    index: usize,
) -> Result<(), Loop> {
    let Some(others) = tx.project().devices(chain) else {
        return Ok(());
    };
    // The device they go in front of, if they don't go at the end.
    let before = (others.iter())
        .map(|device| device.id)
        .filter(|id| !devices.contains(id))
        .nth(index);
    for &device in devices {
        let at = before.and_then(|before| {
            let chain = tx.project().devices(chain).unwrap_or_default();
            (chain.iter())
                .filter(|other| other.id != device)
                .position(|other| other.id == before)
        });
        tx.move_device(device, chain, at.unwrap_or(usize::MAX))?;
    }
    Ok(())
}

/// Moves each run of the selected devices one place along their chain:
/// past the device before it with `left`, or else past the one after it.
/// A run at that end of the chain stays. Moves that would close a loop
/// through a wire are refused, all of them.
fn nudge(ctx: &mut Context<'_>, left: bool) -> Result<(), CommandError> {
    let selected = selected(ctx);
    let project = ctx.document.project();
    let Some(chain) = ctx.document.editor().selected_track else {
        return Ok(());
    };
    let ids: Vec<DeviceId> = (project.devices(chain).unwrap_or_default().iter())
        .map(|device| device.id)
        .collect();
    // Each run's neighbour goes to its other side. Runs have devices
    // between them, so each move leaves the next run where it was.
    let (mut moves, mut moving) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < ids.len() {
        if !selected.contains(&ids[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < ids.len() && selected.contains(&ids[i]) {
            i += 1;
        }
        let neighbour = match left {
            true if start > 0 => Some((ids[start - 1], i - 1)),
            false if i < ids.len() => Some((ids[i], start)),
            _ => None,
        };
        if let Some(neighbour) = neighbour {
            moves.push(neighbour);
            moving.extend_from_slice(&ids[start..i]);
        }
    }
    let name = step_name(ctx, &moving, "Move");
    let moved = ctx.document.transact(&name, |tx| {
        let moved = tx.at_once(|tx| {
            for (device, index) in moves {
                tx.move_device(device, chain, index)?;
            }
            Ok(())
        });
        moved.map_err(|refused| wire::refused(tx.project(), refused))?;
        keep_order(tx, chain)
    });
    moved?;
    select_devices(ctx, chain, &selected);
    Ok(())
}

/// Takes back every edit `tx` has made, and says why, if they left the
/// devices of `chain` out of order: see [`Project::check_order`].
fn keep_order(tx: &mut Transaction<'_>, chain: Chain) -> Result<(), CommandError> {
    let Err(wrong) = tx.project().check_order(chain) else {
        return Ok(());
    };
    let error = misplaced(tx.project(), wrong);
    tx.cancel();
    Err(error)
}

/// `devices` as copies take them, each with the lanes that automate it and
/// the wires into it: a plugin's state taken afresh, as Save as Preset
/// takes it, since a change made in its window can reach the project late.
fn taken(ctx: &mut Context<'_>, devices: &[DeviceId]) -> Vec<TakenDevice> {
    let mut taken = Vec::with_capacity(devices.len());
    for &id in devices {
        let state = ctx.library.plugin_state(id);
        let project = ctx.document.project();
        let Some((_, _, device)) = project.find_device(id) else {
            continue;
        };
        let mut device = device.clone();
        if let (DeviceKind::Plugin(plugin), Some(state)) = (&mut device.kind, state) {
            plugin.settings.state = Some(state);
        }
        let lanes = (project.automation.iter())
            .filter(|lane| lane.param.device() == Some(id))
            .cloned()
            .collect();
        let wires = (project.wires.iter())
            .filter(|wire| wire.to.device() == id)
            .map(|wire| (*wire, project.lane(ParamAddress::Wire(wire.id)).cloned()))
            .collect();
        taken.push(TakenDevice {
            device,
            lanes,
            wires,
        });
    }
    taken
}

/// Puts copies of `taken` at `index` in `chain`, or at the end if `index`
/// is past it, side by side, and returns their ids. Each is a new device
/// with the original's settings, and with `lanes`, lanes of its own with
/// the original's points.
///
/// Each copy keeps the wires into the original, so a copied compressor
/// keeps its key: from the same taps, or from the copy of a device copied
/// with it. A wire that would close a loop, as into a copy put before the
/// device it's keyed after, is left out.
fn add_copies(
    tx: &mut Transaction<'_>,
    taken: Vec<TakenDevice>,
    chain: Chain,
    index: usize,
    lanes: bool,
) -> Vec<DeviceId> {
    let mut copies = Vec::with_capacity(taken.len());
    // Each original's id, its copy's, and the wires into it.
    let mut copied = Vec::with_capacity(taken.len());
    for TakenDevice {
        device: original,
        lanes: its_lanes,
        wires,
    } in taken
    {
        let Device {
            id: original,
            kind,
            bypassed,
            active,
        } = original;
        // A MIDI effect goes before the instrument, and an instrument after
        // the MIDI effects. One where there are no notes is left out.
        let index = index.saturating_add(copies.len());
        let Some(index) = tx.project().fit_device(chain, index, &kind) else {
            continue;
        };
        let copy = Device {
            bypassed,
            active,
            ..tx.new_device(kind)
        };
        let id = copy.id;
        tx.add_device(chain, index, copy);
        // Left out, as a file couldn't hold its settings.
        if tx.project().find_device(id).is_none() {
            continue;
        }
        copies.push(id);
        for lane in its_lanes.into_iter().filter(|_| lanes) {
            if let Some(param) = lane.param.with_device(id) {
                tx.add_lane(usize::MAX, Lane { param, ..lane });
            }
        }
        copied.push((original, id, wires));
    }
    let copy_of = |device| {
        (copied.iter()).find_map(|&(original, copy, _)| (original == device).then_some(copy))
    };
    for (_, copy, wires) in &copied {
        for (wire, lane) in wires {
            let from = match wire.from {
                Tap::Device(device) => Tap::Device(copy_of(device).unwrap_or(device)),
                tap => tap,
            };
            let new = Wire {
                gain: wire.gain,
                ..tx.new_wire(from, wire.to.on(*copy))
            };
            if tx.add_wire(usize::MAX, new).is_ok()
                && let Some(lane) = lane.as_ref().filter(|_| lanes)
            {
                let param = ParamAddress::Wire(new.id);
                tx.add_lane(
                    usize::MAX,
                    Lane {
                        param,
                        ..lane.clone()
                    },
                );
            }
        }
    }
    copies
}

/// Shows `chain` in the devices panel (see [`show_chain`]) with `devices`
/// selected, and asks the app to bring the first into sight.
fn select_devices(ctx: &mut Context<'_>, chain: Chain, devices: &[DeviceId]) {
    let editor = ctx.document.editor_mut();
    show_chain(editor, chain);
    editor.devices.selected = devices.iter().copied().collect();
    if let Some(&first) = devices.first() {
        ctx.requests.push(Request::ShowDevice(first));
    }
}

/// Takes the selected devices for Paste, as Copy does while the devices
/// panel has the focus.
pub(crate) fn copy(ctx: &mut Context<'_>) {
    let devices = selected(ctx);
    if !devices.is_empty() {
        let devices = taken(ctx, &devices);
        ctx.document.editor_mut().devices.clipboard = Some(DeviceClipboard { devices });
    }
}

/// Takes the selected devices for Paste, then deletes them.
pub(crate) fn cut(ctx: &mut Context<'_>) {
    copy(ctx);
    delete_selected(ctx, "Cut");
}

/// Deletes the selected devices, as one step.
pub(crate) fn delete(ctx: &mut Context<'_>) {
    delete_selected(ctx, "Delete");
}

fn delete_selected(ctx: &mut Context<'_>, verb: &str) {
    let devices = selected(ctx);
    let name = step_name(ctx, &devices, verb);
    ctx.document.transact(&name, |tx| {
        for device in devices {
            tx.remove_device(device);
        }
    });
}

/// Puts copies of what Cut or Copy took in the devices panel in front of
/// the first selected device, or at the end of the chain it shows, and
/// selects them.
pub(crate) fn paste(ctx: &mut Context<'_>) {
    let (project, editor) = (ctx.document.project(), ctx.document.editor());
    let (Some(chain), Some(clipboard)) = (editor.selected_track, &editor.devices.clipboard) else {
        return;
    };
    let index = (editor.selected_devices(project).first())
        .and_then(|&first| project.find_device(first))
        .map_or(usize::MAX, |(_, index, _)| index);
    let kinds: Vec<&DeviceKind> = (clipboard.devices.iter()).map(|t| &t.device.kind).collect();
    let name = named("Paste", &kinds);
    let taken = clipboard.devices.clone();
    let lanes = editor.behaviour.copies_take_automation;
    let copies = (ctx.document).transact(&name, |tx| add_copies(tx, taken, chain, index, lanes));
    select_devices(ctx, chain, &copies);
}

/// Puts copies of the selected devices just after the last of them, and
/// selects the copies, so doing it again makes more.
pub(crate) fn duplicate(ctx: &mut Context<'_>) {
    let devices = selected(ctx);
    let Some((chain, last, _)) =
        (devices.last()).and_then(|&last| ctx.document.project().find_device(last))
    else {
        return;
    };
    let name = step_name(ctx, &devices, "Duplicate");
    let taken = taken(ctx, &devices);
    let lanes = ctx.document.editor().behaviour.copies_take_automation;
    let copies = (ctx.document).transact(&name, |tx| add_copies(tx, taken, chain, last + 1, lanes));
    select_devices(ctx, chain, &copies);
}

/// Selects every device of the chain the devices panel shows.
pub(crate) fn select_all(ctx: &mut Context<'_>) {
    let (project, editor) = ctx.document.parts();
    let shown = (editor.selected_track).and_then(|chain| project.devices(chain));
    editor.devices.selected = shown.unwrap_or_default().iter().map(|d| d.id).collect();
}

/// The plugin device with id `device`, if the project has it.
fn plugin<'a>(ctx: &'a Context<'_>, device: DeviceId) -> Result<&'a PluginDevice, CommandError> {
    find(ctx, device)?
        .kind
        .plugin()
        .ok_or_else(|| CommandError::invalid(format!("device {} isn't a plugin", device.0)))
}

/// The sampler with id `device`, if the project has it.
fn sampler<'a>(ctx: &'a Context<'_>, device: DeviceId) -> Result<&'a NativeDevice, CommandError> {
    find(ctx, device)?
        .kind
        .native()
        .filter(|native| native.kind == NativeKind::Sampler)
        .ok_or_else(|| CommandError::invalid(format!("device {} isn't a sampler", device.0)))
}

fn no_device(device: DeviceId) -> CommandError {
    CommandError::invalid(format!("there's no device {}", device.0))
}

/// What the browser and the Edit menu call a device: a built-in device by
/// its kind, such as "EQ", and a plugin by its name.
pub fn device_name(kind: &DeviceKind) -> &str {
    kind.name()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use musicable_model::automation::Point;
    use musicable_model::clip::ClipContent;
    use musicable_model::native::eq;
    use musicable_model::plugin::{PluginParam, PluginState};
    use musicable_model::time::{BeatRange, Beats};
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::edit;
    use crate::testing::Setup;

    fn add(name: &str) -> Args {
        Args::new()
            .with("format", "clap")
            .with("plugin", "org.example.plugin")
            .with("name", name)
    }

    fn settings(setup: &Setup, device: DeviceId) -> PluginSettings {
        let project = setup.document.project();
        let plugin = project.find_device(device).unwrap().2.kind.plugin();
        plugin.unwrap().settings.clone()
    }

    #[test]
    fn plugins_go_on_the_selected_track_a_track_or_the_master() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let mut setup = Setup::new(project);
        assert_eq!(
            setup.run_with(&ADD_PLUGIN, add("Synth")),
            Err(CommandError::invalid(
                "select a track for the plugin to go on"
            ))
        );
        setup.select_tracks(&[keys]);
        let synth = setup
            .run_with(&ADD_PLUGIN, add("Synth").with("kind", "instrument"))
            .unwrap();
        let eq = setup.run_with(&ADD_PLUGIN, add("EQ")).unwrap();
        // In front of the synth.
        let args = add("Arp").with("index", 0_i64).with("track", keys);
        let arp = setup.run_with(&ADD_PLUGIN, args).unwrap();
        let reverb = setup
            .run_with(&ADD_PLUGIN, add(" ").with("track", bass))
            .unwrap();
        let limiter = setup
            .run_with(&ADD_PLUGIN, add("Limiter").with("master", true))
            .unwrap();

        let project = setup.document.project();
        let ids = |devices: &[musicable_model::Device]| -> Vec<Value> {
            devices.iter().map(|d| d.id.into()).collect()
        };
        assert_eq!(ids(&project.tracks[0].devices), [arp, synth, eq]);
        assert_eq!(ids(&project.tracks[1].devices), [reverb]);
        assert_eq!(ids(&project.master.devices), [limiter]);
        let synth = project.tracks[0].devices[1].kind.plugin().unwrap();
        assert_eq!(synth.kind, PluginKind::Instrument);
        assert_eq!(synth.settings, PluginSettings::default());
        assert_eq!(
            setup.committed_names(),
            [
                "Add Synth",
                "Add EQ",
                "Add Arp",
                "Add Plugin",
                "Add Limiter"
            ]
        );

        let wrong = add("Synth").with("format", "au").with("track", keys);
        assert_eq!(
            setup.run_with(&ADD_PLUGIN, wrong),
            Err(CommandError::invalid(
                "there's no plugin format \"au\": it's clap or vst3"
            ))
        );
        let gone = add("Synth").with("track", TrackId(9));
        assert_eq!(
            setup.run_with(&ADD_PLUGIN, gone),
            Err(CommandError::invalid("there's no track 9"))
        );
    }

    #[test]
    fn midi_effects_go_before_the_instrument_and_instruments_after_them() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let mut setup = Setup::new(project);
        let on_keys = |name, kind| add(name).with("kind", kind).with("track", keys);
        let mut added = |args| setup.run_with(&ADD_PLUGIN, args).unwrap();
        let synth = added(on_keys("Synth", "instrument"));
        let eq = added(on_keys("EQ", "audio_effect"));
        // At the end without an index, so before the synth.
        let arp = added(on_keys("Arp", "midi_effect"));
        // After the synth, as asked, is too late: just before it.
        let chord = added(on_keys("Chord", "midi_effect").with("index", 3_i64));
        // Before the arp is too early for a second synth: just after it.
        let pad = added(on_keys("Pad", "instrument").with("index", 0_i64));
        let ids = |setup: &Setup, track: usize| -> Vec<Value> {
            let devices = &setup.document.project().tracks[track].devices;
            devices.iter().map(|d| d.id.into()).collect()
        };
        assert_eq!(
            ids(&setup, 0),
            [&arp, &chord, &pad, &synth, &eq].map(Clone::clone)
        );
        let kind = setup.document.project().tracks[0].devices[0].kind.plugin();
        assert_eq!(kind.unwrap().kind, PluginKind::MidiEffect);

        let refused = Err(CommandError::invalid(
            "Arp is a MIDI effect, which goes on a MIDI track, before its instrument",
        ));
        let on_bass = add("Arp").with("kind", "midi_effect").with("track", bass);
        assert_eq!(setup.run_with(&ADD_PLUGIN, on_bass), refused);
        let on_master = add("Arp").with("kind", "midi_effect").with("master", true);
        assert_eq!(setup.run_with(&ADD_PLUGIN, on_master), refused);
        let to_bass = Args::new()
            .with("devices", vec![arp.clone()])
            .with("track", bass);
        assert_eq!(setup.run_with(&MOVE, to_bass), refused);
        let after_synth = Args::new()
            .with("devices", vec![chord.clone()])
            .with("index", 3_i64);
        assert_eq!(
            setup.run_with(&MOVE, after_synth),
            Err(CommandError::invalid(
                "Chord is a MIDI effect, which goes before Pad, the instrument that plays the \
                 notes it plays on"
            ))
        );
        assert_eq!(ids(&setup, 0)[..2], [arp.clone(), chord.clone()]);
        // A MIDI effect makes a MIDI track of its own.
        let alone = add("Arp")
            .with("kind", "midi_effect")
            .with("new_track", true);
        setup.run_with(&ADD_PLUGIN, alone).unwrap();
        let project = setup.document.project();
        assert_eq!(project.tracks[2].kind, TrackKind::Midi);

        // Copies fit where they can go, and a MIDI effect's is left out
        // where there are no notes.
        let copy = Args::new()
            .with("devices", vec![arp.clone(), eq.clone()])
            .with("track", keys)
            .with("index", 5_i64);
        let Ok(Value::List(copies)) = setup.run_with(&COPY, copy) else {
            panic!("the copies are listed");
        };
        assert_eq!(ids(&setup, 0)[2], copies[0]);
        assert_eq!(ids(&setup, 0).last(), copies.last());
        let copy = Args::new()
            .with("devices", vec![arp, eq])
            .with("track", bass);
        let Ok(Value::List(copies)) = setup.run_with(&COPY, copy) else {
            panic!("the copies are listed");
        };
        assert_eq!(copies.len(), 1);
        assert_eq!(ids(&setup, 1), copies);
    }

    #[test]
    fn a_device_added_is_shown_where_it_went() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let riff = project.new_clip("Riff", range, ClipContent::Midi(Default::default()));
        let riff_id = riff.id;
        project.track_mut(keys).unwrap().clips.push(riff);
        let mut setup = Setup::new(project);
        setup.select_clips(&[riff_id]);
        fn added(setup: &mut Setup, command: &CommandId, args: Args) -> DeviceId {
            match setup.run_with(command, args) {
                Ok(Value::Device(device)) => device,
                other => panic!("a device, not {other:?}"),
            }
        }
        let editor = |setup: &Setup| setup.document.editor().clone();

        // On the selected track, the selection stays as it was.
        let eq = added(&mut setup, &ADD, Args::new().with("kind", "eq"));
        let before = editor(&setup);
        assert_eq!(before.selected_track, Some(Chain::Track(keys)));
        assert_eq!(before.selection, Selection::clips([riff_id]));
        // Dropped on another track or the master, that's selected, as a
        // click on its header selects it, so the devices panel shows it.
        let gate = Args::new().with("kind", "gate").with("track", bass);
        let gate = added(&mut setup, &ADD, gate);
        let after = editor(&setup);
        assert_eq!(after.selected_track, Some(Chain::Track(bass)));
        assert_eq!(after.selection.tracks, BTreeSet::from([bass]));
        assert!(after.selection.clips.is_empty());
        let limiter = added(&mut setup, &ADD_PLUGIN, add("Limiter").with("master", true));
        let after = editor(&setup);
        assert_eq!(after.selected_track, Some(Chain::Master));
        assert!(after.selection.is_empty());
        let synth = Args::new().with("kind", "synth").with("new_track", true);
        let synth = added(&mut setup, &ADD, synth);
        let new = setup.document.project().tracks[2].id;
        assert_eq!(editor(&setup).selected_track, Some(Chain::Track(new)));
        // Each asks the app to show it, in front of the piano roll.
        let shown = [eq, gate, limiter, synth].map(Request::ShowDevice);
        assert_eq!(setup.requests, shown);
        // One that can't go on asks for nothing.
        let gone = Args::new().with("kind", "eq").with("track", TrackId(9));
        assert!(setup.run_with(&ADD, gone).is_err());
        assert_eq!(setup.requests, shown);
    }

    #[test]
    fn parameters_windows_and_deleting() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        project.master.devices.push(gain.clone());
        let mut setup = Setup::new(project);
        let synth = setup
            .run_with(&ADD_PLUGIN, add("Synth").with("track", keys))
            .unwrap();
        let Value::Device(device) = synth else {
            panic!("a device, not {synth:?}");
        };
        let set = |param: i64, value: Option<f64>| {
            let args = Args::new().with("device", device).with("param", param);
            match value {
                Some(value) => args.with("value", value),
                None => args,
            }
        };
        setup.run_with(&SET_PARAM, set(3, Some(0.75))).unwrap();
        setup.run_with(&SET_PARAM, set(1, Some(-2.0))).unwrap();
        setup.run_with(&SET_PARAM, set(3, None)).unwrap();
        assert_eq!(
            settings(&setup, device).params,
            [PluginParam { id: 1, value: -2.0 }]
        );
        assert_eq!(
            setup.run_with(&SET_PARAM, set(-1, Some(0.0))),
            Err(CommandError::invalid(
                "a parameter's id is from 0 to 4294967295"
            ))
        );
        let on_gain = Args::new().with("device", gain.id).with("param", 0_i64);
        assert_eq!(
            setup.run_with(&SET_PARAM, on_gain),
            Err(CommandError::invalid("device 0 has no parameters"))
        );

        let window = Args::new().with("device", device);
        setup.run_with(&SHOW_WINDOW, window.clone()).unwrap();
        setup.run_with(&HIDE_WINDOW, window.clone()).unwrap();
        assert_eq!(
            setup.requests,
            [
                Request::ShowDevice(device),
                Request::PluginWindow { device, show: true },
                Request::PluginWindow {
                    device,
                    show: false
                }
            ]
        );

        setup.run_with(&DELETE, window.clone()).unwrap();
        setup
            .run_with(&DELETE, Args::new().with("device", gain.id))
            .unwrap();
        assert_eq!(
            setup.run_with(&DELETE, window),
            Err(CommandError::invalid("there's no device 1"))
        );
        let project = setup.document.project();
        assert!(project.tracks[0].devices.is_empty() && project.master.devices.is_empty());
        assert_eq!(
            setup.committed_names(),
            [
                "Add Synth",
                "Change Synth",
                "Change Synth",
                "Change Synth",
                "Delete Synth",
                "Delete Device"
            ]
        );
        // Undo brings the synth back as it was set.
        assert!(setup.document.undo());
        assert!(setup.document.undo());
        assert_eq!(
            settings(&setup, device).params,
            [PluginParam { id: 1, value: -2.0 }]
        );
    }

    #[test]
    fn devices_are_bypassed_and_deactivated_as_steps_in_the_undo_history() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let eq = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
        let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let ids = (eq.id, gain.id);
        project.track_mut(vocal).unwrap().devices.push(eq);
        project.master.devices.push(gain);
        let (eq, gain) = ids;
        // A lane bypasses the EQ in the second bar.
        let points = vec![
            Point::new(Beats::new(4), 0.0),
            Point::new(Beats::new(4), 1.0),
        ];
        let lane = Lane::new(ParamAddress::Bypass(eq), points);
        project.automation.push(lane);
        let mut setup = Setup::new(project);
        let set = |devices: Vec<DeviceId>, bypassed: bool| {
            Args::new()
                .with("devices", devices)
                .with("bypassed", bypassed)
        };
        let state = |setup: &Setup, device| {
            let (_, _, found) = setup.document.project().find_device(device).unwrap();
            (found.bypassed, found.active)
        };

        setup.run_with(&SET_BYPASS, set(vec![eq], true)).unwrap();
        assert_eq!(state(&setup, eq), (true, true));
        // As if by hand, so the lane stops moving it.
        assert!(setup.document.project().automation[0].overridden);
        setup
            .run_with(&SET_BYPASS, set(vec![gain, eq, gain], true))
            .unwrap();
        setup.run_with(&SET_BYPASS, set(vec![eq], false)).unwrap();
        let active = |devices: Vec<DeviceId>, active: bool| {
            Args::new().with("devices", devices).with("active", active)
        };
        setup
            .run_with(&SET_ACTIVE, active(vec![eq], false))
            .unwrap();
        assert_eq!(state(&setup, eq), (false, false));
        setup
            .run_with(&SET_ACTIVE, active(vec![eq, gain], true))
            .unwrap();
        assert_eq!(
            setup.committed_names(),
            [
                "Bypass EQ",
                "Bypass 2 Devices",
                "Bring Back EQ",
                "Deactivate EQ",
                "Activate 2 Devices"
            ]
        );
        assert_eq!(state(&setup, gain), (true, true));
        assert!(setup.document.undo());
        assert_eq!(state(&setup, eq), (false, false));

        assert_eq!(
            setup.run_with(&SET_BYPASS, set(vec![eq, DeviceId(7)], true)),
            Err(CommandError::invalid("there's no device 7"))
        );
        assert_eq!(
            setup.run_with(&SET_ACTIVE, active(Vec::new(), true)),
            Err(CommandError::invalid("give at least one device"))
        );
        // Its Bypass lane goes with it.
        setup
            .run_with(&DELETE, Args::new().with("device", eq))
            .unwrap();
        assert!(setup.document.project().automation.is_empty());
        assert!(setup.document.undo());
        assert_eq!(setup.document.project().automation.len(), 1);
    }

    fn native(setup: &Setup, device: DeviceId) -> NativeDevice {
        let project = setup.document.project();
        let native = project.find_device(device).unwrap().2.kind.native();
        native.unwrap().clone()
    }

    #[test]
    fn built_in_devices_go_where_plugins_go() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        let kind = |key: &str| Args::new().with("kind", key);
        assert_eq!(
            setup.run_with(&ADD, kind("eq")),
            Err(CommandError::invalid(
                "select a track for the device to go on"
            ))
        );
        setup.select_tracks(&[keys]);
        let eq = setup.run_with(&ADD, kind("eq")).unwrap();
        let synth = setup
            .run_with(&ADD, kind("synth").with("index", 0_i64))
            .unwrap();
        let utility = setup
            .run_with(&ADD, kind("utility").with("master", true))
            .unwrap();
        assert_eq!(
            setup.run_with(&ADD, kind("Eq")),
            Err(CommandError::invalid(
                "there's no built-in device \"Eq\": it's eq, compressor, gate, delay, reverb, \
                 utility, sampler or synth"
            ))
        );

        let project = setup.document.project();
        let ids =
            |devices: &[Device]| -> Vec<Value> { devices.iter().map(|d| d.id.into()).collect() };
        assert_eq!(ids(&project.tracks[0].devices), [synth, eq]);
        assert_eq!(ids(&project.master.devices), [utility]);
        assert!(project.tracks[0].devices[0].kind.is_instrument());
        assert_eq!(
            project.master.devices[0].kind.native(),
            Some(&NativeDevice::new(NativeKind::Utility))
        );
        assert_eq!(
            setup.committed_names(),
            ["Add EQ", "Add Synth", "Add Utility"]
        );
    }

    #[test]
    fn built_in_parameters_stay_in_range_and_undo() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        let added = Args::new().with("kind", "eq").with("track", keys);
        let Value::Device(device) = setup.run_with(&ADD, added).unwrap() else {
            panic!("a device");
        };
        let set = |param: usize, value: f64| {
            Args::new()
                .with("device", device)
                .with("param", param as i64)
                .with("value", value)
        };
        // Band 3's frequency.
        let freq = 2 * eq::PER_BAND + eq::FREQ;
        setup.run_with(&SET_PARAM, set(freq, 1000.0)).unwrap();
        // Past the top, it stops at the top.
        setup.run_with(&SET_PARAM, set(eq::OUTPUT, 99.0)).unwrap();
        // Switches and choices are whole numbers.
        setup.run_with(&SET_PARAM, set(eq::ON, 0.8)).unwrap();
        let values = native(&setup, device).values;
        assert_eq!(values[freq], 1000.0);
        assert_eq!(values[eq::OUTPUT], 24.0);
        assert_eq!(values[eq::ON], 1.0);

        let no_value = Args::new().with("device", device).with("param", 3_i64);
        assert_eq!(
            setup.run_with(&SET_PARAM, no_value),
            Err(CommandError::invalid("give Band 1 Gain a value"))
        );
        assert_eq!(
            setup.run_with(&SET_PARAM, set(41, 0.0)),
            Err(CommandError::invalid(
                "the EQ has no parameter 41: they're 0 to 40"
            ))
        );

        setup
            .run_with(&DELETE, Args::new().with("device", device))
            .unwrap();
        assert_eq!(
            setup.committed_names(),
            ["Add EQ", "Change EQ", "Change EQ", "Change EQ", "Delete EQ"]
        );
        assert!(setup.document.undo());
        assert!(setup.document.undo());
        assert!(setup.document.undo());
        let values = native(&setup, device).values;
        assert_eq!(values[freq], 1000.0);
        assert_eq!(values[eq::OUTPUT], 0.0);
    }

    #[test]
    fn samplers_get_samples_from_files_the_app_asks_for() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        let mut add = |key: &str| {
            let added = Args::new().with("kind", key).with("track", keys);
            match setup.run_with(&ADD, added).unwrap() {
                Value::Device(device) => device,
                other => panic!("a device, not {other:?}"),
            }
        };
        let (sampler, synth) = (add("sampler"), add("synth"));
        let sample = |device: DeviceId, file: Option<&str>| {
            let args = Args::new().with("device", device);
            match file {
                Some(file) => args.with("file", file),
                None => args,
            }
        };
        let kick = sample(sampler, Some("Samples/kick.wav"));
        setup.run_with(&SET_SAMPLE, kick.clone()).unwrap();
        assert_eq!(
            native(&setup, sampler).sample.as_deref(),
            Some("Samples/kick.wav")
        );
        // The same file again changes nothing.
        setup.run_with(&SET_SAMPLE, kick).unwrap();
        setup.run_with(&SET_SAMPLE, sample(sampler, None)).unwrap();
        assert_eq!(native(&setup, sampler).sample, None);
        assert_eq!(
            setup.run_with(&SET_SAMPLE, sample(synth, Some("kick.wav"))),
            Err(CommandError::invalid("device 1 isn't a sampler"))
        );
        assert_eq!(
            setup.run_with(&SET_SAMPLE, sample(sampler, Some(""))),
            Err(CommandError::invalid("a sample's path can't be empty"))
        );
        assert_eq!(
            setup.run_with(&SET_SAMPLE, sample(DeviceId(7), None)),
            Err(CommandError::invalid("there's no device 7"))
        );
        assert_eq!(
            setup.committed_names(),
            ["Add Sampler", "Add Synth", "Load Sample", "Remove Sample"]
        );
        assert!(setup.document.undo());
        assert_eq!(
            native(&setup, sampler).sample.as_deref(),
            Some("Samples/kick.wav")
        );

        // Loading asks the app for the file. (Adding them asked it to show
        // them.)
        setup.requests.clear();
        setup.run_with(&LOAD_SAMPLE, sample(sampler, None)).unwrap();
        assert_eq!(
            setup.run_with(&LOAD_SAMPLE, sample(synth, None)),
            Err(CommandError::invalid("device 1 isn't a sampler"))
        );
        assert_eq!(setup.requests, [Request::LoadSample(sampler)]);
    }

    #[test]
    fn the_kinds_list_every_built_in_device() {
        let keys: Vec<_> = NativeKind::ALL.iter().map(|kind| kind.key()).collect();
        let (rest, last) = keys.split_at(keys.len() - 1);
        assert_eq!(KINDS, format!("{} or {}", rest.join(", "), last[0]));
    }

    fn synth() -> DeviceKind {
        DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.synth".to_owned(),
            name: "Synth".to_owned(),
            kind: PluginKind::Instrument,
            inputs: Vec::new(),
            settings: PluginSettings {
                state: Some(musicable_model::plugin::PluginState::new(vec![1, 2])),
                params: vec![PluginParam { id: 3, value: 0.5 }],
            },
        })
    }

    #[test]
    fn presets_add_the_devices_they_hold() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut setup = Setup::new(project);
        let warm = "/presets/Synth/Warm.musicable-preset";
        setup.library.presets.insert(warm.into(), synth());
        let args = Args::new().with("preset", warm).with("track", keys);
        let device = setup.run_with(&ADD_PRESET, args).unwrap();
        let added = &setup.document.project().tracks[0].devices[0];
        assert_eq!((Value::from(added.id), &added.kind), (device, &synth()));
        assert_eq!(setup.committed_names(), ["Add Warm"]);
        let missing = Args::new()
            .with("preset", "/presets/Missing.vstpreset")
            .with("track", keys);
        assert_eq!(
            setup.run_with(&ADD_PRESET, missing),
            Err(CommandError::invalid(
                "/presets/Missing.vstpreset isn't a preset"
            ))
        );
    }

    #[test]
    fn devices_save_as_presets_named_after_them() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let device = project.new_device(synth());
        let id = device.id;
        project.tracks[0].devices.push(device);
        let mut setup = Setup::new(project);
        let saved = setup.run_with(&SAVE_PRESET, Args::new().with("device", id));
        assert_eq!(
            saved,
            Ok(Value::Text(
                "/presets/Synth/Synth 1.musicable-preset".into()
            ))
        );
        let named = Args::new().with("device", id).with("name", " Bright ");
        assert_eq!(
            setup.run_with(&SAVE_PRESET, named),
            Ok(Value::Text("/presets/Synth/Bright.musicable-preset".into()))
        );
        let blank = Args::new().with("device", id).with("name", " ");
        assert_eq!(
            setup.run_with(&SAVE_PRESET, blank),
            Err(CommandError::invalid("a preset's name can't be empty"))
        );
        assert_eq!(
            setup.run_with(&SAVE_PRESET, Args::new().with("device", DeviceId(9))),
            Err(CommandError::invalid("there's no device 9"))
        );
        assert_eq!(
            setup.library.saved,
            [
                (synth(), "Synth".to_owned(), None),
                (synth(), "Synth".to_owned(), Some("Bright".to_owned()))
            ]
        );
        // Saving a preset doesn't change the project.
        assert_eq!(setup.committed_names(), Vec::<String>::new());
        assert_eq!(setup.document.project().tracks[0].id, keys);
    }

    #[test]
    fn devices_can_go_on_a_new_track_named_after_them() {
        let mut project = Project::new();
        project.add_track("Reverb", TrackKind::Return);
        let mut setup = Setup::new(project);
        let synth = add("Synth")
            .with("kind", "instrument")
            .with("new_track", true);
        let synth = setup.run_with(&ADD_PLUGIN, synth).unwrap();
        let eq = setup
            .run_with(&ADD_PLUGIN, add("EQ").with("new_track", true))
            .unwrap();
        let project = setup.document.project();
        let tracks: Vec<(&str, TrackKind, Value)> = project
            .tracks
            .iter()
            .map(|t| {
                (
                    t.name.as_str(),
                    t.kind,
                    t.devices.first().map_or(Value::None, |d| d.id.into()),
                )
            })
            .collect();
        assert_eq!(
            tracks,
            [
                ("Synth", TrackKind::Midi, synth),
                ("EQ", TrackKind::Audio, eq),
                ("Reverb", TrackKind::Return, Value::None)
            ]
        );
        // The new track is selected, and it came with its device, as one
        // change.
        let eq_track = project.tracks[1].id;
        let selected = setup.document.editor().selected_track;
        assert_eq!(selected, Some(Chain::Track(eq_track)));
        assert_eq!(setup.committed_names(), ["Add Synth", "Add EQ"]);
        assert!(setup.document.undo());
        assert_eq!(setup.document.project().tracks.len(), 2);
    }

    /// Puts one of Musicable's own devices at the end of `chain`.
    fn put(project: &mut Project, chain: Chain, kind: NativeKind) -> DeviceId {
        let device = project.new_device(DeviceKind::Native(NativeDevice::new(kind)));
        let id = device.id;
        project.devices_mut(chain).unwrap().push(device);
        id
    }

    /// The devices of `chain`, in order.
    fn ids(setup: &Setup, chain: Chain) -> Vec<DeviceId> {
        let devices = setup.document.project().devices(chain).unwrap();
        devices.iter().map(|device| device.id).collect()
    }

    /// The devices the devices panel has selected, in order.
    fn in_panel(setup: &Setup) -> Vec<DeviceId> {
        let editor = setup.document.editor();
        editor.selected_devices(setup.document.project())
    }

    #[test]
    fn devices_are_selected_in_one_chain_which_the_panel_shows() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let chain = Chain::Track(vocal);
        let kinds = [NativeKind::Eq, NativeKind::Compressor, NativeKind::Delay];
        let [eq, compressor, delay] = kinds.map(|kind| put(&mut project, chain, kind));
        let utility = put(&mut project, Chain::Master, NativeKind::Utility);
        let mut setup = Setup::new(project);
        let select = |devices: Vec<DeviceId>, toggle: bool| {
            Args::new().with("devices", devices).with("toggle", toggle)
        };

        let picked = select(vec![delay, eq], false);
        setup.run_with(&SELECT, picked).unwrap();
        // The panel shows their chain, with its track selected as a click
        // on its header selects it, and has the focus.
        let editor = setup.document.editor();
        assert_eq!(editor.selected_track, Some(chain));
        assert_eq!(editor.selection.tracks, BTreeSet::from([vocal]));
        assert_eq!(editor.focus, Focus::DEVICES);
        // In the order they play.
        assert_eq!(in_panel(&setup), [eq, delay]);
        let toggled = select(vec![delay, compressor], true);
        setup.run_with(&SELECT, toggled).unwrap();
        assert_eq!(in_panel(&setup), [eq, compressor]);
        assert_eq!(
            setup.run_with(&SELECT, select(vec![eq, utility], false)),
            Err(CommandError::invalid(
                "give devices of one chain: a track's, or the master's"
            ))
        );
        assert_eq!(
            setup.run_with(&SELECT, select(vec![DeviceId(9)], false)),
            Err(CommandError::invalid("there's no device 9"))
        );
        assert_eq!(in_panel(&setup), [eq, compressor]);
        // None: the chain stays.
        setup.run_with(&SELECT, select(Vec::new(), false)).unwrap();
        assert!(in_panel(&setup).is_empty());
        assert_eq!(setup.document.editor().selected_track, Some(chain));

        // The master's leaves no track selected.
        setup.select_devices(&[utility]);
        let editor = setup.document.editor();
        assert_eq!(editor.selected_track, Some(Chain::Master));
        assert!(editor.selection.is_empty());
        // Showing another chain forgets them, and a device that goes
        // leaves the selection.
        setup.select_tracks(&[vocal]);
        assert!(setup.document.editor().devices.selected.is_empty());
        setup.select_devices(&[eq, delay]);
        let gone = Args::new().with("device", delay);
        setup.run_with(&DELETE, gone).unwrap();
        assert_eq!(
            setup.document.editor().devices.selected,
            BTreeSet::from([eq])
        );
        // Selecting changes nothing to undo.
        assert_eq!(setup.committed_names(), ["Delete Delay"]);
    }

    #[test]
    fn moved_devices_keep_their_ids_and_lanes_in_one_step() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let (v, b) = (Chain::Track(vocal), Chain::Track(bass));
        let kinds = [
            NativeKind::Eq,
            NativeKind::Compressor,
            NativeKind::Delay,
            NativeKind::Reverb,
        ];
        let [eq, compressor, delay, reverb] = kinds.map(|kind| put(&mut project, v, kind));
        let gate = put(&mut project, b, NativeKind::Gate);
        // The EQ's output moves, and the delay is bypassed from the second
        // bar.
        let output = ParamAddress::Device(eq, eq::OUTPUT as u32);
        let points = vec![
            Point::new(Beats::ZERO, -6.0),
            Point::new(Beats::new(4), 0.0),
        ];
        project.automation.push(Lane::new(output, points));
        let bypass = vec![Point::new(Beats::new(4), 1.0)];
        (project.automation).push(Lane::new(ParamAddress::Bypass(delay), bypass));
        let before = project.clone();
        let mut setup = Setup::new(project);
        let moving = |devices: Vec<DeviceId>| Args::new().with("devices", devices);

        assert_eq!(
            setup.run_with(&MOVE, moving(vec![eq])),
            Err(CommandError::invalid(
                "select a track for the devices to go on"
            ))
        );
        // Where it is already: no step.
        let stays = moving(vec![eq]).with("track", vocal).with("index", 0_i64);
        setup.run_with(&MOVE, stays).unwrap();
        // Side by side, in the order they play, the first at the index in
        // the chain without them.
        let along = moving(vec![delay, eq]).with("track", vocal);
        setup.run_with(&MOVE, along.with("index", 1_i64)).unwrap();
        assert_eq!(ids(&setup, v), [compressor, eq, delay, reverb]);
        // They keep their ids, so their lanes go with them.
        assert_eq!(setup.document.project().automation, before.automation);
        // Past the end, at the end.
        let last = moving(vec![compressor]).with("index", 9_i64);
        setup.run_with(&MOVE, last.with("track", vocal)).unwrap();
        assert_eq!(ids(&setup, v), [eq, delay, reverb, compressor]);
        // Onto another track, which the devices panel then shows, with
        // them selected.
        let across = moving(vec![reverb])
            .with("track", bass)
            .with("index", 0_i64);
        setup.run_with(&MOVE, across).unwrap();
        assert_eq!(ids(&setup, b), [reverb, gate]);
        assert_eq!(setup.document.editor().selected_track, Some(b));
        assert_eq!(in_panel(&setup), [reverb]);
        // Onto the master, and onto a new track named after the first.
        setup
            .run_with(&MOVE, moving(vec![gate]).with("master", true))
            .unwrap();
        setup
            .run_with(&MOVE, moving(vec![delay, eq]).with("new_track", true))
            .unwrap();
        assert_eq!(ids(&setup, Chain::Master), [gate]);
        let project = setup.document.project();
        let new = &project.tracks[2];
        assert_eq!((new.name.as_str(), new.kind), ("EQ", TrackKind::Audio));
        assert_eq!(ids(&setup, Chain::Track(new.id)), [eq, delay]);
        assert_eq!(setup.document.project().automation, before.automation);
        assert_eq!(
            setup.committed_names(),
            [
                "Move 2 Devices",
                "Move Compressor",
                "Move Reverb",
                "Move Gate",
                "Move 2 Devices"
            ]
        );
        // Each shows them, the one that left the EQ where it was too.
        let shown = [eq, eq, compressor, reverb, gate, eq].map(Request::ShowDevice);
        assert_eq!(setup.requests, shown);
        // Each undoes in one step, back to where they were.
        for _ in 0..5 {
            assert!(setup.document.undo());
        }
        let project = setup.document.project();
        assert_eq!(project.tracks, before.tracks);
        assert_eq!(project.master, before.master);
        // An instrument's new track is a MIDI track.
        let synth = setup.run_with(&ADD, Args::new().with("kind", "synth").with("track", bass));
        let Ok(Value::Device(synth)) = synth else {
            panic!("a device, not {synth:?}");
        };
        let instruments = moving(vec![gate, synth]).with("new_track", true);
        setup.run_with(&MOVE, instruments).unwrap();
        let new = setup.document.project().tracks.last().unwrap();
        assert_eq!((new.name.as_str(), new.kind), ("Gate", TrackKind::Midi));

        assert_eq!(
            setup.run_with(&MOVE, moving(Vec::new()).with("master", true)),
            Err(CommandError::invalid("give at least one device"))
        );
        assert_eq!(
            setup.run_with(&MOVE, moving(vec![DeviceId(9)]).with("master", true)),
            Err(CommandError::invalid("there's no device 9"))
        );
        assert_eq!(
            setup.run_with(&MOVE, moving(vec![eq]).with("track", TrackId(9))),
            Err(CommandError::invalid("there's no track 9"))
        );
    }

    #[test]
    fn moving_left_or_right_moves_each_run_past_its_neighbour() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let v = Chain::Track(vocal);
        let kinds = [
            NativeKind::Eq,
            NativeKind::Compressor,
            NativeKind::Gate,
            NativeKind::Delay,
            NativeKind::Reverb,
        ];
        let [eq, compressor, gate, delay, reverb] = kinds.map(|kind| put(&mut project, v, kind));
        let mut setup = Setup::new(project);
        assert!(!setup.applies(&MOVE_LEFT) && !setup.applies(&MOVE_RIGHT));
        setup.select_devices(&[compressor, delay]);
        assert!(setup.applies(&MOVE_LEFT) && setup.applies(&MOVE_RIGHT));

        setup.run(&MOVE_LEFT);
        assert_eq!(ids(&setup, v), [compressor, eq, delay, gate, reverb]);
        // A run at the start stays, and the others go on.
        setup.run(&MOVE_LEFT);
        assert_eq!(ids(&setup, v), [compressor, delay, eq, gate, reverb]);
        // Side by side, they move as one run.
        setup.run(&MOVE_RIGHT);
        assert_eq!(ids(&setup, v), [eq, compressor, delay, gate, reverb]);
        assert_eq!(in_panel(&setup), [compressor, delay]);
        // At the end, nothing moves, and there's no step.
        setup.select_devices(&[reverb]);
        setup.run(&MOVE_RIGHT);
        assert_eq!(
            setup.committed_names(),
            ["Move 2 Devices", "Move Delay", "Move 2 Devices"]
        );
        assert!(setup.document.undo());
        assert_eq!(ids(&setup, v), [compressor, delay, eq, gate, reverb]);
    }

    #[test]
    fn copies_are_new_devices_with_lanes_of_their_own() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let (v, b) = (Chain::Track(vocal), Chain::Track(bass));
        let eq = put(&mut project, v, NativeKind::Eq);
        let plugin = project.new_device(synth());
        let synth = plugin.id;
        project.devices_mut(v).unwrap().push(plugin);
        let gate = put(&mut project, b, NativeKind::Gate);
        let found = project.device_mut(eq).unwrap();
        found.bypassed = true;
        if let DeviceKind::Native(native) = &mut found.kind {
            native.values[eq::OUTPUT] = -3.0;
        }
        let point = |at, value| Point::new(Beats::new(at), value);
        let output = ParamAddress::Device(eq, eq::OUTPUT as u32);
        project.automation = vec![
            Lane::new(output, vec![point(0, -6.0), point(4, 0.0)]),
            Lane::new(ParamAddress::Bypass(eq), vec![point(8, 0.0)]),
            Lane::new(ParamAddress::Device(synth, 3), vec![point(0, 0.25)]),
            // Not the devices', so no copy takes it.
            Lane::new(ParamAddress::Volume(v), vec![point(0, 0.5)]),
        ];
        let lanes = project.automation.clone();
        let mut setup = Setup::new(project);
        // A change made in the synth's window, which the project hasn't
        // got yet.
        let fresh = PluginState::new(vec![7, 7]);
        setup.library.states.insert(synth, fresh.clone());

        let copy = Args::new()
            .with("devices", vec![synth, eq])
            .with("track", bass)
            .with("index", 0_i64);
        let copies = setup.run_with(&COPY, copy).unwrap();
        let on_bass = ids(&setup, b);
        let [eq_copy, synth_copy] = [on_bass[0], on_bass[1]];
        assert_eq!(copies, Value::from(vec![eq_copy, synth_copy]));
        assert_eq!(on_bass[2], gate);
        // The originals stay as they were.
        assert_eq!(ids(&setup, v), [eq, synth]);
        let project = setup.document.project();
        let device = |id| project.find_device(id).unwrap().2.clone();
        assert_eq!(
            (device(eq_copy).kind, device(eq_copy).bypassed),
            (device(eq).kind, true)
        );
        let settings = |id| device(id).kind.plugin().unwrap().settings.clone();
        assert_eq!(settings(synth_copy).state, Some(fresh));
        assert_eq!(settings(synth_copy).params, settings(synth).params);
        assert_eq!(settings(synth).state, Some(PluginState::new(vec![1, 2])));
        let calls = &setup.library.calls;
        assert!(calls.contains(&format!("state {}", synth.0)));
        // Each copy has lanes of its own with its original's points.
        let copied: Vec<(ParamAddress, Vec<Point>)> = (project.automation[lanes.len()..].iter())
            .map(|lane| (lane.param, lane.points.clone()))
            .collect();
        let output = ParamAddress::Device(eq_copy, eq::OUTPUT as u32);
        assert_eq!(
            copied,
            [
                (output, lanes[0].points.clone()),
                (ParamAddress::Bypass(eq_copy), lanes[1].points.clone()),
                (ParamAddress::Device(synth_copy, 3), lanes[2].points.clone()),
            ]
        );
        assert_eq!(project.automation[..lanes.len()], lanes);
        // The panel shows them, selected.
        assert_eq!(setup.document.editor().selected_track, Some(b));
        assert_eq!(in_panel(&setup), [eq_copy, synth_copy]);

        // Without automation, as the setting has it.
        setup.document.editor_mut().behaviour.copies_take_automation = false;
        let copy = Args::new().with("devices", vec![eq]).with("master", true);
        let Ok(Value::List(copies)) = setup.run_with(&COPY, copy) else {
            panic!("a list of copies");
        };
        let [Value::Device(on_master)] = copies[..] else {
            panic!("one copy, not {copies:?}");
        };
        let project = setup.document.project();
        let its_own = |lane: &Lane| lane.param.device() == Some(on_master);
        assert!(!project.automation.iter().any(its_own));
        assert_eq!(setup.committed_names(), ["Copy 2 Devices", "Copy EQ"]);
        // Undo takes copies and their lanes away together.
        assert!(setup.document.undo());
        assert!(setup.document.undo());
        let project = setup.document.project();
        assert_eq!(project.automation, lanes);
        assert_eq!(ids(&setup, b), [gate]);
    }

    #[test]
    fn the_edit_commands_take_the_selected_devices_while_the_panel_has_the_focus() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let v = Chain::Track(vocal);
        let kinds = [NativeKind::Eq, NativeKind::Compressor, NativeKind::Delay];
        let [eq, compressor, delay] = kinds.map(|kind| put(&mut project, v, kind));
        let utility = put(&mut project, Chain::Master, NativeKind::Utility);
        let bypass = vec![Point::new(Beats::new(4), 1.0)];
        (project.automation).push(Lane::new(ParamAddress::Bypass(delay), bypass));
        let mut setup = Setup::new(project);
        let takers = [edit::CUT, edit::COPY, edit::DUPLICATE, edit::DELETE];
        assert!(!takers.iter().any(|id| setup.applies(id)));
        setup.select_devices(&[delay, eq]);
        assert!(takers.iter().all(|id| setup.applies(id)));
        assert!(setup.applies(&edit::SELECT_ALL));
        assert!(!setup.applies(&edit::PASTE));

        setup.run(&edit::COPY);
        assert!(setup.applies(&edit::PASTE));
        // Duplicate puts copies just after the last, and selects them, so
        // doing it again makes more.
        setup.run(&edit::DUPLICATE);
        let after = ids(&setup, v);
        assert_eq!(after[..3], [eq, compressor, delay]);
        let (eq_2, delay_2) = (after[3], after[4]);
        assert_eq!(in_panel(&setup), [eq_2, delay_2]);
        let project = setup.document.project();
        assert!(project.lane(ParamAddress::Bypass(delay_2)).is_some());
        // Paste puts them in front of the first selected device.
        setup.select_devices(&[compressor]);
        setup.run(&edit::PASTE);
        let after = ids(&setup, v);
        assert_eq!([after[0], after[3]], [eq, compressor]);
        assert_eq!(in_panel(&setup), after[1..3]);
        // With nothing selected, at the end, on the master too.
        setup.select_devices(&[utility]);
        setup.select_devices(&[]);
        setup.run(&edit::PASTE);
        let on_master = ids(&setup, Chain::Master);
        assert_eq!((on_master.len(), on_master[0]), (3, utility));
        assert_eq!(in_panel(&setup), on_master[1..]);
        // Cut takes them, then deletes them.
        setup.select_devices(&[compressor]);
        setup.run(&edit::CUT);
        assert!(setup.document.project().find_device(compressor).is_none());
        let clipboard = setup.document.editor().devices.clipboard.clone().unwrap();
        assert_eq!(clipboard.devices.len(), 1);
        assert_eq!(clipboard.devices[0].device.id, compressor);
        // Delete deletes them all, as one step.
        setup.select_devices(&[eq, delay]);
        setup.run(&edit::DELETE);
        let project = setup.document.project();
        assert!(project.lane(ParamAddress::Bypass(delay)).is_none());
        // Select All selects the whole chain.
        setup.run(&edit::SELECT_ALL);
        assert_eq!(in_panel(&setup), ids(&setup, v));
        assert_eq!(
            setup.committed_names(),
            [
                "Duplicate 2 Devices",
                "Paste 2 Devices",
                "Paste 2 Devices",
                "Cut Compressor",
                "Delete 2 Devices"
            ]
        );
        assert!(setup.document.undo());
        assert_eq!(ids(&setup, v)[..2], [eq, after[1]]);
        // While the arrangement has the focus, they're the arrangement's.
        setup.select_tracks(&[vocal]);
        assert!(!setup.applies(&edit::COPY) && !setup.applies(&edit::DUPLICATE));
    }

    #[test]
    fn the_selected_devices_are_bypassed_or_all_brought_back() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let v = Chain::Track(vocal);
        let [eq, compressor] =
            [NativeKind::Eq, NativeKind::Compressor].map(|k| put(&mut project, v, k));
        project.device_mut(eq).unwrap().bypassed = true;
        let bypass = vec![Point::new(Beats::new(4), 1.0)];
        (project.automation).push(Lane::new(ParamAddress::Bypass(compressor), bypass));
        let mut setup = Setup::new(project);
        assert!(!setup.applies(&TOGGLE_BYPASS));
        setup.select_devices(&[eq, compressor]);
        let bypassed = |setup: &Setup| {
            let project = setup.document.project();
            [eq, compressor].map(|id| project.find_device(id).unwrap().2.bypassed)
        };
        // Not every one is, so all are.
        setup.run(&TOGGLE_BYPASS);
        assert_eq!(bypassed(&setup), [true, true]);
        // As if by hand: the lane stops moving it.
        assert!(setup.document.project().automation[0].overridden);
        setup.run(&TOGGLE_BYPASS);
        assert_eq!(bypassed(&setup), [false, false]);
        assert_eq!(
            setup.committed_names(),
            ["Bypass 2 Devices", "Bring Back 2 Devices"]
        );
        // Wherever the focus is, as from the command palette.
        setup.document.editor_mut().focus = Focus::ARRANGEMENT;
        assert!(setup.applies(&TOGGLE_BYPASS));
    }
}
