//! What the mixer's controls do: faders, pan, mute, solo, arm, inputs,
//! monitoring, routing, sends and clip types, for each track and the
//! master.
//!
//! A control sends a [`Change`], and the app runs the command it names (see
//! [`Change::command`]), which makes it as a transaction with [`change`].
//! Scripts and agents run the same commands.

use musicable_model::mixer::Bus;
use musicable_model::param::ParamAddress;
use musicable_model::routing::{InputError, Tap};
use musicable_model::{
    AudioInput, Chain, ClipType, DeviceId, Input, MidiInput, Monitor, Project, SendId, TrackId,
    TrackKind,
};

use crate::{
    Args, Command, CommandError, CommandId, Document, Loop, Param, Registry, Transaction, Type,
    Value, track, wire,
};

/// Sets a track's fader.
pub const SET_VOLUME: CommandId = CommandId::new("mixer.set_volume");
/// Sets a track's pan.
pub const SET_PAN: CommandId = CommandId::new("mixer.set_pan");
/// Mutes a track, or unmutes it.
pub const SET_MUTE: CommandId = CommandId::new("mixer.set_mute");
/// Solos a track, or unsolos it.
pub const SET_SOLO: CommandId = CommandId::new("mixer.set_solo");
/// Makes a track solo-safe, or not.
pub const SET_SOLO_SAFE: CommandId = CommandId::new("mixer.set_solo_safe");
/// Arms a track for recording, or disarms it.
pub const SET_ARM: CommandId = CommandId::new("mixer.set_arm");
/// Sets what a track records from: an interface's input, or another
/// track's tap.
pub const SET_INPUT: CommandId = CommandId::new("mixer.set_input");
/// Sets when a track plays its input: its monitoring.
pub const SET_MONITOR: CommandId = CommandId::new("mixer.set_monitor");
/// Sets where a track's output goes.
pub const SET_OUTPUT: CommandId = CommandId::new("mixer.set_output");
/// Sets a track's clip type.
pub const SET_CLIP_TYPE: CommandId = CommandId::new("mixer.set_clip_type");
/// Sets the level of a track's send to a return track.
pub const SET_SEND_LEVEL: CommandId = CommandId::new("mixer.set_send_level");
/// Sets whether a track's send takes the signal before the fader.
pub const SET_SEND_PRE_FADER: CommandId = CommandId::new("mixer.set_send_pre_fader");
/// Removes a track's send to a return track.
pub const REMOVE_SEND: CommandId = CommandId::new("mixer.remove_send");
/// Sets the master's fader.
pub const SET_MASTER_VOLUME: CommandId = CommandId::new("mixer.set_master_volume");
/// Sets the master's pan.
pub const SET_MASTER_PAN: CommandId = CommandId::new("mixer.set_master_pan");
/// Sets the master's clip type.
pub const SET_MASTER_CLIP_TYPE: CommandId = CommandId::new("mixer.set_master_clip_type");

const TRACK: Param = Param::required("track", Type::Track, "The track.");
const TO: Param = Param::required("to", Type::Track, "The return track the send goes to.");
const GAIN: Param = Param::required("gain", Type::Number, "The gain, linear: 1 is unity.");
const PAN: Param = Param::required("pan", Type::Number, "The balance, from -1 (left) to 1.");
const CLIP_TYPE: Param = Param::required("clip_type", Type::Text, "Off, Hard or Soft.");

/// Reads a change to a project from a command's arguments.
type Build = fn(&Args, &Project) -> Result<Change, CommandError>;

/// The mixer's commands, with their titles, parameters and the change each
/// makes.
const COMMANDS: [(CommandId, &str, &[Param], Build); 16] = [
    (SET_VOLUME, "Set Volume", &[TRACK, GAIN], |args, _| {
        Ok(Change::Gain(args.get("track")?, number(args, "gain")?))
    }),
    (SET_PAN, "Set Pan", &[TRACK, PAN], |args, _| {
        Ok(Change::Pan(args.get("track")?, number(args, "pan")?))
    }),
    (
        SET_MUTE,
        "Set Mute",
        &[
            TRACK,
            Param::required("muted", Type::Bool, "Whether it's muted."),
        ],
        |args, _| Ok(Change::Muted(args.get("track")?, args.get("muted")?)),
    ),
    (
        SET_SOLO,
        "Set Solo",
        &[
            TRACK,
            Param::required("solo", Type::Bool, "Whether it's soloed."),
            Param::optional("exclusive", Type::Bool, "Unsolos every other track."),
        ],
        |args, _| {
            Ok(Change::Solo {
                track: args.get("track")?,
                solo: args.get("solo")?,
                exclusive: args.opt("exclusive")?.unwrap_or(false),
            })
        },
    ),
    (
        SET_SOLO_SAFE,
        "Set Solo-Safe",
        &[
            TRACK,
            Param::required(
                "solo_safe",
                Type::Bool,
                "Whether soloing others leaves it on.",
            ),
        ],
        |args, _| Ok(Change::SoloSafe(args.get("track")?, args.get("solo_safe")?)),
    ),
    (
        SET_ARM,
        "Set Arm",
        &[
            TRACK,
            Param::required("armed", Type::Bool, "Whether it's armed."),
        ],
        |args, _| Ok(Change::Armed(args.get("track")?, args.get("armed")?)),
    ),
    (
        SET_INPUT,
        "Set Input",
        &[
            TRACK,
            Param::optional(
                "device",
                Type::Text,
                "The input's id, as the audio settings list it: the default input without it.",
            ),
            Param::optional("name", Type::Text, "Its name, shown while it's missing."),
            Param::optional(
                "channel",
                Type::Int,
                "An audio input's first channel, from 0, or 0 without it. A MIDI input's one \
                 channel, from 0 to 15, or every channel without it.",
            ),
            Param::optional(
                "stereo",
                Type::Bool,
                "Whether an audio input takes two channels: true without it.",
            ),
            Param::optional(
                "from",
                Type::Track,
                "Another track to take the input from, at its tap, in place of an interface's, \
                 which the track monitors In and records: its sound for an audio track, or for \
                 a MIDI track, another MIDI track's notes, its pre_fx.",
            ),
            Param::optional(
                "tap",
                Type::Choice(wire::TAPS),
                "Where on that track: what comes into its devices, what comes out of the last \
                 of them, or what comes out of its fader. post_fx without it, or pre_fx for a \
                 MIDI track.",
            ),
            Param::optional(
                "after",
                Type::Device,
                "Takes what this device, on another track, puts out instead.",
            ),
            Param::optional(
                "resampling",
                Type::Bool,
                "Takes the master's Post Fader: Resampling. The track records the mix, and plays \
                 nothing while it does, so it isn't in it.",
            ),
            Param::optional("none", Type::Bool, "Whether it records from nothing."),
        ],
        |args, project| Ok(Change::Input(args.get("track")?, input(args, project)?)),
    ),
    (
        SET_MONITOR,
        "Set Monitor",
        &[
            TRACK,
            Param::required("monitor", Type::Text, "In, Auto or Off."),
        ],
        |args, _| Ok(Change::Monitor(args.get("track")?, monitor(args)?)),
    ),
    (
        SET_OUTPUT,
        "Set Output",
        &[
            TRACK,
            Param::optional(
                "group",
                Type::Track,
                "The group track it feeds: the master without it.",
            ),
            Param::optional(
                "sends_only",
                Type::Bool,
                "Feeds nothing: the track is heard through its sends, and what other tracks take \
                 from it, alone.",
            ),
        ],
        |args, _| {
            let group = args.opt("group")?;
            let bus = match (args.opt("sends_only")?.unwrap_or(false), group) {
                (true, Some(_)) => {
                    return Err(CommandError::invalid(
                        "give a group to feed, or sends_only, not both",
                    ));
                }
                (true, None) => Bus::SendsOnly,
                (false, group) => group.map_or(Bus::Master, Bus::Group),
            };
            Ok(Change::Output(args.get("track")?, bus))
        },
    ),
    (
        SET_CLIP_TYPE,
        "Set Clip Type",
        &[TRACK, CLIP_TYPE],
        |args, _| Ok(Change::ClipType(args.get("track")?, clip_type(args)?)),
    ),
    (
        SET_SEND_LEVEL,
        "Set Send Level",
        &[
            TRACK,
            TO,
            Param::required("level", Type::Number, "The level, linear."),
        ],
        |args, _| {
            Ok(Change::SendLevel {
                track: args.get("track")?,
                to: args.get("to")?,
                level: number(args, "level")?,
            })
        },
    ),
    (
        SET_SEND_PRE_FADER,
        "Set Send Pre-Fader",
        &[
            TRACK,
            TO,
            Param::required(
                "pre_fader",
                Type::Bool,
                "Whether it takes the signal before the fader.",
            ),
        ],
        |args, _| {
            Ok(Change::SendPreFader {
                track: args.get("track")?,
                to: args.get("to")?,
                pre_fader: args.get("pre_fader")?,
            })
        },
    ),
    (REMOVE_SEND, "Remove Send", &[TRACK, TO], |args, _| {
        Ok(Change::RemoveSend {
            track: args.get("track")?,
            to: args.get("to")?,
        })
    }),
    (
        SET_MASTER_VOLUME,
        "Set Master Volume",
        &[GAIN],
        |args, _| Ok(Change::MasterGain(number(args, "gain")?)),
    ),
    (SET_MASTER_PAN, "Set Master Pan", &[PAN], |args, _| {
        Ok(Change::MasterPan(number(args, "pan")?))
    }),
    (
        SET_MASTER_CLIP_TYPE,
        "Set Master Clip Type",
        &[CLIP_TYPE],
        |args, _| Ok(Change::MasterClipType(clip_type(args)?)),
    ),
];

/// Registers the mixer's commands.
pub fn register(registry: &mut Registry) {
    for (id, title, params, build) in COMMANDS {
        registry.add(
            Command::new(id, title).with_params(params),
            move |ctx, args| {
                let change = build(args, ctx.document.project())?;
                self::change(ctx.document, change)?;
                Ok(Value::None)
            },
        );
    }
}

/// The argument `name`, as the `f32` the mixer keeps.
fn number(args: &Args, name: &str) -> Result<f32, CommandError> {
    Ok(args.get::<f64>(name)? as f32)
}

/// The clip type the argument `clip_type` names.
fn clip_type(args: &Args) -> Result<ClipType, CommandError> {
    let name: String = args.get("clip_type")?;
    [ClipType::Off, ClipType::Hard, ClipType::Soft]
        .into_iter()
        .find(|clip_type| clip_type_name(*clip_type).eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| CommandError::invalid(format!("there's no clip type called {name}")))
}

/// A clip type's name, as the mixer's commands take it.
pub fn clip_type_name(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "Off",
        ClipType::Hard => "Hard",
        ClipType::Soft => "Soft",
    }
}

/// The input the arguments of [`SET_INPUT`] name: one of the kind the track
/// records, a tap on another track it can take, or none.
fn input(args: &Args, project: &Project) -> Result<Option<Input>, CommandError> {
    if args.opt("none")?.unwrap_or(false) {
        return Ok(None);
    }
    if let Some(tap) = tap(args, project)? {
        check_tap(project, args.get("track")?, tap)?;
        return Ok(Some(Input::Tap(tap)));
    }
    let device = args.opt("device")?.unwrap_or_default();
    let name = args.opt("name")?.unwrap_or_default();
    let channel: Option<i64> = args.opt("channel")?;
    let track = project.track(args.get("track")?);
    Ok(Some(match track.map(|t| t.kind) {
        Some(TrackKind::Midi) => Input::Midi(MidiInput {
            device,
            name,
            channel: channel
                .map(|c| u8::try_from(c).ok().filter(|c| *c <= 15).ok_or(c))
                .transpose()
                .map_err(|c| {
                    CommandError::invalid(format!(
                        "there's no MIDI channel {c}: they go from 0 to 15"
                    ))
                })?,
        }),
        _ => Input::Audio(AudioInput {
            device,
            name,
            channel: match channel {
                None => 0,
                Some(c) => u16::try_from(c)
                    .ok()
                    .filter(|c| *c < u16::MAX)
                    .ok_or_else(|| CommandError::invalid(format!("there's no channel {c}")))?,
            },
            stereo: args.opt("stereo")?.unwrap_or(true),
        }),
    }))
}

/// The tap the arguments of [`SET_INPUT`] name, if they name one: a point
/// on another track, what a device puts out, or the master's Post Fader,
/// for Resampling. A point that isn't named is Post FX, or for a MIDI
/// track, which takes notes, Pre FX.
fn tap(args: &Args, project: &Project) -> Result<Option<Tap>, CommandError> {
    let from: Option<TrackId> = args.opt("from")?;
    let point: Option<String> = args.opt("tap")?;
    let after: Option<DeviceId> = args.opt("after")?;
    let resampling = args.opt("resampling")?.unwrap_or(false);
    let ways = [
        from.is_some() || point.is_some(),
        after.is_some(),
        resampling,
    ];
    match ways.iter().filter(|&&given| given).count() {
        0 => return Ok(None),
        1 => {}
        _ => {
            return Err(CommandError::invalid(
                "give a track to take the input from, a device to take it after, or \
                 resampling, only one",
            ));
        }
    }
    let interface = ["device", "name", "channel", "stereo"];
    if interface.iter().any(|name| args.value(name).is_some()) {
        return Err(CommandError::invalid(
            "give an interface's input or another track's tap, not both",
        ));
    }
    if resampling {
        return Ok(Input::RESAMPLING.tap());
    }
    if let Some(device) = after {
        return match project.find_device(device) {
            Some(_) => Ok(Some(Tap::Device(device))),
            None => Err(CommandError::invalid(format!(
                "there's no device {}",
                device.0
            ))),
        };
    }
    let Some(from) = from else {
        return Err(CommandError::invalid(
            "give the track to take the input from, as from",
        ));
    };
    if project.track(from).is_none() {
        return Err(CommandError::invalid(format!(
            "there's no track {}",
            from.0
        )));
    }
    let on = Chain::Track(from);
    let notes = (project.track(args.get("track")?)).is_some_and(|t| t.kind == TrackKind::Midi);
    let unnamed = if notes { "pre_fx" } else { "post_fx" };
    Ok(Some(match point.as_deref().unwrap_or(unnamed) {
        "pre_fx" => Tap::PreFx(from),
        "post_fader" => Tap::PostFader(on),
        _ => Tap::PostFx(on),
    }))
}

/// Whether `track` can take its input from `tap`, with an error that says
/// why not if it can't.
fn check_tap(project: &Project, track: TrackId, tap: Tap) -> Result<(), CommandError> {
    let tap_name = || {
        project
            .tap_name(tap)
            .unwrap_or_else(|| "that tap".to_owned())
    };
    let error = match project.check_input(track, tap) {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    Err(CommandError::invalid(match error {
        InputError::NoTrack => format!("there's no track {}", track.0),
        InputError::NoTap => format!("the project has no {}", tap_name()),
        InputError::Bus => "a group or a return track takes what feeds it, not an input".into(),
        InputError::Notes => format!("{} is notes: an audio track takes sound", tap_name()),
        InputError::Sound => format!(
            "{} is sound: a MIDI track takes another MIDI track's notes",
            tap_name()
        ),
        InputError::Master => {
            "the master's one tap for an input is its Post Fader: give resampling".into()
        }
        InputError::Loop => {
            let name = project
                .track(track)
                .map_or("the track", |t| t.name.as_str());
            format!(
                "that would make a loop: what comes out of {name} reaches {}",
                tap_name()
            )
        }
    }))
}

/// `args` with the arguments that name `tap` as [`SET_INPUT`] takes it:
/// the master's taps as Resampling, the one it has for an input.
pub fn with_input_tap(args: Args, tap: Tap) -> Args {
    match tap {
        Tap::PreFx(track) => args.with("from", track).with("tap", "pre_fx"),
        Tap::Device(device) => args.with("after", device),
        Tap::PostFx(Chain::Track(track)) => args.with("from", track).with("tap", "post_fx"),
        Tap::PostFader(Chain::Track(track)) => args.with("from", track).with("tap", "post_fader"),
        Tap::PostFx(Chain::Master) | Tap::PostFader(Chain::Master) => args.with("resampling", true),
    }
}

/// The monitoring the argument `monitor` names.
fn monitor(args: &Args) -> Result<Monitor, CommandError> {
    let name: String = args.get("monitor")?;
    [Monitor::In, Monitor::Auto, Monitor::Off]
        .into_iter()
        .find(|monitor| monitor_name(*monitor).eq_ignore_ascii_case(name.trim()))
        .ok_or_else(|| CommandError::invalid(format!("there's no monitor mode called {name}")))
}

/// A track's monitoring, as the mixer's commands take it.
pub fn monitor_name(monitor: Monitor) -> &'static str {
    match monitor {
        Monitor::In => "In",
        Monitor::Auto => "Auto",
        Monitor::Off => "Off",
    }
}

/// A change one of the mixer's controls makes.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// The track's fader, linear.
    Gain(TrackId, f32),
    /// The track's balance, from -1.0 to 1.0.
    Pan(TrackId, f32),
    Muted(TrackId, bool),
    /// Solos the track, or unsolos it. With `exclusive`, every other track
    /// unsolos, so it's the only one soloed.
    Solo {
        track: TrackId,
        solo: bool,
        exclusive: bool,
    },
    SoloSafe(TrackId, bool),
    Armed(TrackId, bool),
    /// What the track records from. Inputs the track can't take are left
    /// alone, and a tap that would close a loop is refused: see
    /// [`Transaction::set_track_input`]. A track that takes another's tap
    /// in place of what it took monitors In, but for Resampling, which it
    /// never monitors.
    Input(TrackId, Option<Input>),
    Monitor(TrackId, Monitor),
    /// Where the track's output goes. Buses the track can't feed are left
    /// alone: see [`Project::can_feed`](musicable_model::Project::can_feed).
    /// One that would close a loop through an input or a wire is refused.
    Output(TrackId, Bus),
    ClipType(TrackId, ClipType),
    /// The level of `track`'s send to the return track `to`, linear. A
    /// track with no send there gets one, after the fader, unless it would
    /// close a loop through an input or a wire.
    SendLevel {
        track: TrackId,
        to: TrackId,
        level: f32,
    },
    /// Whether `track`'s send to the return track `to` takes the signal
    /// before the fader. A track with no send there gets a silent one.
    SendPreFader {
        track: TrackId,
        to: TrackId,
        pre_fader: bool,
    },
    RemoveSend {
        track: TrackId,
        to: TrackId,
    },
    /// Deletes the track. Tracks feeding it feed the master instead, sends
    /// to it go, and tracks taking their input from it go to No Input.
    Delete(TrackId),
    MasterGain(f32),
    MasterPan(f32),
    MasterClipType(ClipType),
}

impl Change {
    /// The command that makes this change, with its arguments: what the
    /// mixer's controls run.
    pub fn command(&self) -> (CommandId, Args) {
        let on = |id, track: TrackId| (id, Args::new().with("track", track));
        match *self {
            Self::Input(track, None) => {
                let (id, args) = on(SET_INPUT, track);
                (id, args.with("none", true))
            }
            Self::Input(track, Some(Input::Audio(ref input))) => {
                let (id, args) = on(SET_INPUT, track);
                let args = args
                    .with("device", input.device.as_str())
                    .with("name", input.name.as_str())
                    .with("channel", i64::from(input.channel))
                    .with("stereo", input.stereo);
                (id, args)
            }
            Self::Input(track, Some(Input::Midi(ref input))) => {
                let (id, args) = on(SET_INPUT, track);
                let mut args = args
                    .with("device", input.device.as_str())
                    .with("name", input.name.as_str());
                if let Some(channel) = input.channel {
                    args.set("channel", i64::from(channel));
                }
                (id, args)
            }
            Self::Input(track, Some(Input::Tap(tap))) => {
                let (id, args) = on(SET_INPUT, track);
                (id, with_input_tap(args, tap))
            }
            Self::Monitor(track, monitor) => {
                let (id, args) = on(SET_MONITOR, track);
                (id, args.with("monitor", monitor_name(monitor)))
            }
            Self::Gain(track, gain) => {
                let (id, args) = on(SET_VOLUME, track);
                (id, args.with("gain", gain))
            }
            Self::Pan(track, pan) => {
                let (id, args) = on(SET_PAN, track);
                (id, args.with("pan", pan))
            }
            Self::Muted(track, muted) => {
                let (id, args) = on(SET_MUTE, track);
                (id, args.with("muted", muted))
            }
            Self::Solo {
                track,
                solo,
                exclusive,
            } => {
                let (id, args) = on(SET_SOLO, track);
                (id, args.with("solo", solo).with("exclusive", exclusive))
            }
            Self::SoloSafe(track, solo_safe) => {
                let (id, args) = on(SET_SOLO_SAFE, track);
                (id, args.with("solo_safe", solo_safe))
            }
            Self::Armed(track, armed) => {
                let (id, args) = on(SET_ARM, track);
                (id, args.with("armed", armed))
            }
            Self::Output(track, Bus::Master) => on(SET_OUTPUT, track),
            Self::Output(track, Bus::Group(group)) => {
                let (id, args) = on(SET_OUTPUT, track);
                (id, args.with("group", group))
            }
            Self::Output(track, Bus::SendsOnly) => {
                let (id, args) = on(SET_OUTPUT, track);
                (id, args.with("sends_only", true))
            }
            Self::ClipType(track, clip_type) => {
                let (id, args) = on(SET_CLIP_TYPE, track);
                (id, args.with("clip_type", clip_type_name(clip_type)))
            }
            Self::SendLevel { track, to, level } => {
                let (id, args) = on(SET_SEND_LEVEL, track);
                (id, args.with("to", to).with("level", level))
            }
            Self::SendPreFader {
                track,
                to,
                pre_fader,
            } => {
                let (id, args) = on(SET_SEND_PRE_FADER, track);
                (id, args.with("to", to).with("pre_fader", pre_fader))
            }
            Self::RemoveSend { track, to } => {
                let (id, args) = on(REMOVE_SEND, track);
                (id, args.with("to", to))
            }
            Self::Delete(track) => (track::DELETE, Args::new().with("tracks", vec![track])),
            Self::MasterGain(gain) => (SET_MASTER_VOLUME, Args::new().with("gain", gain)),
            Self::MasterPan(pan) => (SET_MASTER_PAN, Args::new().with("pan", pan)),
            Self::MasterClipType(clip_type) => (
                SET_MASTER_CLIP_TYPE,
                Args::new().with("clip_type", clip_type_name(clip_type)),
            ),
        }
    }

    /// What the undo menu calls it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Gain(..) | Self::MasterGain(_) => "Volume",
            Self::Pan(..) | Self::MasterPan(_) => "Pan",
            Self::Muted(_, true) => "Mute",
            Self::Muted(_, false) => "Unmute",
            Self::Solo { solo: true, .. } => "Solo",
            Self::Solo { solo: false, .. } => "Unsolo",
            Self::SoloSafe(..) => "Solo-Safe",
            Self::Armed(_, true) => "Arm",
            Self::Armed(_, false) => "Disarm",
            Self::Input(..) => "Input",
            Self::Monitor(..) => "Monitor",
            Self::Output(..) => "Output",
            Self::ClipType(..) | Self::MasterClipType(_) => "Clip Type",
            Self::SendLevel { .. } => "Send Level",
            Self::SendPreFader { .. } => "Pre/Post",
            Self::RemoveSend { .. } => "Remove Send",
            Self::Delete(_) => "Delete Track",
        }
    }
}

/// Makes `change` to the document's project, as one transaction. Moving an
/// automated fader, pan or send overrides its automation, as moving it by
/// hand does. An input, an output or a new send that would close a loop is
/// refused, with an error naming the input or the wire it would close it
/// through, and changes nothing.
pub fn change(document: &mut Document, change: Change) -> Result<(), CommandError> {
    let by_hand = |tx: &mut Transaction<'_>, param, value: f32| {
        tx.set_param_by_hand(param, f64::from(value));
    };
    let made = document.transact(change.name(), |tx| -> Result<(), Loop> {
        match change {
            Change::Gain(track, gain) => {
                by_hand(tx, ParamAddress::Volume(Chain::Track(track)), gain)
            }
            Change::Pan(track, pan) => by_hand(tx, ParamAddress::Pan(Chain::Track(track)), pan),
            Change::Muted(track, muted) => tx.set_track_muted(track, muted),
            Change::Solo {
                track,
                solo,
                exclusive,
            } => {
                if exclusive {
                    let others: Vec<TrackId> = tx
                        .project()
                        .tracks
                        .iter()
                        .filter(|t| t.id != track)
                        .map(|t| t.id)
                        .collect();
                    for other in others {
                        tx.set_track_solo(other, false);
                    }
                }
                tx.set_track_solo(track, solo);
            }
            Change::SoloSafe(track, solo_safe) => tx.set_track_solo_safe(track, solo_safe),
            Change::Armed(track, armed) => tx.set_track_armed(track, armed),
            Change::Input(track, input) => {
                let input_of =
                    |tx: &Transaction<'_>| tx.project().track(track).map(|t| t.input.clone());
                let before = input_of(tx);
                tx.set_track_input(track, input)?;
                let after = input_of(tx);
                // On Auto, it would be heard only while it's armed.
                let plays = after
                    .as_ref()
                    .and_then(|input| input.as_ref()?.tap())
                    .is_some_and(|tap| !Input::Tap(tap).is_resampling());
                if before != after && plays {
                    tx.set_track_monitor(track, Monitor::In);
                }
            }
            Change::Monitor(track, monitor) => tx.set_track_monitor(track, monitor),
            Change::Output(track, output) => tx.set_track_output(track, output)?,
            Change::ClipType(track, clip_type) => tx.set_track_clip_type(track, clip_type),
            Change::SendLevel { track, to, level } => {
                // A silent send would change nothing but the file.
                let send = match send_to(tx.project(), track, to) {
                    Some(send) => Some(send),
                    None if level.is_finite() && level > 0.0 => add_send(tx, track, to)?,
                    None => None,
                };
                if let Some(send) = send {
                    by_hand(tx, ParamAddress::Send(send), level);
                }
            }
            Change::SendPreFader {
                track,
                to,
                pre_fader,
            } => {
                let send = match send_to(tx.project(), track, to) {
                    Some(send) => Some(send),
                    None => add_send(tx, track, to)?,
                };
                if let Some(send) = send {
                    tx.set_send_pre_fader(track, send, pre_fader);
                }
            }
            Change::RemoveSend { track, to } => {
                if let Some(send) = send_to(tx.project(), track, to) {
                    tx.remove_send(track, send);
                }
            }
            Change::Delete(track) => {
                tx.remove_track(track);
            }
            Change::MasterGain(gain) => by_hand(tx, ParamAddress::Volume(Chain::Master), gain),
            Change::MasterPan(pan) => by_hand(tx, ParamAddress::Pan(Chain::Master), pan),
            Change::MasterClipType(clip_type) => tx.set_master_clip_type(clip_type),
        }
        Ok(())
    });
    made.map_err(|refused| wire::refused(document.project(), refused))
}

/// The id of `track`'s send to the track `to`, if it has one.
fn send_to(project: &Project, track: TrackId, to: TrackId) -> Option<SendId> {
    let track = project.track(track)?;
    track
        .sends
        .iter()
        .find(|send| send.to == to)
        .map(|send| send.id)
}

/// Adds a silent send after the fader from `track` to the return track `to`,
/// if `track` can send there, and returns its id, unless it would close a
/// loop through an input or a wire.
fn add_send(tx: &mut Transaction<'_>, track: TrackId, to: TrackId) -> Result<Option<SendId>, Loop> {
    if !tx.project().can_send(track, to) {
        return Ok(None);
    }
    let send = tx.new_send(to);
    tx.add_send(track, usize::MAX, send)?;
    Ok(Some(send.id))
}

#[cfg(test)]
mod tests {
    use musicable_model::native::{NativeDevice, NativeKind};
    use musicable_model::{DeviceKind, Project, Send, TrackKind};

    use super::*;
    use crate::Edit;
    use crate::testing::Setup;

    /// Drums feeding a group, and a return track.
    fn document() -> (Document, [TrackId; 3]) {
        let mut project = Project::new();
        let drums = project.add_track("Drums", TrackKind::Audio);
        let group = project.add_track("Group", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        project.track_mut(drums).unwrap().output = Bus::Group(group);
        (Document::new(project), [drums, group, reverb])
    }

    #[test]
    fn each_change_is_one_named_transaction() {
        let (mut doc, [drums, _, _]) = document();
        change(&mut doc, Change::Gain(drums, 0.5)).unwrap();
        change(&mut doc, Change::Pan(drums, -2.0)).unwrap();
        change(&mut doc, Change::Muted(drums, true)).unwrap();
        let committed = doc.take_committed();
        let names: Vec<&str> = committed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Volume", "Pan", "Mute"]);
        assert_eq!(
            committed[1].edits,
            [Edit::TrackPan {
                track: drums,
                from: 0.0,
                to: -1.0
            }]
        );
        let track = doc.project().track(drums).unwrap();
        assert_eq!((track.gain, track.pan, track.muted), (0.5, -1.0, true));
    }

    #[test]
    fn exclusive_solo_unsolos_the_others() {
        let (mut doc, [drums, group, reverb]) = document();
        let solo = |track, exclusive| Change::Solo {
            track,
            solo: true,
            exclusive,
        };
        change(&mut doc, solo(drums, false)).unwrap();
        change(&mut doc, solo(reverb, false)).unwrap();
        change(&mut doc, solo(group, true)).unwrap();
        let soloed: Vec<bool> = doc.project().tracks.iter().map(|t| t.solo).collect();
        assert_eq!(soloed, [false, true, false]);
    }

    #[test]
    fn sends_go_to_return_tracks_once() {
        let (mut doc, [drums, group, reverb]) = document();
        let level = |to, level| Change::SendLevel {
            track: drums,
            to,
            level,
        };
        // A silent send, and one to a track that isn't a return: nothing.
        change(&mut doc, level(reverb, 0.0)).unwrap();
        change(&mut doc, level(group, 0.5)).unwrap();
        assert_eq!(doc.take_committed(), []);
        // The first level adds the send, the second changes it.
        change(&mut doc, level(reverb, 0.5)).unwrap();
        change(&mut doc, level(reverb, 0.25)).unwrap();
        let sends = &doc.project().track(drums).unwrap().sends;
        assert_eq!(sends.len(), 1);
        assert_eq!(
            (sends[0].to, sends[0].level, sends[0].pre_fader),
            (reverb, 0.25, false)
        );
        let send = sends[0];
        assert!(matches!(
            doc.take_committed()[0].edits[..],
            [
                Edit::AddSend { .. },
                Edit::SendLevel {
                    from: 0.0,
                    to: 0.5,
                    ..
                }
            ]
        ));
        let pre_fader = |pre_fader| Change::SendPreFader {
            track: drums,
            to: reverb,
            pre_fader,
        };
        change(&mut doc, pre_fader(true)).unwrap();
        assert!(doc.project().tracks[0].sends[0].pre_fader);
        change(
            &mut doc,
            Change::RemoveSend {
                track: drums,
                to: reverb,
            },
        )
        .unwrap();
        assert!(doc.project().track(drums).unwrap().sends.is_empty());
        let committed = doc.take_committed();
        assert_eq!(committed.len(), 2);
        assert_eq!(
            committed[1].edits,
            [Edit::RemoveSend {
                track: drums,
                index: 0,
                send: Send {
                    pre_fader: true,
                    ..send
                }
            }]
        );
        // Pre/post on a track with no send adds a silent one.
        change(&mut doc, pre_fader(true)).unwrap();
        let sends = &doc.project().track(drums).unwrap().sends;
        assert_eq!((sends[0].level, sends[0].pre_fader), (0.0, true));
    }

    #[test]
    fn outputs_stay_within_the_rules() {
        let (mut doc, [drums, group, reverb]) = document();
        // Into itself, into a track that isn't a group, and a return into a
        // group: all left alone.
        change(&mut doc, Change::Output(group, Bus::Group(group))).unwrap();
        change(&mut doc, Change::Output(group, Bus::Group(drums))).unwrap();
        change(&mut doc, Change::Output(reverb, Bus::Group(group))).unwrap();
        assert_eq!(doc.take_committed(), []);
        change(&mut doc, Change::Output(drums, Bus::Master)).unwrap();
        assert_eq!(doc.project().track(drums).unwrap().output, Bus::Master);
    }

    #[test]
    fn only_audio_and_midi_tracks_arm() {
        let (mut doc, [drums, group, _]) = document();
        change(&mut doc, Change::Armed(group, true)).unwrap();
        change(&mut doc, Change::Armed(drums, true)).unwrap();
        let armed: Vec<bool> = doc.project().tracks.iter().map(|t| t.armed).collect();
        assert_eq!(armed, [true, false, false]);
    }

    #[test]
    fn tracks_take_inputs_of_the_kind_they_record() {
        let mut project = Project::new();
        let vocal = project.add_track("Vocal", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let group = project.add_track("Group", TrackKind::Group);
        let mut setup = crate::testing::Setup::new(project);
        let set = |setup: &mut Setup, track: TrackId, args: Args| {
            setup.run_with(&SET_INPUT, args.with("track", track))
        };
        let input =
            |setup: &Setup, track| setup.document.project().track(track).unwrap().input.clone();
        // The fourth channel of an interface, alone.
        let mono = Args::new()
            .with("device", "rme")
            .with("name", "RME")
            .with("channel", 3_i64)
            .with("stereo", false);
        set(&mut setup, vocal, mono).unwrap();
        let expected = AudioInput {
            device: "rme".into(),
            name: "RME".into(),
            channel: 3,
            stereo: false,
        };
        assert_eq!(input(&setup, vocal), Some(Input::Audio(expected)));
        // The default MIDI input's tenth channel, then all of them.
        set(&mut setup, keys, Args::new().with("channel", 9_i64)).unwrap();
        let tenth = MidiInput {
            channel: Some(9),
            ..MidiInput::default()
        };
        assert_eq!(input(&setup, keys), Some(Input::Midi(tenth)));
        set(&mut setup, keys, Args::new()).unwrap();
        assert_eq!(input(&setup, keys), Some(Input::Midi(MidiInput::default())));
        set(&mut setup, vocal, Args::new().with("none", true)).unwrap();
        assert_eq!(input(&setup, vocal), None);
        // Buses record nothing.
        set(&mut setup, group, Args::new()).unwrap();
        assert_eq!(input(&setup, group), None);
        assert_eq!(
            setup.committed_names(),
            ["Input", "Input", "Input", "Input"]
        );

        let channel = |c: i64| Args::new().with("channel", c);
        assert_eq!(
            set(&mut setup, keys, channel(16)),
            Err(CommandError::invalid(
                "there's no MIDI channel 16: they go from 0 to 15"
            ))
        );
        for c in [-1, i64::from(u16::MAX)] {
            assert_eq!(
                set(&mut setup, vocal, channel(c)),
                Err(CommandError::invalid(format!("there's no channel {c}")))
            );
        }

        let monitor = |setup: &mut Setup, name: &str| {
            let args = Args::new().with("track", vocal).with("monitor", name);
            setup.run_with(&SET_MONITOR, args)
        };
        monitor(&mut setup, " in").unwrap();
        assert_eq!(setup.document.project().tracks[0].monitor, Monitor::In);
        assert_eq!(
            monitor(&mut setup, "Sometimes"),
            Err(CommandError::invalid(
                "there's no monitor mode called Sometimes"
            ))
        );
    }

    #[test]
    fn deleting_a_bus_reroutes_what_fed_it_first() {
        let (mut doc, [drums, group, reverb]) = document();
        let send = Change::SendLevel {
            track: drums,
            to: reverb,
            level: 0.5,
        };
        change(&mut doc, send).unwrap();
        doc.take_committed();
        let send = doc.project().tracks[0].sends[0];
        change(&mut doc, Change::Delete(group)).unwrap();
        change(&mut doc, Change::Delete(reverb)).unwrap();
        let committed = doc.take_committed();
        let project = doc.project();
        assert_eq!(project.tracks.len(), 1);
        assert_eq!(project.tracks[0].output, Bus::Master);
        assert!(project.tracks[0].sends.is_empty());
        assert!(matches!(
            committed[0].edits[..],
            [
                Edit::TrackOutput { track, from: Bus::Group(_), to: Bus::Master },
                Edit::RemoveTrack { index: 1, .. },
            ] if track == drums
        ));
        assert!(matches!(
            committed[1].edits[..],
            [
                Edit::RemoveSend { track, index: 0, send: removed },
                Edit::RemoveTrack { index: 1, .. },
            ] if track == drums && removed == send
        ));
    }

    #[test]
    fn values_a_file_cant_hold_do_nothing() {
        let (mut doc, [drums, _, _]) = document();
        change(&mut doc, Change::Gain(drums, f32::NAN)).unwrap();
        change(&mut doc, Change::Pan(drums, f32::NAN)).unwrap();
        change(&mut doc, Change::MasterGain(f32::INFINITY)).unwrap();
        assert_eq!(doc.take_committed(), []);
    }

    #[test]
    fn each_change_is_a_command() {
        let (doc, [drums, group, reverb]) = document();
        let changes = [
            Change::Gain(drums, 0.5),
            Change::Pan(drums, -0.25),
            Change::Muted(drums, true),
            Change::Solo {
                track: drums,
                solo: true,
                exclusive: true,
            },
            Change::SoloSafe(drums, true),
            Change::Armed(drums, true),
            Change::Input(
                drums,
                Some(Input::Audio(AudioInput {
                    device: "rme".into(),
                    name: "RME".into(),
                    channel: 2,
                    stereo: false,
                })),
            ),
            Change::Input(
                drums,
                Some(Input::Tap(Tap::PostFader(Chain::Track(reverb)))),
            ),
            Change::Input(drums, Some(Input::Tap(Tap::PreFx(reverb)))),
            Change::Input(drums, Some(Input::RESAMPLING)),
            Change::Input(drums, None),
            Change::Input(drums, Some(Input::Audio(AudioInput::default()))),
            Change::Monitor(drums, Monitor::Off),
            Change::Output(drums, Bus::SendsOnly),
            Change::Output(drums, Bus::Master),
            Change::ClipType(group, ClipType::Soft),
            Change::SendLevel {
                track: drums,
                to: reverb,
                level: 0.5,
            },
            Change::SendPreFader {
                track: drums,
                to: reverb,
                pre_fader: true,
            },
            Change::RemoveSend {
                track: drums,
                to: reverb,
            },
            Change::MasterGain(0.5),
            Change::MasterPan(0.5),
            Change::MasterClipType(ClipType::Hard),
            Change::Delete(group),
        ];
        let mut setup = crate::testing::Setup::new(doc.project().clone());
        let mut expected = Document::new(doc.project().clone());
        for change in changes {
            let (id, args) = change.command();
            assert!(!args.is_empty(), "{id}");
            setup.run_with(&id, args).unwrap();
            super::change(&mut expected, change.clone()).unwrap();
            assert_eq!(setup.document.project(), expected.project(), "{change:?}");
        }
        let names = setup.committed_names();
        let expected: Vec<_> = expected
            .take_committed()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, expected);
        // They're for the mixer's controls, scripts and agents, not keys.
        for (id, ..) in COMMANDS {
            assert!(setup.registry.get(&id).unwrap().needs_arguments(), "{id}");
        }
        let wrong = Args::new().with("clip_type", "Warm");
        assert_eq!(
            setup.run_with(&SET_MASTER_CLIP_TYPE, wrong),
            Err(CommandError::invalid("there's no clip type called Warm"))
        );
    }

    /// A kick with an EQ feeding a group, a print, a MIDI track and a
    /// return track, and the EQ's id.
    fn printing() -> (Setup, [TrackId; 5], DeviceId) {
        let mut project = Project::new();
        let kick = project.add_track("Kick", TrackKind::Audio);
        let print = project.add_track("Print", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let group = project.add_track("Group", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        let eq = project.new_device(DeviceKind::Native(NativeDevice::new(NativeKind::Eq)));
        let eq_id = eq.id;
        let track = project.track_mut(kick).unwrap();
        track.devices.push(eq);
        track.output = Bus::Group(group);
        (
            Setup::new(project),
            [kick, print, keys, group, reverb],
            eq_id,
        )
    }

    fn set_input(setup: &mut Setup, track: TrackId, args: Args) -> Result<Value, CommandError> {
        setup.run_with(&SET_INPUT, args.with("track", track))
    }

    fn track(setup: &Setup, track: TrackId) -> &musicable_model::Track {
        setup.document.project().track(track).unwrap()
    }

    #[test]
    fn audio_tracks_take_other_tracks_taps_and_monitor_them() {
        let (mut setup, [kick, print, _, group, _], eq) = printing();
        // Post FX without a tap, which the print then monitors.
        set_input(&mut setup, print, Args::new().with("from", kick)).unwrap();
        let taken = track(&setup, print);
        let post_fx = Tap::PostFx(Chain::Track(kick));
        assert_eq!(taken.input, Some(Input::Tap(post_fx)));
        assert_eq!(taken.monitor, Monitor::In);
        let monitor = Args::new().with("track", print).with("monitor", "Off");
        setup.run_with(&SET_MONITOR, monitor).unwrap();
        let from = |track: TrackId, tap: &str| Args::new().with("from", track).with("tap", tap);
        for (args, tap) in [
            (from(kick, "pre_fx"), Tap::PreFx(kick)),
            (from(kick, "post_fader"), Tap::PostFader(Chain::Track(kick))),
            (Args::new().with("after", eq), Tap::Device(eq)),
            (
                from(group, "post_fader"),
                Tap::PostFader(Chain::Track(group)),
            ),
        ] {
            set_input(&mut setup, print, args).unwrap();
            assert_eq!(track(&setup, print).input, Some(Input::Tap(tap)));
            assert_eq!(track(&setup, print).monitor, Monitor::In, "{tap:?}");
        }
        // Resampling leaves the monitoring alone: the track never plays it.
        let monitor = Args::new().with("track", print).with("monitor", "Auto");
        setup.run_with(&SET_MONITOR, monitor).unwrap();
        let resampling = Args::new().with("resampling", true);
        set_input(&mut setup, print, resampling).unwrap();
        assert_eq!(track(&setup, print).input, Some(Input::RESAMPLING));
        assert_eq!(track(&setup, print).monitor, Monitor::Auto);
        // A tap and the monitoring it sets are one step.
        let names = setup.committed_names();
        assert_eq!(names.iter().filter(|name| *name == "Input").count(), 6);
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn midi_tracks_take_other_midi_tracks_notes_and_monitor_them() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let double = project.add_track("Double", TrackKind::Midi);
        let mut setup = Setup::new(project);
        // Pre FX without a tap: the notes.
        set_input(&mut setup, double, Args::new().with("from", keys)).unwrap();
        let taken = track(&setup, double);
        assert_eq!(taken.input, Some(Input::Tap(Tap::PreFx(keys))));
        assert_eq!(taken.monitor, Monitor::In);
        assert_eq!(setup.committed_names(), ["Input"]);
        let invalid = |message: &str| Err(CommandError::invalid(message));
        let post_fx = Args::new().with("from", keys).with("tap", "post_fx");
        assert_eq!(
            set_input(&mut setup, double, post_fx),
            invalid("Keys Post FX is sound: a MIDI track takes another MIDI track's notes")
        );
        // The Keys taking the notes the Double takes from them would loop.
        assert_eq!(
            set_input(&mut setup, keys, Args::new().with("from", double)),
            invalid("that would make a loop: what comes out of Keys reaches Double Pre FX")
        );
    }

    #[test]
    fn inputs_from_taps_are_refused_where_they_dont_fit() {
        let (mut setup, [kick, print, keys, group, _], _) = printing();
        let invalid = |message: &str| Err(CommandError::invalid(message));
        let from = |track: TrackId| Args::new().with("from", track);
        assert_eq!(
            set_input(&mut setup, keys, from(kick)),
            invalid("Kick Pre FX is sound: a MIDI track takes another MIDI track's notes")
        );
        assert_eq!(
            set_input(&mut setup, group, from(kick)),
            invalid("a group or a return track takes what feeds it, not an input")
        );
        assert_eq!(
            set_input(&mut setup, print, from(keys).with("tap", "pre_fx")),
            invalid("Keys Pre FX is notes: an audio track takes sound")
        );
        // From itself, or round a loop: what the kick feeds.
        assert_eq!(
            set_input(&mut setup, kick, from(kick)),
            invalid("that would make a loop: what comes out of Kick reaches Kick Post FX")
        );
        assert_eq!(
            set_input(&mut setup, kick, from(group).with("tap", "pre_fx")),
            invalid("that would make a loop: what comes out of Kick reaches Group Pre FX")
        );
        let both = from(kick).with("device", "rme");
        assert_eq!(
            set_input(&mut setup, print, both),
            invalid("give an interface's input or another track's tap, not both")
        );
        assert_eq!(
            set_input(&mut setup, print, Args::new().with("tap", "pre_fx")),
            invalid("give the track to take the input from, as from")
        );
        assert_eq!(
            set_input(&mut setup, print, from(kick).with("resampling", true)),
            invalid(
                "give a track to take the input from, a device to take it after, or \
                 resampling, only one"
            )
        );
        assert_eq!(
            set_input(&mut setup, print, from(TrackId(99))),
            invalid("there's no track 99")
        );
        let after = Args::new().with("after", DeviceId(99));
        assert_eq!(
            set_input(&mut setup, print, after),
            invalid("there's no device 99")
        );
        assert_eq!(setup.committed_names(), [] as [&str; 0]);
    }

    #[test]
    fn routes_that_would_loop_through_an_input_are_refused_naming_it() {
        let (mut setup, [kick, print, _, group, reverb], eq) = printing();
        let refused = |through: &str| {
            Err(CommandError::invalid(format!(
                "that would make a loop, through Print's input from {through}"
            )))
        };
        // An output into the group it takes.
        let group_out = Args::new().with("from", group).with("tap", "post_fader");
        set_input(&mut setup, print, group_out).unwrap();
        let output = Args::new().with("track", print).with("group", group);
        let made = setup.run_with(&SET_OUTPUT, output);
        assert_eq!(made, refused("Group Post Fader"));
        // A send to the return it takes.
        let reverb_out = Args::new().with("from", reverb);
        set_input(&mut setup, print, reverb_out).unwrap();
        let send = Args::new()
            .with("track", print)
            .with("to", reverb)
            .with("level", 0.5);
        let made = setup.run_with(&SET_SEND_LEVEL, send);
        assert_eq!(made, refused("Reverb Post FX"));
        // A device it takes, moved onto it.
        set_input(&mut setup, print, Args::new().with("after", eq)).unwrap();
        let moved = Args::new().with("devices", vec![eq]).with("track", print);
        let made = setup.run_with(&crate::device::MOVE, moved);
        assert_eq!(made, refused("Kick after EQ"));
        let project = setup.document.project();
        assert_eq!(
            project.find_device(eq).map(|(chain, ..)| chain),
            Some(Chain::Track(kick))
        );
        assert!(track(&setup, print).sends.is_empty());
        assert_eq!(track(&setup, print).output, Bus::Master);
    }

    #[test]
    fn a_track_goes_to_sends_only_and_back() {
        let (mut setup, [kick, _, _, group, _], _) = printing();
        let sends_only = Args::new().with("track", kick).with("sends_only", true);
        setup.run_with(&SET_OUTPUT, sends_only).unwrap();
        assert_eq!(track(&setup, kick).output, Bus::SendsOnly);
        let both = Args::new()
            .with("track", kick)
            .with("sends_only", true)
            .with("group", group);
        assert_eq!(
            setup.run_with(&SET_OUTPUT, both),
            Err(CommandError::invalid(
                "give a group to feed, or sends_only, not both"
            ))
        );
        setup
            .run_with(&SET_OUTPUT, Args::new().with("track", kick))
            .unwrap();
        assert_eq!(track(&setup, kick).output, Bus::Master);
    }

    #[test]
    fn deleting_what_tracks_take_their_inputs_from_leaves_them_none() {
        let (mut setup, [kick, print, ..], eq) = printing();
        set_input(&mut setup, print, Args::new().with("after", eq)).unwrap();
        setup.document.take_committed();
        let deleted = Args::new().with("device", eq);
        setup.run_with(&crate::device::DELETE, deleted).unwrap();
        assert_eq!(track(&setup, print).input, None);
        let committed = setup.document.take_committed();
        let step = committed.last().unwrap();
        assert!(
            matches!(
                step.edits[..],
                [Edit::TrackInput { track, to: None, .. }, Edit::RemoveDevice { .. }]
                    if track == print
            ),
            "{:?}",
            step.edits
        );
        // Undo brings back the input with the device.
        setup.run_with(&crate::edit::UNDO, Args::new()).unwrap();
        assert_eq!(
            track(&setup, print).input,
            Some(Input::Tap(Tap::Device(eq)))
        );
        // And deleting the track takes the input from it too.
        let kick_out = Args::new().with("from", kick).with("tap", "post_fader");
        set_input(&mut setup, print, kick_out).unwrap();
        let deleted = Args::new().with("tracks", vec![kick]);
        setup.run_with(&crate::track::DELETE, deleted).unwrap();
        assert_eq!(track(&setup, print).input, None);
    }
}
