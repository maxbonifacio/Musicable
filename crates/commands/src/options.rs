//! The options commands: the Settings window, with commands for some of
//! its pages, switching profiles, scanning for plugins, and changing
//! settings, the audio device and Render Ahead among them.
//!
//! Commands can't show a window, or reach the user's profiles, which are
//! the app's, so these ask the app, with a [`Request`]. Agents can't see the
//! Settings window, so the commands that show it are the user's.

use crate::{AudioDevice, Command, CommandError, CommandId, Param, Registry, Request, Type, Value};

/// Shows the Settings window, at the page it showed last.
pub const SETTINGS: CommandId = CommandId::new("options.settings");
/// Shows the Settings window's Audio page: the device, its mode and the
/// buffer size.
pub const AUDIO_SETTINGS: CommandId = CommandId::new("options.audio_settings");
/// Shows the Settings window's Keys page, where every command's keys can
/// be changed.
pub const KEYBOARD_SHORTCUTS: CommandId = CommandId::new("options.keyboard_shortcuts");
/// Shows the Settings window's Appearance page, where the theme is picked.
pub const APPEARANCE: CommandId = CommandId::new("options.appearance");
/// Shows the Settings window's Profiles page, where profiles are made and
/// switched between.
pub const PROFILES: CommandId = CommandId::new("options.profiles");
/// Shows the Settings window's Agents page, which lets agents such as
/// Claude Code control Musicable.
pub const AGENTS: CommandId = CommandId::new("options.agents");
/// Switches to the next of the user's profiles, in the order the Profiles
/// page lists them, and from the last back to the first: its keys, mouse
/// map, theme, workspaces and behaviour, all at once.
pub const NEXT_PROFILE: CommandId = CommandId::new("options.next_profile");
/// Scans for plugins again, loading the libraries that were left out as bad
/// too.
pub const RESCAN_PLUGINS: CommandId = CommandId::new("options.rescan_plugins");

/// Changes a setting, as the Settings window does, by its name in the
/// settings file, such as `audio.render_ahead` or `behaviour.play_from`.
pub const SET: CommandId = CommandId::new("options.set");
/// Sets how far ahead of the audio device the engine renders: Off, Short,
/// Normal or Long.
pub const SET_RENDER_AHEAD: CommandId = CommandId::new("options.set_render_ahead");
/// Plays through another audio device, or through the same one with
/// another buffer size, sample rate or mode.
pub const SET_AUDIO_DEVICE: CommandId = CommandId::new("options.set_audio_device");

/// Render Ahead's settings, as the settings file names them.
pub const RENDER_AHEAD: &[&str] = &["off", "short", "normal", "long"];
/// The driver types, as [`SET_AUDIO_DEVICE`] takes them: the system's own
/// audio, WASAPI on Windows, or an ASIO driver.
pub const DRIVER_TYPES: &[&str] = &["system", "asio"];

/// Every options command, with its title and what it asks for.
const COMMANDS: [(CommandId, &str, Request); 8] = [
    (SETTINGS, "Settings...", Request::Settings),
    (AUDIO_SETTINGS, "Audio Settings...", Request::AudioSettings),
    (
        KEYBOARD_SHORTCUTS,
        "Keyboard Shortcuts...",
        Request::KeyboardShortcuts,
    ),
    (APPEARANCE, "Appearance...", Request::Appearance),
    (PROFILES, "Profiles...", Request::Profiles),
    (AGENTS, "Agents...", Request::Agents),
    (NEXT_PROFILE, "Switch to Next Profile", Request::NextProfile),
    (RESCAN_PLUGINS, "Rescan Plugins", Request::RescanPlugins),
];

/// Registers the options commands.
pub fn register(registry: &mut Registry) {
    crate::register_requests(registry, &COMMANDS);
    registry.add(
        Command::new(SET, "Change Setting")
            .with_params(&[
                Param::required(
                    "key",
                    Type::Text,
                    "The setting, as the settings file names it: its section and its name, \
                     such as audio.render_ahead, recording.capture_minutes, analysis.fast, \
                     behaviour.play_from or export.bits.",
                ),
                Param::required(
                    "value",
                    Type::Text,
                    "What to make it, as text, as the settings file has it without quotes, \
                     such as long, true or 10, or none to clear one that can be empty.",
                ),
            ])
            .alone(),
        |ctx, args| {
            let key: String = args.get("key")?;
            if key.trim().is_empty() {
                return Err(CommandError::invalid("the setting needs a name"));
            }
            ctx.requests.push(Request::SetSetting {
                key,
                value: args.get("value")?,
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_RENDER_AHEAD, "Set Render Ahead")
            .with_params(&[Param::required(
                "render_ahead",
                Type::Choice(RENDER_AHEAD),
                "How far ahead of the audio device the engine renders all but what plays \
                 live: off, short (about 20 ms), normal (about 50 ms) or long (about 200 ms).",
            )])
            .alone(),
        |ctx, args| {
            ctx.requests.push(Request::SetSetting {
                key: "audio.render_ahead".into(),
                value: args.get("render_ahead")?,
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_AUDIO_DEVICE, "Set Audio Device")
            .with_params(&[
                Param::optional(
                    "driver_type",
                    Type::Choice(DRIVER_TYPES),
                    "system, the system's own audio (WASAPI on Windows), or asio. Without a \
                     device, a new driver type plays through the system's default output, or \
                     the first ASIO driver.",
                ),
                Param::optional(
                    "device",
                    Type::Text,
                    "The device's id or name, as list_audio_devices gives them.",
                ),
                Param::optional(
                    "buffer",
                    Type::Int,
                    "Frames per buffer, such as 64. Drivers round it, and an ASIO driver \
                     takes its own from its settings.",
                ),
                Param::optional("sample_rate", Type::Int, "The sample rate, such as 48000."),
                Param::optional(
                    "exclusive",
                    Type::Bool,
                    "Whether to take the device for Musicable alone, in WASAPI's exclusive \
                     mode.",
                ),
            ])
            .with_some_arguments()
            .alone(),
        |ctx, args| {
            let positive = |name: &str| -> Result<Option<u32>, CommandError> {
                match args.opt::<i64>(name)? {
                    None => Ok(None),
                    Some(n) => (u32::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .map(Some)
                        .ok_or_else(|| CommandError::invalid(format!("{name} must be over 0"))),
                }
            };
            let device = AudioDevice {
                driver_type: args.opt("driver_type")?,
                device: args.opt("device")?,
                buffer: positive("buffer")?,
                sample_rate: positive("sample_rate")?,
                exclusive: args.opt("exclusive")?,
            };
            if device == AudioDevice::default() {
                return Err(CommandError::invalid(
                    "say what to change: driver_type, device, buffer, sample_rate or exclusive",
                ));
            }
            ctx.requests.push(Request::SetAudioDevice(device));
            Ok(Value::None)
        },
    );
}

#[cfg(test)]
mod tests {
    use musicable_model::Project;

    use super::*;
    use crate::testing::StubTransport;
    use crate::{Context, Document, Keymap, view};

    #[test]
    fn the_windows_are_commands_with_keys() {
        let mut registry = Registry::new();
        register(&mut registry);
        view::register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        let ids = [
            SETTINGS,
            AUDIO_SETTINGS,
            KEYBOARD_SHORTCUTS,
            APPEARANCE,
            PROFILES,
            AGENTS,
            NEXT_PROFILE,
            RESCAN_PLUGINS,
            view::COMMAND_PALETTE,
        ];
        for id in &ids {
            let mut ctx = Context {
                document: &mut document,
                transport: &mut StubTransport::default(),
                requests: &mut requests,
                library: &mut crate::NoLibrary,
            };
            registry.run(id, &mut ctx).unwrap();
        }
        assert_eq!(
            requests,
            [
                Request::Settings,
                Request::AudioSettings,
                Request::KeyboardShortcuts,
                Request::Appearance,
                Request::Profiles,
                Request::Agents,
                Request::NextProfile,
                Request::RescanPlugins,
                Request::CommandPalette
            ]
        );
        assert_eq!(document.take_committed(), []);
        // Agents can't see the windows, but can switch profiles and rescan.
        let for_agents: Vec<_> = (ids.iter())
            .filter(|&id| registry.get(id).unwrap().agents)
            .collect();
        assert_eq!(for_agents, [&NEXT_PROFILE, &RESCAN_PLUGINS]);

        // Live's keys for its settings and its key map mode, and the usual
        // key for a command palette, which Live doesn't have.
        let keymap = Keymap::default_keymap();
        let chords =
            |id| -> Vec<String> { keymap.chords_for(&id).map(|c| c.to_string()).collect() };
        assert_eq!(chords(SETTINGS), ["Ctrl+Comma"]);
        assert!(chords(AUDIO_SETTINGS).is_empty());
        assert_eq!(chords(KEYBOARD_SHORTCUTS), ["Ctrl+K"]);
        assert_eq!(chords(view::COMMAND_PALETTE), ["Ctrl+Shift+P"]);
    }

    #[test]
    fn settings_change_through_the_app() {
        let mut registry = Registry::new();
        register(&mut registry);
        let mut document = Document::new(Project::new());
        let mut requests = Vec::new();
        let mut run = |id: &CommandId, args: crate::Args| {
            let mut ctx = Context {
                document: &mut document,
                transport: &mut StubTransport::default(),
                requests: &mut requests,
                library: &mut crate::NoLibrary,
            };
            registry.run_with(id, &args, &mut ctx)
        };
        let args = crate::Args::new;
        run(&SET_RENDER_AHEAD, args().with("render_ahead", "short")).unwrap();
        let capture = args()
            .with("key", "recording.capture_minutes")
            .with("value", "10");
        run(&SET, capture).unwrap();
        let asio = args().with("driver_type", "asio").with("buffer", 64_i64);
        run(&SET_AUDIO_DEVICE, asio).unwrap();
        // Refused, they ask for nothing.
        assert!(run(&SET_RENDER_AHEAD, args().with("render_ahead", "huge")).is_err());
        assert!(run(&SET_AUDIO_DEVICE, args()).is_err());
        assert!(run(&SET_AUDIO_DEVICE, args().with("buffer", 0_i64)).is_err());
        let unnamed = args().with("key", " ").with("value", "1");
        assert!(run(&SET, unnamed).is_err());
        assert_eq!(
            requests,
            [
                Request::SetSetting {
                    key: "audio.render_ahead".into(),
                    value: "short".into(),
                },
                Request::SetSetting {
                    key: "recording.capture_minutes".into(),
                    value: "10".into(),
                },
                Request::SetAudioDevice(AudioDevice {
                    driver_type: Some("asio".into()),
                    buffer: Some(64),
                    ..AudioDevice::default()
                }),
            ]
        );
        assert_eq!(document.take_committed(), []);
        // They need their arguments, so they're for agents and scripts,
        // not menus and keys, and run on their own, as the app answers them.
        for id in [SET, SET_RENDER_AHEAD, SET_AUDIO_DEVICE] {
            let command = registry.get(&id).unwrap();
            assert!(command.needs_arguments() && command.agents && command.alone);
        }
    }
}
