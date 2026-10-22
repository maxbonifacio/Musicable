//! The device commands: adding Musicable's own devices and plugins to a
//! track's devices or the master's, setting their parameters, giving
//! samplers their samples, showing and closing plugins' windows, and
//! deleting devices.

use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::{Chain, Device, DeviceId, DeviceKind, PluginFormat, TrackId};

use crate::{
    Args, Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value,
};

/// Adds one of Musicable's own devices to a track's devices, or the
/// master's.
pub const ADD: CommandId = CommandId::new("device.add");
/// Adds a plugin to a track's devices, or the master's.
pub const ADD_PLUGIN: CommandId = CommandId::new("device.add_plugin");
/// Sets a device's parameter, or leaves a plugin's to its state.
pub const SET_PARAM: CommandId = CommandId::new("device.set_param");
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
        ]),
        |ctx, args| {
            let key: String = args.get("kind")?;
            let Some(kind) = NativeKind::from_key(&key) else {
                return Err(CommandError::invalid(format!(
                    "there's no built-in device {key:?}: it's {KINDS}"
                )));
            };
            let native = DeviceKind::Native(NativeDevice::new(kind));
            add(ctx, args, kind.name(), native)
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
            let name = plugin_name(&plugin).to_owned();
            add(ctx, args, &name, DeviceKind::Plugin(plugin))
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
            let name = format!("Change {}", device_name(found));
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
        Command::new(LOAD_SAMPLE, "Load Sample...").with_params(&[DEVICE]),
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
            let name = format!("Delete {}", device_name(find(ctx, device)?));
            ctx.document.transact(&name, |tx| {
                tx.remove_device(device);
            });
            Ok(Value::None)
        },
    );
}

/// Adds the device `kind`, called `name`, where `args` say, as the
/// transaction "Add `name`". Returns its id.
fn add(
    ctx: &mut Context<'_>,
    args: &Args,
    name: &str,
    kind: DeviceKind,
) -> Result<Value, CommandError> {
    let chain = chain(
        ctx,
        args.opt("track")?,
        args.opt("master")?.unwrap_or(false),
        if matches!(kind, DeviceKind::Plugin(_)) {
            "plugin"
        } else {
            "device"
        },
    )?;
    let index = args
        .opt::<i64>("index")?
        .map_or(usize::MAX, |index| usize::try_from(index).unwrap_or(0));
    let device = ctx.document.transact(&format!("Add {name}"), |tx| {
        let device = tx.new_device(kind);
        let id = device.id;
        tx.add_device(chain, index, device);
        id
    });
    Ok(device.into())
}

/// The chain a new device goes in: the master's with `master`, else
/// `track`'s, else the selected track's. `what` names the device in the
/// error without a track.
fn chain(
    ctx: &Context<'_>,
    track: Option<TrackId>,
    master: bool,
    what: &str,
) -> Result<Chain, CommandError> {
    if master {
        return Ok(Chain::Master);
    }
    let Some(track) = track.or(ctx.document.editor().selected_track) else {
        return Err(CommandError::invalid(format!(
            "select a track for the {what} to go on"
        )));
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

/// What the Edit menu calls a device: a built-in device by its kind, such
/// as "EQ", and a plugin by its name.
fn device_name(device: &Device) -> &str {
    match device.kind {
        DeviceKind::Native(ref native) => native.kind.name(),
        DeviceKind::Plugin(ref plugin) => plugin_name(plugin),
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
    use musicable_model::native::eq;
    use musicable_model::plugin::PluginParam;
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

        // Loading asks the app for the file.
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
}
