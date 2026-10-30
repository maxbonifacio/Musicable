//! The wire commands: routing a tap's signal into a port, such as a kick's
//! into the side input of the Compressor on the bass, so the kick ducks it,
//! or a MIDI track's notes into an instrument on another track, so it plays
//! it.
//!
//! A wire joins a [`Tap`] to a [`Port`], at a gain, and wires into the same
//! port sum. The ports are the Compressor's and the Gate's Sidechain,
//! plugins' side inputs, and instruments' and MIDI effects' note inputs,
//! which note wires go into, on a MIDI channel or on the notes' own. One
//! that would close a loop is refused, and so is a move, an output, a send
//! or an input that would close one through a wire: see
//! [`musicable_model::routing`].

use musicable_model::native::{NativeKind, compressor, gate};
use musicable_model::param::ParamAddress;
use musicable_model::routing::{Port, Route, Tap, Wire, WireError, WireId};
use musicable_model::{Chain, DeviceId, DeviceKind, Project, TrackId};

use crate::{Args, Command, CommandError, CommandId, Context, Loop, Param, Registry, Type, Value};

/// Wires a tap to a device's side input.
pub const CONNECT: CommandId = CommandId::new("wire.connect");
/// Removes a wire.
pub const DISCONNECT: CommandId = CommandId::new("wire.disconnect");
/// Has a wire take its signal from another tap.
pub const SET_SOURCE: CommandId = CommandId::new("wire.set_source");
/// Sets a wire's gain.
pub const SET_GAIN: CommandId = CommandId::new("wire.set_gain");
/// Sets the MIDI channel a note wire plays on.
pub const SET_CHANNEL: CommandId = CommandId::new("wire.set_channel");

const WIRE: Param = Param::required("wire", Type::Wire, "The wire.");

/// The arguments that name a tap: a track or the master and a point on it,
/// or a device.
const TRACK: Param = Param::optional("track", Type::Track, "The track the signal comes from.");
const MASTER: Param = Param::optional(
    "master",
    Type::Bool,
    "Takes the signal from the master instead.",
);
const TAP: Param = Param::optional(
    "tap",
    Type::Choice(TAPS),
    "Where on the track or the master the signal comes from: what comes into its devices, what \
     comes out of the last of them, or what comes out of its fader. post_fx without it. The \
     master has no pre_fx.",
);
const AFTER: Param = Param::optional(
    "after",
    Type::Device,
    "Takes what this device puts out instead, on any track.",
);

/// The points on a track that a wire or a track's input takes from, as the
/// commands name them.
pub(crate) const TAPS: &[&str] = &["pre_fx", "post_fx", "post_fader"];

/// The argument that names a note wire's channel.
const CHANNEL: Param = Param::optional(
    "channel",
    Type::Int,
    "The MIDI channel to play every note on, from 0 to 15: each note on its own without it, \
     with its own expression.",
);

/// Registers the wire commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(CONNECT, "Connect Wire").with_params(&[
            Param::required(
                "device",
                Type::Device,
                "The device the signal goes into, at a side input: a Compressor's or a \
                 Gate's Sidechain, which this turns on, or a plugin's. Or with notes, its note \
                 input.",
            ),
            Param::optional(
                "input",
                Type::Int,
                "Which of a plugin's side inputs, by the plugin's id for it, as get_device \
                 lists them: its first without it. A Compressor or a Gate has one.",
            ),
            Param::optional(
                "notes",
                Type::Bool,
                "Wires notes into the device's note input instead: an instrument's or a MIDI \
                 effect's, from a MIDI track's pre_fx or post_fx, or after one of its MIDI \
                 effects. That's how a track plays an instrument on another.",
            ),
            TRACK,
            MASTER,
            TAP,
            AFTER,
            Param::optional(
                "gain",
                Type::Number,
                "The wire's gain, linear: 1 is unity, and the most is about 15.85, +24 dB. 1 \
                 without it. A note wire has none.",
            ),
            Param::optional(
                "channel",
                Type::Int,
                "For a note wire, the MIDI channel to play every note on, from 0 to 15: each \
                 note on its own without it, with its own expression.",
            ),
        ]),
        |ctx, args| {
            let from = tap(ctx, args)?;
            let notes = args.opt("notes")?.unwrap_or(false);
            let to = port(
                ctx.document.project(),
                args.get("device")?,
                args.opt("input")?,
                notes,
            )?;
            let gain = args.opt::<f64>("gain")?;
            let channel = channel(args)?;
            match (notes, gain, channel) {
                (true, Some(_), _) => {
                    return Err(CommandError::invalid(
                        "a note wire has no gain: leave out gain",
                    ));
                }
                (false, _, Some(_)) => {
                    return Err(CommandError::invalid(
                        "only a note wire has a channel: give notes, or leave out channel",
                    ));
                }
                _ => {}
            }
            let gain = gain.map_or(1.0, |gain| gain as f32);
            let made = ctx.document.transact("Connect Wire", |tx| {
                let wire = Wire {
                    gain,
                    channel,
                    ..tx.new_wire(from, to)
                };
                tx.add_wire(usize::MAX, wire)?;
                if let Some(switch) = switch(tx.project(), to) {
                    tx.set_param_value(switch, 1.0);
                }
                Ok(wire.id)
            });
            let project = ctx.document.project();
            let wire = made.map_err(|error| wire_error(project, from, to, error))?;
            Ok(wire.into())
        },
    );
    registry.add(
        Command::new(DISCONNECT, "Disconnect Wire").with_params(&[WIRE]),
        |ctx, args| {
            let wire = existing(ctx, args.get("wire")?)?;
            ctx.document.transact("Disconnect Wire", |tx| {
                tx.remove_wire(wire);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_SOURCE, "Set Wire Source").with_params(&[WIRE, TRACK, MASTER, TAP, AFTER]),
        |ctx, args| {
            let wire = existing(ctx, args.get("wire")?)?;
            let from = tap(ctx, args)?;
            let made = ctx
                .document
                .transact("Wire Source", |tx| tx.set_wire_from(wire, from));
            let project = ctx.document.project();
            let to = project.wire(wire).expect("the wire is the project's").to;
            made.map_err(|error| wire_error(project, from, to, error))?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_GAIN, "Set Wire Gain").with_params(&[
            WIRE,
            Param::required(
                "gain",
                Type::Number,
                "The gain, linear: 1 is unity, and the most is about 15.85, +24 dB.",
            ),
        ]),
        |ctx, args| {
            let wire = existing(ctx, args.get("wire")?)?;
            if ctx
                .document
                .project()
                .wire(wire)
                .is_some_and(Wire::takes_notes)
            {
                return Err(CommandError::invalid(format!(
                    "wire {} is a note wire, which has no gain",
                    wire.0
                )));
            }
            let gain = args.get::<f64>("gain")?;
            // As if by hand: a lane on the gain stops moving it.
            ctx.document.transact("Wire Gain", |tx| {
                tx.set_param_by_hand(ParamAddress::Wire(wire), gain);
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_CHANNEL, "Set Wire Channel").with_params(&[WIRE, CHANNEL]),
        |ctx, args| {
            let wire = existing(ctx, args.get("wire")?)?;
            if !ctx
                .document
                .project()
                .wire(wire)
                .is_some_and(Wire::takes_notes)
            {
                return Err(CommandError::invalid(format!(
                    "wire {} takes sound, which has no channel: only a note wire has one",
                    wire.0
                )));
            }
            let channel = channel(args)?;
            ctx.document.transact("Wire Channel", |tx| {
                tx.set_wire_channel(wire, channel);
            });
            Ok(Value::None)
        },
    );
}

/// The MIDI channel the argument `channel` names, from 0 to 15, or `None`
/// without it.
pub(crate) fn channel(args: &Args) -> Result<Option<u8>, CommandError> {
    let Some(channel) = args.opt::<i64>("channel")? else {
        return Ok(None);
    };
    match u8::try_from(channel) {
        Ok(channel) if channel < Wire::CHANNELS => Ok(Some(channel)),
        _ => Err(CommandError::invalid(format!(
            "there's no MIDI channel {channel}: they go from 0 to 15"
        ))),
    }
}

/// The tap the arguments of [`CONNECT`] and [`SET_SOURCE`] name: after a
/// device, or a point on a track or the master.
fn tap(ctx: &Context<'_>, args: &Args) -> Result<Tap, CommandError> {
    let project = ctx.document.project();
    let master = args.opt("master")?.unwrap_or(false);
    let track: Option<TrackId> = args.opt("track")?;
    let point: Option<String> = args.opt("tap")?;
    if let Some(device) = args.opt::<DeviceId>("after")? {
        if master || track.is_some() || point.is_some() {
            return Err(CommandError::invalid(
                "give a device to take the signal after, or a track or the master, not both",
            ));
        }
        return match project.find_device(device) {
            Some(_) => Ok(Tap::Device(device)),
            None => Err(no_device(device)),
        };
    }
    let chain = match (master, track) {
        (true, _) => Chain::Master,
        (false, Some(track)) if project.track(track).is_some() => Chain::Track(track),
        (false, Some(track)) => {
            return Err(CommandError::invalid(format!(
                "there's no track {}",
                track.0
            )));
        }
        (false, None) => {
            return Err(CommandError::invalid(
                "give a track, the master or a device to take the signal from",
            ));
        }
    };
    match (point.as_deref().unwrap_or("post_fx"), chain) {
        ("pre_fx", Chain::Track(track)) => Ok(Tap::PreFx(track)),
        ("pre_fx", Chain::Master) => Err(CommandError::invalid("the master has no Pre FX")),
        ("post_fader", chain) => Ok(Tap::PostFader(chain)),
        _ => Ok(Tap::PostFx(chain)),
    }
}

/// The port of the device with id `device` that [`CONNECT`] wires into: a
/// plugin's side input with the plugin's id `input`, or its first, a
/// Compressor's or a Gate's Sidechain, or with `notes`, its note input.
fn port(
    project: &Project,
    device: DeviceId,
    input: Option<i64>,
    notes: bool,
) -> Result<Port, CommandError> {
    let Some((_, _, found)) = project.find_device(device) else {
        return Err(no_device(device));
    };
    if notes {
        if input.is_some() {
            return Err(CommandError::invalid(
                "notes go into the device's note input, not a side input: leave out input",
            ));
        }
        return project.note_input(device).ok_or_else(|| {
            CommandError::invalid(format!(
                "{} has no note input: only an instrument and a MIDI effect have one",
                found.kind.name()
            ))
        });
    }
    let DeviceKind::Plugin(plugin) = &found.kind else {
        return match input {
            Some(_) => Err(CommandError::invalid(
                "only a plugin's side inputs have ids: leave out input",
            )),
            None => Ok(Port::Sidechain(device)),
        };
    };
    let name = found.kind.name();
    let id = match input {
        Some(input) => u32::try_from(input)
            .ok()
            .filter(|&id| plugin.input(id).is_some()),
        None => plugin.inputs.first().map(|input| input.id),
    };
    match (id, input) {
        (Some(id), _) => Ok(Port::PluginInput(device, id)),
        (None, Some(input)) => Err(CommandError::invalid(format!(
            "{name} has no side input {input}"
        ))),
        (None, None) => Err(CommandError::invalid(format!(
            "{name} has no side input: it has none, or hasn't loaded yet"
        ))),
    }
}

/// `args` with the arguments that name `port`, as [`CONNECT`] takes them.
pub fn with_port(args: Args, port: Port) -> Args {
    match port {
        Port::Sidechain(device) => args.with("device", device),
        Port::PluginInput(device, input) => {
            args.with("device", device).with("input", i64::from(input))
        }
        Port::Notes(device) => args.with("device", device).with("notes", true),
    }
}

/// `args` with the arguments that name `tap`, as [`CONNECT`] and
/// [`SET_SOURCE`] take them.
pub fn with_tap(args: Args, tap: Tap) -> Args {
    let on = |args: Args, chain| match chain {
        Chain::Track(track) => args.with("track", track),
        Chain::Master => args.with("master", true),
    };
    match tap {
        Tap::PreFx(track) => args.with("track", track).with("tap", "pre_fx"),
        Tap::Device(device) => args.with("after", device),
        Tap::PostFx(chain) => on(args, chain).with("tap", "post_fx"),
        Tap::PostFader(chain) => on(args, chain).with("tap", "post_fader"),
    }
}

/// The switch that has `port`'s device hear what comes into it: the
/// Compressor's and the Gate's Sidechain. A plugin's switches are its own.
pub fn switch(project: &Project, port: Port) -> Option<ParamAddress> {
    match port {
        Port::Sidechain(device) => {
            let (_, _, found) = project.find_device(device)?;
            let n = match found.kind.native()?.kind {
                NativeKind::Compressor => compressor::SIDECHAIN,
                NativeKind::Gate => gate::SIDECHAIN,
                _ => return None,
            };
            Some(ParamAddress::Device(device, n as u32))
        }
        Port::PluginInput(..) | Port::Notes(_) => None,
    }
}

/// The wire with id `wire`, if the project has it.
fn existing(ctx: &Context<'_>, wire: WireId) -> Result<WireId, CommandError> {
    match ctx.document.project().wire(wire) {
        Some(_) => Ok(wire),
        None => Err(CommandError::invalid(format!("there's no wire {}", wire.0))),
    }
}

fn no_device(device: DeviceId) -> CommandError {
    CommandError::invalid(format!("there's no device {}", device.0))
}

/// Why a wire from `from` to `to` can't join `project`, as the commands
/// say it.
fn wire_error(project: &Project, from: Tap, to: Port, error: WireError) -> CommandError {
    let tap = project
        .tap_name(from)
        .unwrap_or_else(|| "that tap".to_owned());
    let port = project
        .port_name(to)
        .unwrap_or_else(|| "that device".to_owned());
    CommandError::invalid(match error {
        WireError::NoTap => format!("the project has no {tap}"),
        WireError::Notes => format!(
            "{tap} is notes, which go only into an instrument's or a MIDI effect's note input: \
             give notes"
        ),
        WireError::Sound => format!("{tap} is sound, and the {port} takes notes"),
        WireError::NoPort if to.takes_notes() => format!("the {port} has no note input"),
        WireError::NoPort => format!("the {port} has no side input"),
        WireError::Wired(wire) => {
            format!("{tap} is wired to the {port} already, by wire {}", wire.0)
        }
        WireError::Loop => {
            format!("that would make a loop: what the {port} puts out reaches {tap}")
        }
    })
}

/// The error for an edit refused because it would close a loop through a
/// track's input or a wire, naming it.
pub(crate) fn refused(project: &Project, Loop(route): Loop) -> CommandError {
    let route = match route {
        Route::Wire(wire) => {
            let ends = project
                .wire(wire)
                .and_then(|w| Some((project.tap_name(w.from)?, project.port_name(w.to)?)));
            match ends {
                Some((from, to)) => format!("the wire from {from} to the {to}"),
                None => format!("wire {}", wire.0),
            }
        }
        Route::Input(track) => {
            let input = project.track(track).and_then(|t| {
                let tap = t.input.as_ref()?.tap()?;
                Some((t.name.as_str(), project.tap_name(tap)?))
            });
            match input {
                Some((name, tap)) => format!("{name}'s input from {tap}"),
                None => format!("track {}'s input", track.0),
            }
        }
    };
    CommandError::invalid(format!("that would make a loop, through {route}"))
}

#[cfg(test)]
mod tests {
    use musicable_model::mixer::Bus;
    use musicable_model::native::NativeDevice;
    use musicable_model::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings};
    use musicable_model::time::Beats;
    use musicable_model::{PluginFormat, TrackKind};

    use super::*;
    use crate::testing::Setup;
    use crate::{automation, device, mixer, modulation, track};

    /// A kick with a gate, a bass with an EQ and a compressor after it, a
    /// group, a return track and a MIDI track.
    struct Mix {
        setup: Setup,
        kick: TrackId,
        bass: TrackId,
        group: TrackId,
        reverb: TrackId,
        keys: TrackId,
        gate: DeviceId,
        eq: DeviceId,
        compressor: DeviceId,
    }

    fn mix() -> Mix {
        let mut project = Project::new();
        let kick = project.add_track("Kick", TrackKind::Audio);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let group = project.add_track("Group", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let mut device = |kind| project.new_device(DeviceKind::Native(NativeDevice::new(kind)));
        let [gate, eq, compressor] =
            [NativeKind::Gate, NativeKind::Eq, NativeKind::Compressor].map(&mut device);
        let ids = [gate.id, eq.id, compressor.id];
        project.track_mut(kick).unwrap().devices = vec![gate];
        project.track_mut(bass).unwrap().devices = vec![eq, compressor];
        Mix {
            setup: Setup::new(project),
            kick,
            bass,
            group,
            reverb,
            keys,
            gate: ids[0],
            eq: ids[1],
            compressor: ids[2],
        }
    }

    impl Mix {
        fn project(&self) -> &Project {
            self.setup.document.project()
        }

        /// Wires what `from` names into `device`'s side input.
        fn connect(&mut self, from: Args, device: DeviceId) -> Result<WireId, CommandError> {
            let args = from.with("device", device);
            match self.setup.run_with(&CONNECT, args)? {
                Value::Wire(wire) => Ok(wire),
                other => panic!("{other:?}"),
            }
        }

        /// Wires the track `from`'s Post FX into `device`'s side input.
        fn key(&mut self, from: TrackId, device: DeviceId) -> WireId {
            self.connect(Args::new().with("track", from), device)
                .unwrap()
        }

        fn wired(&self) -> Vec<(Tap, Port)> {
            self.project()
                .wires
                .iter()
                .map(|w| (w.from, w.to))
                .collect()
        }

        fn devices(&self, track: TrackId) -> Vec<DeviceId> {
            let devices = self.project().devices(Chain::Track(track)).unwrap();
            devices.iter().map(|d| d.id).collect()
        }
    }

    #[test]
    fn a_kick_keys_the_bass_as_one_step_with_its_sidechain_on() {
        let mut m = mix();
        let wire = m.key(m.kick, m.compressor);
        let to = Port::Sidechain(m.compressor);
        let sidechain = ParamAddress::Device(m.compressor, compressor::SIDECHAIN as u32);
        let keyed = Wire {
            id: wire,
            from: Tap::PostFx(Chain::Track(m.kick)),
            to,
            gain: 1.0,
            channel: None,
        };
        assert_eq!(m.project().wires, [keyed]);
        assert_eq!(m.project().param_value(sidechain), Some(1.0));
        assert_eq!(m.setup.committed_names(), ["Connect Wire"]);

        let gain = |m: &Mix| m.project().wire(wire).map(|w| w.gain);
        let set_gain = |m: &mut Mix, gain: f64| {
            let args = Args::new().with("wire", wire).with("gain", gain);
            m.setup.run_with(&SET_GAIN, args).unwrap();
        };
        set_gain(&mut m, 0.5);
        assert_eq!(gain(&m), Some(0.5));
        // Kept to its range.
        set_gain(&mut m, 100.0);
        assert_eq!(gain(&m), Some(Wire::GAIN_MAX));
        set_gain(&mut m, -1.0);
        assert_eq!(gain(&m), Some(0.0));
        m.setup
            .run_with(&DISCONNECT, Args::new().with("wire", wire))
            .unwrap();
        assert!(m.project().wires.is_empty());
        assert_eq!(
            m.setup.committed_names(),
            ["Wire Gain", "Wire Gain", "Wire Gain", "Disconnect Wire"]
        );
        // Undo brings it back as it was, then takes it away, Sidechain and all.
        assert!(m.setup.document.undo());
        assert_eq!(gain(&m), Some(0.0));
        for _ in 0..3 {
            assert!(m.setup.document.undo());
        }
        assert_eq!(m.project().wires, [keyed]);
        assert!(m.setup.document.undo());
        assert!(m.project().wires.is_empty());
        assert_eq!(m.project().param_value(sidechain), Some(0.0));
    }

    #[test]
    fn a_wire_takes_from_any_tap_that_makes_no_loop() {
        let mut m = mix();
        let (kick, bass, eq, compressor) = (m.kick, m.bass, m.eq, m.compressor);
        let pre_fx = Args::new().with("track", bass).with("tap", "pre_fx");
        m.connect(pre_fx, compressor).unwrap();
        m.connect(Args::new().with("after", eq), compressor)
            .unwrap();
        let post_fader = Args::new().with("track", kick).with("tap", "post_fader");
        m.connect(post_fader.with("gain", 2.0), compressor).unwrap();
        assert_eq!(
            m.wired(),
            [
                (Tap::PreFx(bass), Port::Sidechain(compressor)),
                (Tap::Device(eq), Port::Sidechain(compressor)),
                (
                    Tap::PostFader(Chain::Track(kick)),
                    Port::Sidechain(compressor)
                ),
            ]
        );
        assert_eq!(m.project().wires[2].gain, 2.0);

        let error =
            |m: &mut Mix, from: Args, device| m.connect(from, device).unwrap_err().to_string();
        assert_eq!(
            error(&mut m, Args::new().with("master", true), compressor),
            "that would make a loop: what the Compressor on Bass puts out reaches Master Post FX"
        );
        assert_eq!(
            error(&mut m, Args::new().with("after", compressor), compressor),
            "that would make a loop: what the Compressor on Bass puts out reaches Bass after Compressor"
        );
        let master_pre_fx = Args::new().with("master", true).with("tap", "pre_fx");
        assert_eq!(
            error(&mut m, master_pre_fx, compressor),
            "the master has no Pre FX"
        );
        let both = Args::new().with("after", m.eq).with("track", m.kick);
        assert_eq!(
            error(&mut m, both, compressor),
            "give a device to take the signal after, or a track or the master, not both"
        );
        assert_eq!(
            error(&mut m, Args::new(), compressor),
            "give a track, the master or a device to take the signal from"
        );
        let keys = Args::new().with("track", m.keys).with("tap", "pre_fx");
        assert_eq!(
            error(&mut m, keys, compressor),
            "Keys Pre FX is notes, which go only into an instrument's or a MIDI effect's note \
             input: give notes"
        );
        assert_eq!(
            error(&mut m, Args::new().with("track", kick), eq),
            "the EQ on Bass has no side input"
        );
        let again = Args::new().with("track", bass).with("tap", "pre_fx");
        assert_eq!(
            error(&mut m, again, compressor),
            "Bass Pre FX is wired to the Compressor on Bass already, by wire 0"
        );
        assert_eq!(
            error(&mut m, Args::new().with("track", TrackId(99)), compressor),
            "there's no track 99"
        );
        assert_eq!(
            error(&mut m, Args::new().with("track", kick), DeviceId(99)),
            "there's no device 99"
        );
        let gone = Args::new().with("wire", WireId(99));
        let disconnect = m.setup.run_with(&DISCONNECT, gone);
        assert_eq!(disconnect.unwrap_err().to_string(), "there's no wire 99");
        // Refused, they left no step, though they used up wire ids.
        assert_eq!(m.setup.committed_names().len(), 3);
        assert_eq!(m.project().wires.len(), 3);
    }

    #[test]
    fn outputs_sends_and_moves_that_would_close_a_loop_are_refused() {
        let mut m = mix();
        let (bass, compressor) = (m.bass, m.compressor);
        m.key(m.group, compressor);
        m.key(m.reverb, compressor);
        m.connect(Args::new().with("after", m.eq), compressor)
            .unwrap();
        m.setup.committed_names();
        let before = m.project().clone();

        // The group and the return key the bass, so it can't feed either.
        let output = Args::new().with("track", bass).with("group", m.group);
        let refused = m.setup.run_with(&mixer::SET_OUTPUT, output);
        assert_eq!(
            refused.unwrap_err().to_string(),
            "that would make a loop, through the wire from Group Post FX to the Compressor on \
             Bass"
        );
        let send = (Args::new().with("track", bass))
            .with("to", m.reverb)
            .with("level", 0.5);
        let refused = m.setup.run_with(&mixer::SET_SEND_LEVEL, send);
        assert_eq!(
            refused.unwrap_err().to_string(),
            "that would make a loop, through the wire from Reverb Post FX to the Compressor on \
             Bass"
        );
        // Nor can the compressor go before the EQ it's keyed after, by a
        // move or a nudge.
        let to_start = (Args::new().with("devices", vec![compressor]))
            .with("track", bass)
            .with("index", 0_i64);
        let refused = m.setup.run_with(&device::MOVE, to_start);
        let loop_after_eq =
            "that would make a loop, through the wire from Bass after EQ to the Compressor on Bass";
        assert_eq!(refused.unwrap_err().to_string(), loop_after_eq);
        m.setup.select_devices(&[compressor]);
        let refused = m.setup.run_with(&device::MOVE_LEFT, Args::new());
        assert_eq!(refused.unwrap_err().to_string(), loop_after_eq);
        assert_eq!(m.setup.committed_names(), [] as [&str; 0]);
        assert_eq!(m.project().tracks, before.tracks);
        assert_eq!(m.project().wires, before.wires);
        // A gate on the master, keyed after an EQ there, can't go onto a
        // new track, which would feed the master: the new track goes too.
        let add = |m: &mut Mix, kind: &str| {
            let args = Args::new().with("kind", kind).with("master", true);
            match m.setup.run_with(&device::ADD, args).unwrap() {
                Value::Device(device) => device,
                other => panic!("{other:?}"),
            }
        };
        let [master_eq, master_gate] = ["eq", "gate"].map(|kind| add(&mut m, kind));
        m.connect(Args::new().with("after", master_eq), master_gate)
            .unwrap();
        m.setup.committed_names();
        let tracks = m.project().tracks.clone();
        let new_track = (Args::new().with("devices", vec![master_gate])).with("new_track", true);
        let moved = m.setup.run_with(&device::MOVE, new_track);
        assert_eq!(
            moved.unwrap_err().to_string(),
            "that would make a loop, through the wire from Master after EQ to the Gate on Master"
        );
        assert_eq!(m.setup.committed_names(), [] as [&str; 0]);
        assert_eq!(m.project().tracks, tracks);

        // Other tracks can feed the group.
        let output = Args::new().with("track", m.kick).with("group", m.group);
        m.setup.run_with(&mixer::SET_OUTPUT, output).unwrap();
        assert_eq!(
            m.project().track(m.kick).unwrap().output,
            Bus::Group(m.group)
        );
    }

    #[test]
    fn devices_moved_together_are_checked_once_they_have_all_moved() {
        let mut m = mix();
        let (eq, compressor) = (m.eq, m.compressor);
        m.connect(Args::new().with("after", eq), compressor)
            .unwrap();
        let output = Args::new().with("track", m.bass).with("group", m.group);
        m.setup.run_with(&mixer::SET_OUTPUT, output).unwrap();
        // With the EQ moved first, the compressor would key the group from
        // the bass, which the group feeds, until it moved too.
        let both = (Args::new().with("devices", vec![eq, compressor])).with("track", m.group);
        m.setup.run_with(&device::MOVE, both).unwrap();
        assert_eq!(m.devices(m.group), [eq, compressor]);
        assert_eq!(m.devices(m.bass), []);
        assert_eq!(m.wired(), [(Tap::Device(eq), Port::Sidechain(compressor))]);
        // And undo takes them back the same way.
        assert!(m.setup.document.undo());
        assert_eq!(m.devices(m.bass), [eq, compressor]);
    }

    #[test]
    fn a_wire_takes_another_source_and_keeps_its_gain_and_lane() {
        let mut m = mix();
        let (kick, bass, gate, compressor) = (m.kick, m.bass, m.gate, m.compressor);
        let wire = m.connect(
            Args::new().with("track", kick).with("gain", 0.5),
            compressor,
        );
        let wire = wire.unwrap();
        let lane = (Args::new().with("param", ParamAddress::Wire(wire)))
            .with("at", Beats::new(4))
            .with("value", 0.25);
        m.setup.run_with(&automation::ADD_POINT, lane).unwrap();
        let pre_fx = Args::new().with("track", kick).with("tap", "pre_fx");
        let other = m.connect(pre_fx, compressor).unwrap();
        m.setup.committed_names();

        let source =
            |m: &mut Mix, from: Args| m.setup.run_with(&SET_SOURCE, from.with("wire", wire));
        source(&mut m, Args::new().with("after", gate)).unwrap();
        let to = Port::Sidechain(compressor);
        let moved = Wire {
            id: wire,
            from: Tap::Device(gate),
            to,
            gain: 0.5,
            channel: None,
        };
        assert_eq!(m.project().wires[0], moved);
        assert!(m.project().lane(ParamAddress::Wire(wire)).is_some());
        assert_eq!(m.setup.committed_names(), ["Wire Source"]);
        // The same source again changes nothing.
        source(&mut m, Args::new().with("after", gate)).unwrap();
        assert_eq!(m.setup.committed_names(), Vec::<String>::new());

        // Not from a tap another wire into it takes already, or after it.
        let pre_fx = Args::new().with("track", kick).with("tap", "pre_fx");
        assert_eq!(
            source(&mut m, pre_fx),
            Err(CommandError::invalid(format!(
                "Kick Pre FX is wired to the Compressor on Bass already, by wire {}",
                other.0
            )))
        );
        assert_eq!(
            source(&mut m, Args::new().with("track", bass)),
            Err(CommandError::invalid(
                "that would make a loop: what the Compressor on Bass puts out reaches Bass Post FX"
            ))
        );
        assert_eq!(m.project().wires[0], moved);

        assert!(m.setup.document.undo());
        assert_eq!(m.project().wires[0].from, Tap::PostFx(Chain::Track(kick)));
    }

    #[test]
    fn deleting_a_track_or_a_device_deletes_its_wires_in_the_same_step() {
        let mut m = mix();
        let (kick, compressor) = (m.kick, m.compressor);
        let from_kick = m.key(kick, compressor);
        let pre_fx = Args::new().with("track", m.bass).with("tap", "pre_fx");
        let to_gate = m.connect(pre_fx, m.gate).unwrap();
        let after_eq = m
            .connect(Args::new().with("after", m.eq), compressor)
            .unwrap();
        let lane = (Args::new().with("param", ParamAddress::Wire(from_kick)))
            .with("at", Beats::new(4))
            .with("value", 0.5);
        m.setup.run_with(&automation::ADD_POINT, lane).unwrap();
        m.setup.committed_names();
        let before = m.project().clone();

        let delete = Args::new().with("tracks", vec![kick]);
        m.setup.run_with(&track::DELETE, delete).unwrap();
        let ids: Vec<WireId> = m.project().wires.iter().map(|w| w.id).collect();
        assert_eq!(ids, [after_eq], "{to_gate:?} went too");
        assert!(m.project().automation.is_empty());
        assert_eq!(m.setup.committed_names().len(), 1);
        assert!(m.setup.document.undo());
        assert_eq!(m.project(), &before);

        let delete = Args::new().with("device", m.eq);
        m.setup.run_with(&device::DELETE, delete).unwrap();
        let ids: Vec<WireId> = m.project().wires.iter().map(|w| w.id).collect();
        assert_eq!(ids, [from_kick, to_gate]);
        let delete = Args::new().with("device", compressor);
        m.setup.run_with(&device::DELETE, delete).unwrap();
        let ids: Vec<WireId> = m.project().wires.iter().map(|w| w.id).collect();
        assert_eq!(ids, [to_gate]);
        assert!(m.project().automation.is_empty());
        assert!(m.setup.document.undo());
        assert!(m.setup.document.undo());
        assert_eq!(m.project(), &before);
    }

    #[test]
    fn routings_to_a_wires_gain_go_with_the_wire() {
        let mut m = mix();
        let (kick, bass, compressor) = (m.kick, m.bass, m.compressor);
        let wire = m.key(kick, compressor);
        let knob = |m: &mut Mix, track: TrackId| {
            let add = Args::new().with("kind", "macro").with("track", track);
            let Value::Modulator(knob) = m.setup.run_with(&modulation::ADD, add).unwrap() else {
                panic!("a Macro");
            };
            let routing = (Args::new().with("modulator", knob))
                .with("param", ParamAddress::Wire(wire))
                .with("amount", -0.5);
            m.setup.run_with(&modulation::ADD_ROUTING, routing)
        };
        // It's in reach where the compressor is, not where the kick is.
        assert_eq!(
            knob(&mut m, kick),
            Err(CommandError::invalid(format!(
                "wire {} is out of the modulator's reach: its own channel's parameters, those \
                 of the tracks feeding a group it's on, and on the master, every one",
                wire.0
            )))
        );
        knob(&mut m, bass).unwrap();
        m.setup.committed_names();
        let before = m.project().clone();
        let routings = |m: &Mix| {
            let strip = m.project().modulation(Chain::Track(bass)).unwrap();
            strip.routings.len()
        };
        assert_eq!(routings(&m), 1);

        let disconnect = Args::new().with("wire", wire);
        m.setup.run_with(&DISCONNECT, disconnect).unwrap();
        assert!(m.project().wires.is_empty());
        assert_eq!(routings(&m), 0);
        assert_eq!(m.setup.committed_names().len(), 1);
        assert!(m.setup.document.undo());
        assert_eq!(m.project(), &before);

        // And with the device it feeds, or the track it comes from.
        let delete = Args::new().with("device", compressor);
        m.setup.run_with(&device::DELETE, delete).unwrap();
        assert_eq!(routings(&m), 0);
        assert!(m.setup.document.undo());
        let delete = Args::new().with("tracks", vec![kick]);
        m.setup.run_with(&track::DELETE, delete).unwrap();
        assert_eq!(routings(&m), 0);
        assert!(m.setup.document.undo());
        assert_eq!(m.project(), &before);
    }

    #[test]
    fn copies_keep_their_keys_but_not_the_wires_out_of_them() {
        let mut m = mix();
        let (bass, eq, compressor) = (m.bass, m.eq, m.compressor);
        // From before the kick's gate, so the gate can key from the bass.
        let pre_fx = Args::new().with("track", m.kick).with("tap", "pre_fx");
        let from_kick = m.connect(pre_fx, compressor).unwrap();
        m.connect(Args::new().with("after", eq), compressor)
            .unwrap();
        m.connect(Args::new().with("after", compressor), m.gate)
            .unwrap();
        let lane = (Args::new().with("param", ParamAddress::Wire(from_kick)))
            .with("at", Beats::new(4))
            .with("value", 0.5);
        m.setup.run_with(&automation::ADD_POINT, lane).unwrap();

        // Both, to the end: the copy is keyed after the EQ's copy.
        let copy = Args::new()
            .with("devices", vec![eq, compressor])
            .with("track", bass);
        let Value::List(copies) = m.setup.run_with(&device::COPY, copy).unwrap() else {
            panic!("copies");
        };
        let [Value::Device(eq_2), Value::Device(compressor_2)] = copies[..] else {
            panic!("{copies:?}");
        };
        let kick = Tap::PreFx(m.kick);
        let into_copy = [
            (kick, Port::Sidechain(compressor_2)),
            (Tap::Device(eq_2), Port::Sidechain(compressor_2)),
        ];
        assert_eq!(m.wired()[3..], into_copy);
        // With its gain's lane.
        let keyed = m.project().wires[3];
        assert!(m.project().lane(ParamAddress::Wire(keyed.id)).is_some());

        // The compressor alone, before the EQ it's keyed after: that key
        // would loop, so the copy keeps the kick's alone.
        let copy = (Args::new().with("devices", vec![compressor]))
            .with("track", bass)
            .with("index", 0_i64);
        let Value::List(copies) = m.setup.run_with(&device::COPY, copy).unwrap() else {
            panic!("copies");
        };
        let [Value::Device(compressor_3)] = copies[..] else {
            panic!("{copies:?}");
        };
        assert_eq!(m.wired()[5..], [(kick, Port::Sidechain(compressor_3))]);
        assert_eq!(m.devices(bass)[..2], [compressor_3, eq]);
    }

    fn input(id: u32, name: &str) -> PluginInput {
        PluginInput {
            id,
            name: name.into(),
        }
    }

    /// Adds a plugin called `name`, with side inputs `inputs`, to the end of
    /// `chain`, as one step.
    fn add_plugin(m: &mut Mix, chain: Chain, name: &str, inputs: Vec<PluginInput>) -> DeviceId {
        let kind = DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: format!("org.example.{name}"),
            name: name.into(),
            kind: PluginKind::AudioEffect,
            inputs,
            settings: PluginSettings::default(),
        });
        let index = m.project().devices(chain).unwrap().len();
        let id = m.setup.document.transact("Add Plugin", |tx| {
            let device = tx.new_device(kind);
            let id = device.id;
            tx.add_device(chain, index, device);
            id
        });
        m.setup.committed_names();
        id
    }

    #[test]
    fn wires_go_into_a_plugins_side_inputs_by_its_ids() {
        let mut m = mix();
        let (kick, bass, compressor) = (m.kick, m.bass, m.compressor);
        let inputs = vec![input(1, "Carrier"), input(3, "Aux")];
        let vocoder = add_plugin(&mut m, Chain::Track(bass), "Vocoder", inputs);
        let effect = add_plugin(&mut m, Chain::Master, "Effect", Vec::new());
        let connect = |m: &mut Mix, device: DeviceId, input: Option<i64>| {
            let args = Args::new().with("track", kick).with("device", device);
            let args = match input {
                Some(input) => args.with("input", input),
                None => args,
            };
            m.setup.run_with(&CONNECT, args)
        };
        // Into its first without an input.
        connect(&mut m, vocoder, None).unwrap();
        connect(&mut m, vocoder, Some(3)).unwrap();
        let from = Tap::PostFx(Chain::Track(kick));
        let [carrier, aux] = [1, 3].map(|id| Port::PluginInput(vocoder, id));
        assert_eq!(m.wired(), [(from, carrier), (from, aux)]);
        // The plugin's own switches turn it on, so it's one edit.
        assert_eq!(m.setup.committed_names(), ["Connect Wire", "Connect Wire"]);
        let error = |m: &mut Mix, device, input| connect(m, device, input).unwrap_err().to_string();
        assert_eq!(
            error(&mut m, vocoder, Some(2)),
            "Vocoder has no side input 2"
        );
        assert_eq!(
            error(&mut m, vocoder, Some(-1)),
            "Vocoder has no side input -1"
        );
        assert_eq!(
            error(&mut m, effect, None),
            "Effect has no side input: it has none, or hasn't loaded yet"
        );
        assert_eq!(
            error(&mut m, compressor, Some(1)),
            "only a plugin's side inputs have ids: leave out input"
        );
        assert_eq!(
            error(&mut m, vocoder, Some(1)),
            "Kick Post FX is wired to the Vocoder Carrier on Bass already, by wire 0"
        );
        let after = Args::new().with("after", vocoder).with("device", vocoder);
        assert_eq!(
            m.setup.run_with(&CONNECT, after).unwrap_err().to_string(),
            "that would make a loop: what the Vocoder Carrier on Bass puts out reaches Bass \
             after Vocoder"
        );
        assert!(m.setup.committed_names().is_empty());
        // Deleting the plugin takes its wires with it.
        let delete = Args::new().with("device", vocoder);
        m.setup.run_with(&device::DELETE, delete).unwrap();
        assert!(m.project().wires.is_empty());
        assert!(m.setup.document.undo());
        assert_eq!(m.wired(), [(from, carrier), (from, aux)]);
    }

    #[test]
    fn a_plugins_inputs_follow_the_plugin_but_keep_the_wired_ones() {
        let mut m = mix();
        let (kick, bass) = (m.kick, m.bass);
        let inputs = vec![input(1, "Carrier"), input(3, "Aux")];
        let vocoder = add_plugin(&mut m, Chain::Track(bass), "Vocoder", inputs);
        m.key(kick, vocoder);
        m.setup.committed_names();
        let follow = |m: &mut Mix, inputs: Vec<PluginInput>| {
            (m.setup.document).follow("Change Vocoder", |tx| tx.set_plugin_inputs(vocoder, inputs));
        };
        let listed = |m: &Mix| {
            let device = m.project().find_device(vocoder).unwrap().2;
            device.kind.plugin().unwrap().inputs.clone()
        };
        // An update of the plugin drops the carrier and adds a key: the
        // carrier stays while the kick goes into it.
        follow(&mut m, vec![input(3, "Aux"), input(4, "Key")]);
        let kept = vec![input(3, "Aux"), input(4, "Key"), input(1, "Carrier")];
        assert_eq!(listed(&m), kept);
        assert_eq!(m.setup.committed_names(), ["Change Vocoder"]);
        // Two inputs with one id are the plugin's mistake: nothing changes.
        follow(&mut m, vec![input(3, "Aux"), input(3, "Key")]);
        assert_eq!(listed(&m), kept);
        assert!(m.setup.committed_names().is_empty());
        // No step to undo: undo takes the wire back.
        assert!(m.setup.document.undo());
        assert!(m.project().wires.is_empty());
        assert_eq!(listed(&m), kept);
        // Unwired, the carrier goes when the plugin next says.
        follow(&mut m, vec![input(3, "Aux"), input(4, "Key")]);
        assert_eq!(listed(&m), [input(3, "Aux"), input(4, "Key")]);
        // So redo can't bring back the wire into it, and changes nothing.
        assert!(m.setup.document.redo());
        assert!(m.project().wires.is_empty());
        assert_eq!(
            m.setup.committed_names(),
            ["Undo Connect Wire", "Change Vocoder"]
        );
        // Back in a later version, it takes wires again.
        follow(&mut m, vec![input(1, "Carrier"), input(3, "Aux")]);
        assert_eq!(listed(&m), [input(1, "Carrier"), input(3, "Aux")]);
        let again = m.key(kick, vocoder);
        assert_eq!(
            m.project().wire(again).unwrap().to,
            Port::PluginInput(vocoder, 1)
        );
        // Other devices have no inputs to set.
        let compressor = m.compressor;
        follow_other(&mut m, compressor);
        assert_eq!(
            m.setup.committed_names(),
            ["Change Vocoder", "Connect Wire"]
        );

        fn follow_other(m: &mut Mix, device: DeviceId) {
            let inputs = vec![input(1, "Sidechain")];
            (m.setup.document).follow("Change", |tx| tx.set_plugin_inputs(device, inputs));
        }
    }

    #[test]
    fn a_midi_track_plays_another_tracks_synth_through_a_note_wire() {
        let mut m = mix();
        let (lead, synth) = m.setup.document.transact("Lead", |tx| {
            let lead = tx.new_track("Lead", TrackKind::Midi);
            let id = lead.id;
            tx.add_track(usize::MAX, lead);
            let synth = tx.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Synth)));
            let synth_id = synth.id;
            tx.add_device(Chain::Track(id), 0, synth);
            (id, synth_id)
        });
        m.setup.committed_names();
        let notes = |from: Args| from.with("notes", true).with("device", synth);
        let keys = Args::new().with("track", m.keys);
        let wire = match m
            .setup
            .run_with(&CONNECT, notes(keys.clone()).with("channel", 3_i64))
        {
            Ok(Value::Wire(wire)) => wire,
            other => panic!("{other:?}"),
        };
        let played = Wire {
            id: wire,
            from: Tap::PostFx(Chain::Track(m.keys)),
            to: Port::Notes(synth),
            gain: 1.0,
            channel: Some(3),
        };
        assert_eq!(m.project().wire(wire), Some(&played));
        assert_eq!(m.setup.committed_names(), ["Connect Wire"]);
        let refused = |m: &mut Mix, command: &CommandId, args: Args| {
            m.setup.run_with(command, args).unwrap_err().to_string()
        };
        let error = refused(&mut m, &CONNECT, notes(keys.clone()));
        assert!(
            error.contains("is wired to the Synth on Lead already"),
            "{error}"
        );
        // Sound goes into no note input, and notes into no side input.
        let kick = Args::new().with("track", m.kick);
        let error = refused(&mut m, &CONNECT, notes(kick));
        assert!(
            error.contains("Kick Post FX is sound, and the Synth on Lead takes notes"),
            "{error}"
        );
        let pre = keys.clone().with("tap", "pre_fx");
        let compressor = pre.clone().with("device", m.compressor);
        let error = refused(&mut m, &CONNECT, compressor);
        assert!(error.contains("Keys Pre FX is notes"), "{error}");
        let gate = pre.clone().with("device", m.gate).with("notes", true);
        let error = refused(&mut m, &CONNECT, gate);
        assert!(error.contains("Gate has no note input"), "{error}");
        // A note wire has a channel, and no gain.
        let error = refused(&mut m, &CONNECT, notes(pre.clone()).with("gain", 0.5));
        assert!(error.contains("a note wire has no gain"), "{error}");
        let audio = Args::new()
            .with("track", m.kick)
            .with("device", m.compressor);
        let error = refused(&mut m, &CONNECT, audio.with("channel", 2_i64));
        assert!(error.contains("only a note wire has a channel"), "{error}");
        let error = refused(&mut m, &CONNECT, notes(pre).with("channel", 16_i64));
        assert!(error.contains("there's no MIDI channel 16"), "{error}");
        assert!(m.setup.committed_names().is_empty());

        // Its channel changes, or goes back to the notes' own.
        let channel = |m: &Mix| m.project().wire(wire).unwrap().channel;
        let set = Args::new().with("wire", wire);
        m.setup
            .run_with(&SET_CHANNEL, set.clone().with("channel", 15_i64))
            .unwrap();
        assert_eq!(channel(&m), Some(15));
        m.setup.run_with(&SET_CHANNEL, set.clone()).unwrap();
        assert_eq!(channel(&m), None);
        assert_eq!(m.setup.committed_names(), ["Wire Channel", "Wire Channel"]);
        assert!(m.setup.document.undo());
        assert_eq!(channel(&m), Some(15));
        let error = refused(&mut m, &SET_CHANNEL, set.with("channel", -1_i64));
        assert!(error.contains("there's no MIDI channel -1"), "{error}");
        let error = refused(
            &mut m,
            &SET_GAIN,
            Args::new().with("wire", wire).with("gain", 0.5),
        );
        assert!(
            error.contains("is a note wire, which has no gain"),
            "{error}"
        );
        let key = m.key(m.kick, m.compressor);
        let error = refused(&mut m, &SET_CHANNEL, Args::new().with("wire", key));
        assert!(
            error.contains("takes sound, which has no channel"),
            "{error}"
        );
        // Muting the keys silences what they play on the lead's synth.
        let muted = Args::new().with("track", m.keys).with("muted", true);
        m.setup.run_with(&mixer::SET_MUTE, muted).unwrap();
        assert!(m.project().silenced().wire(wire));
        assert!(!m.project().silenced().output(lead));
    }
}
