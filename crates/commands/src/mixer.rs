//! What the mixer's controls do: faders, pan, mute, solo, arm, inputs,
//! monitoring, routing, sends and clip types, for each track and the
//! master.
//!
//! A control sends a [`Change`], and the app runs the command it names (see
//! [`Change::command`]), which makes it as a transaction with [`change`].
//! Scripts and agents run the same commands.

use musicable_model::mixer::Bus;
use musicable_model::param::ParamAddress;
use musicable_model::{
    AudioInput, Chain, ClipType, Input, MidiInput, Monitor, Project, SendId, TrackId, TrackKind,
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
/// Sets what a track records from.
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
        ],
        |args, _| {
            let bus = args.opt("group")?.map_or(Bus::Master, Bus::Group);
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
/// records, or none.
fn input(args: &Args, project: &Project) -> Result<Option<Input>, CommandError> {
    if args.opt("none")?.unwrap_or(false) {
        return Ok(None);
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
    /// alone: see [`Transaction::set_track_input`].
    Input(TrackId, Option<Input>),
    Monitor(TrackId, Monitor),
    /// Where the track's output goes. Buses the track can't feed are left
    /// alone: see [`Project::can_feed`](musicable_model::Project::can_feed).
    /// One that would close a loop through a wire is refused.
    Output(TrackId, Bus),
    ClipType(TrackId, ClipType),
    /// The level of `track`'s send to the return track `to`, linear. A
    /// track with no send there gets one, after the fader, unless it would
    /// close a loop through a wire.
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
    /// Deletes the track. Tracks feeding it feed the master instead, and
    /// sends to it go.
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
/// hand does. An output or a new send that would close a loop through a
/// wire is refused, with an error naming the wire, and changes nothing.
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
            Change::Input(track, input) => tx.set_track_input(track, input),
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
/// loop through a wire.
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
    use musicable_model::{Project, Send, TrackKind};

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
            Change::Input(drums, None),
            Change::Input(drums, Some(Input::Audio(AudioInput::default()))),
            Change::Monitor(drums, Monitor::Off),
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
}
