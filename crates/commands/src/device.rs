//! The device commands: adding plugins to a track's devices or the
//! master's, setting their parameters, showing and closing their windows,
//! and deleting devices.

use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::{Chain, DeviceId, DeviceKind, PluginFormat, TrackId};

use crate::{Command, CommandError, CommandId, Context, Param, Registry, Request, Type, Value};

/// Adds a plugin to a track's devices, or the master's.
pub const ADD_PLUGIN: CommandId = CommandId::new("device.add_plugin");
/// Sets a plugin's parameter, or leaves it to the plugin's state.
pub const SET_PARAM: CommandId = CommandId::new("device.set_param");
/// Shows a plugin's own window.
pub const SHOW_WINDOW: CommandId = CommandId::new("device.show_window");
/// Closes a plugin's own window.
pub const HIDE_WINDOW: CommandId = CommandId::new("device.hide_window");
/// Deletes a device.
pub const DELETE: CommandId = CommandId::new("device.delete");

const DEVICE: Param = Param::required("device", Type::Device, "The device.");

/// Registers the device commands.
pub fn register(registry: &mut Registry) {
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
            Param::optional(
                "track",
                Type::Track,
                "The track it goes on: the selected track without it.",
            ),
            Param::optional("master", Type::Bool, "Puts it on the master instead."),
            Param::optional(
                "index",
                Type::Int,
                "Where it goes in the chain: the end without it.",
            ),
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
            let chain = chain(
                ctx,
                args.opt("track")?,
                args.opt("master")?.unwrap_or(false),
            )?;
            let index = args
                .opt::<i64>("index")?
                .map_or(usize::MAX, |index| usize::try_from(index).unwrap_or(0));
            let plugin = PluginDevice {
                format,
                id,
                name: args.get("name")?,
                instrument: args.opt("instrument")?.unwrap_or(false),
                settings: PluginSettings::default(),
            };
            let name = format!("Add {}", plugin_name(&plugin));
            let device = ctx.document.transact(&name, |tx| {
                let device = tx.new_device(DeviceKind::Plugin(plugin));
                let id = device.id;
                tx.add_device(chain, index, device);
                id
            });
            Ok(device.into())
        },
    );
    registry.add(
        Command::new(SET_PARAM, "Set Plugin Parameter").with_params(&[
            DEVICE,
            Param::required("param", Type::Int, "The plugin's id for the parameter."),
            Param::optional(
                "value",
                Type::Number,
                "Its value, in the plugin's own units: the value in the plugin's state \
                 without it.",
            ),
        ]),
        |ctx, args| {
            let device = args.get("device")?;
            let param = u32::try_from(args.get::<i64>("param")?)
                .map_err(|_| CommandError::invalid("a parameter's id is from 0 to 4294967295"))?;
            let value = args.opt("value")?;
            let name = format!("Change {}", plugin_name(plugin(ctx, device)?));
            ctx.document.transact(&name, |tx| {
                // As if by hand: automation stops moving it.
                tx.override_automation(ParamAddress::Device(device, param));
                tx.set_plugin_param(device, param, value);
            });
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
            let Some((_, _, found)) = ctx.document.project().find_device(device) else {
                return Err(no_device(device));
            };
            let name = match found.kind.plugin() {
                Some(plugin) => format!("Delete {}", plugin_name(plugin)),
                None => "Delete Device".to_owned(),
            };
            ctx.document.transact(&name, |tx| {
                tx.remove_device(device);
            });
            Ok(Value::None)
        },
    );
}

/// The chain a new device goes in: the master's with `master`, else
/// `track`'s, else the selected track's.
fn chain(ctx: &Context<'_>, track: Option<TrackId>, master: bool) -> Result<Chain, CommandError> {
    if master {
        return Ok(Chain::Master);
    }
    let Some(track) = track.or(ctx.document.editor().selected_track) else {
        return Err(CommandError::invalid(
            "select a track for the plugin to go on",
        ));
    };
    match ctx.document.project().track(track) {
        Some(_) => Ok(Chain::Track(track)),
        None => Err(CommandError::invalid(format!(
            "there's no track {}",
            track.0
        ))),
    }
}

/// The plugin device with id `device`, if the project has it.
fn plugin<'a>(ctx: &'a Context<'_>, device: DeviceId) -> Result<&'a PluginDevice, CommandError> {
    let Some((_, _, found)) = ctx.document.project().find_device(device) else {
        return Err(no_device(device));
    };
    found
        .kind
        .plugin()
        .ok_or_else(|| CommandError::invalid(format!("device {} isn't a plugin", device.0)))
}

fn no_device(device: DeviceId) -> CommandError {
    CommandError::invalid(format!("there's no device {}", device.0))
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
    use musicable_model::plugin::PluginParam;
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::Args;
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
            Err(CommandError::invalid("device 0 isn't a plugin"))
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
}
