//! The device commands: adding Musicable's own devices, plugins and presets
//! to a track's devices or the master's, setting their parameters,
//! bypassing and deactivating them, giving samplers their samples, showing
//! and closing plugins' windows, saving devices as presets, and deleting
//! devices.

use std::path::Path;

use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::{Chain, Device, DeviceId, DeviceKind, PluginFormat, TrackId, TrackKind};

use crate::editor::Selection;
use crate::{
    Args, Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value,
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

/// The keys of Musicable's own devices, as `device.add` takes them.
const KINDS: &str = "eq, compressor, gate, delay, reverb, utility, sampler or synth";

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
            Param::required("format", Type::Text, "The plugin's format: clap or vst3."),
            Param::required(
                "plugin",
                Type::Text,
                "The plugin's id in its format, such as org.surge-synth-team.surge-xt.",
            ),
            Param::required("name", Type::Text, "Its name, to show while it's missing."),
            Param::optional(
                "instrument",
                Type::Bool,
                "Whether it plays notes: not without it.",
            ),
            TRACK,
            MASTER,
            INDEX,
            NEW_TRACK,
        ]),
        |ctx, args| {
            let format = match args.get::<String>("format")?.as_str() {
                "clap" => PluginFormat::Clap,
                "vst3" => PluginFormat::Vst3,
                other => {
                    return Err(CommandError::invalid(format!(
                        "there's no plugin format {other:?}: it's clap or vst3"
                    )));
                }
            };
            let id: String = args.get("plugin")?;
            if id.is_empty() {
                return Err(CommandError::invalid("a plugin's id can't be empty"));
            }
            let plugin = PluginDevice {
                format,
                id,
                name: args.get("name")?,
                instrument: args.opt("instrument")?.unwrap_or(false),
                settings: PluginSettings::default(),
            };
            let name = format!("Add {}", plugin_name(&plugin));
            add(ctx, args, DeviceKind::Plugin(plugin), &name)
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
            let bypassed = args.get("bypassed")?;
            let verb = if bypassed { "Bypass" } else { "Bring Back" };
            let name = switch_name(ctx, &devices, verb);
            ctx.document.transact(&name, |tx| {
                for &device in &devices {
                    // As if by hand: its lane stops moving it.
                    tx.override_automation(ParamAddress::Bypass(device));
                    tx.set_device_bypass(device, bypassed);
                }
            });
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
            let name = switch_name(ctx, &devices, verb);
            ctx.document.transact(&name, |tx| {
                for &device in &devices {
                    tx.set_device_active(device, active);
                }
            });
            Ok(Value::None)
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
    let index = args
        .opt::<i64>("index")?
        .map_or(usize::MAX, |index| usize::try_from(index).unwrap_or(0));
    let what = match kind {
        DeviceKind::Plugin(_) => "plugin",
        _ => "device",
    };
    let chain = match args.opt("new_track")?.unwrap_or(false) {
        true => None,
        false => Some(chain(
            ctx,
            args.opt("track")?,
            args.opt("master")?.unwrap_or(false),
            what,
        )?),
    };
    let track = match kind.is_instrument() {
        true => TrackKind::Midi,
        false => TrackKind::Audio,
    };
    let track_name = device_name(&kind).to_owned();
    let (device, chain) = ctx.document.transact(name, |tx| {
        let chain = match chain {
            Some(chain) => chain,
            None => Chain::Track(crate::track::add_track(tx, track_name, track)),
        };
        let device = tx.new_device(kind);
        let id = device.id;
        tx.add_device(chain, index, device);
        (id, chain)
    });
    let editor = ctx.document.editor_mut();
    if editor.selected_track != Some(chain) {
        // As a click on its header selects it.
        editor.selection = Selection {
            tracks: chain.track().into_iter().collect(),
            ..Selection::default()
        };
        editor.selected_track = Some(chain);
    }
    ctx.requests.push(Request::ShowDevice(device));
    Ok(device.into())
}

/// The chain a new device goes in: the master's with `master`, else
/// `track`'s, else the selected track's, which can be the master's. `what`
/// names the device in the error without a track.
fn chain(
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

/// The devices `args` give, each once, if the project has every one.
fn devices(ctx: &Context<'_>, args: &Args) -> Result<Vec<DeviceId>, CommandError> {
    let mut devices: Vec<DeviceId> = args.get("devices")?;
    if devices.is_empty() {
        return Err(CommandError::invalid("give at least one device"));
    }
    devices.sort_unstable();
    devices.dedup();
    for &device in &devices {
        find(ctx, device)?;
    }
    Ok(devices)
}

/// What the Edit menu calls `verb` done to `devices`: "Bypass EQ" for one,
/// and "Bypass 3 Devices" for several.
fn switch_name(ctx: &Context<'_>, devices: &[DeviceId], verb: &str) -> String {
    let project = ctx.document.project();
    match devices {
        [device] => match project.find_device(*device) {
            Some((_, _, found)) => format!("{verb} {}", device_name(&found.kind)),
            None => format!("{verb} Device"),
        },
        _ => format!("{verb} {} Devices", devices.len()),
    }
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
    match kind {
        DeviceKind::Native(native) => native.kind.name(),
        DeviceKind::Plugin(plugin) => plugin_name(plugin),
        _ => "Device",
    }
}

/// What the Edit menu calls a plugin.
fn plugin_name(plugin: &PluginDevice) -> &str {
    match plugin.name.trim() {
        "" => "Plugin",
        name => name,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use musicable_model::automation::{Lane, Point};
    use musicable_model::clip::ClipContent;
    use musicable_model::native::eq;
    use musicable_model::plugin::PluginParam;
    use musicable_model::time::{BeatRange, Beats};
    use musicable_model::{Project, TrackKind};

    use super::*;
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
            .run_with(&ADD_PLUGIN, add("Synth").with("instrument", true))
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
        assert!(synth.instrument);
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
        setup.run_with(&SET_BYPASS, set(vec![gain, eq, gain], true)).unwrap();
        setup.run_with(&SET_BYPASS, set(vec![eq], false)).unwrap();
        let active = |devices: Vec<DeviceId>, active: bool| {
            Args::new().with("devices", devices).with("active", active)
        };
        setup.run_with(&SET_ACTIVE, active(vec![eq], false)).unwrap();
        assert_eq!(state(&setup, eq), (false, false));
        setup.run_with(&SET_ACTIVE, active(vec![eq, gain], true)).unwrap();
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
            instrument: true,
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
            .with("instrument", true)
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
}
