//! The wire commands: routing a tap's signal into a port, such as a kick's
//! into the side input of the Compressor on the bass, so the kick ducks it.
//!
//! A wire joins a [`Tap`] to a [`Port`], at a gain, and wires into the same
//! port sum. One that would close a loop is refused, and so is a move, an
//! output or a send that would close one through a wire: see
//! [`musicable_model::routing`].

use musicable_model::native::{NativeKind, compressor, gate};
use musicable_model::param::ParamAddress;
use musicable_model::routing::{Port, Tap, Wire, WireError, WireId};
use musicable_model::{Chain, DeviceId, Project, TrackId};

use crate::{Args, Command, CommandError, CommandId, Context, Loop, Param, Registry, Type, Value};

/// Wires a tap to a device's side input.
pub const CONNECT: CommandId = CommandId::new("wire.connect");
/// Removes a wire.
pub const DISCONNECT: CommandId = CommandId::new("wire.disconnect");
/// Has a wire take its signal from another tap.
pub const SET_SOURCE: CommandId = CommandId::new("wire.set_source");
/// Sets a wire's gain.
pub const SET_GAIN: CommandId = CommandId::new("wire.set_gain");

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

/// The points on a track that a wire takes from, as the commands name them.
const TAPS: &[&str] = &["pre_fx", "post_fx", "post_fader"];

/// Registers the wire commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(CONNECT, "Connect Wire").with_params(&[
            Param::required(
                "device",
                Type::Device,
                "The device the signal goes into, at its side input: a Compressor or a Gate, \
                 whose Sidechain this turns on.",
            ),
            TRACK,
            MASTER,
            TAP,
            AFTER,
            Param::optional(
                "gain",
                Type::Number,
                "The wire's gain, linear: 1 is unity, and the most is about 15.85, +24 dB. 1 \
                 without it.",
            ),
        ]),
        |ctx, args| {
            let from = tap(ctx, args)?;
            let device: DeviceId = args.get("device")?;
            if ctx.document.project().find_device(device).is_none() {
                return Err(no_device(device));
            }
            let to = Port::Sidechain(device);
            let gain = args.opt::<f64>("gain")?.map_or(1.0, |gain| gain as f32);
            let made = ctx.document.transact("Connect Wire", |tx| {
                let wire = Wire {
                    gain,
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
            let gain = args.get::<f64>("gain")?;
            // As if by hand: a lane on the gain stops moving it.
            ctx.document.transact("Wire Gain", |tx| {
                tx.set_param_by_hand(ParamAddress::Wire(wire), gain);
            });
            Ok(Value::None)
        },
    );
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
/// Compressor's and the Gate's Sidechain.
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
        WireError::Notes => format!("{tap} is notes, which no wire takes yet"),
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
/// wire, naming the wire.
pub(crate) fn refused(project: &Project, Loop(wire): Loop) -> CommandError {
    let ends = project
        .wire(wire)
        .and_then(|w| Some((project.tap_name(w.from)?, project.port_name(w.to)?)));
    let wire = match ends {
        Some((from, to)) => format!("the wire from {from} to the {to}"),
        None => format!("wire {}", wire.0),
    };
    CommandError::invalid(format!("that would make a loop, through {wire}"))
}

#[cfg(test)]
mod tests {
    use musicable_model::mixer::Bus;
    use musicable_model::native::NativeDevice;
    use musicable_model::time::Beats;
    use musicable_model::{DeviceKind, TrackKind};

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
            "Keys Pre FX is notes, which no wire takes yet"
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
}
