//! What the mixer's controls do: faders, pan, mute, solo, arm, routing,
//! sends and clip types, for each track and the master.
//!
//! Each control sets a value, so these aren't commands, which don't take
//! values yet: a control sends a [`Change`], and [`change`] makes it as a
//! transaction, like [`transport::set_tempo`](crate::transport::set_tempo)
//! for the tempo field.

use musicable_model::mixer::Bus;
use musicable_model::{ClipType, SendId, TrackId};

use crate::{Document, Transaction};

/// A change one of the mixer's controls makes.
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// Where the track's output goes. Buses the track can't feed are left
    /// alone: see [`Project::can_feed`](musicable_model::Project::can_feed).
    Output(TrackId, Bus),
    ClipType(TrackId, ClipType),
    /// The level of `track`'s send to the return track `to`, linear. A
    /// track with no send there gets one, after the fader.
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
            Self::Output(..) => "Output",
            Self::ClipType(..) | Self::MasterClipType(_) => "Clip Type",
            Self::SendLevel { .. } => "Send Level",
            Self::SendPreFader { .. } => "Pre/Post",
            Self::RemoveSend { .. } => "Remove Send",
            Self::Delete(_) => "Delete Track",
        }
    }
}

/// Makes `change` to the document's project, as one transaction.
pub fn change(document: &mut Document, change: Change) {
    document.transact(change.name(), |tx| match change {
        Change::Gain(track, gain) => tx.set_track_gain(track, gain),
        Change::Pan(track, pan) => tx.set_track_pan(track, pan),
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
        Change::Output(track, output) => tx.set_track_output(track, output),
        Change::ClipType(track, clip_type) => tx.set_track_clip_type(track, clip_type),
        Change::SendLevel { track, to, level } => {
            // A silent send would change nothing but the file.
            let send = match send_to(tx.project(), track, to) {
                Some(send) => Some(send),
                None if level.is_finite() && level > 0.0 => add_send(tx, track, to),
                None => None,
            };
            if let Some(send) = send {
                tx.set_send_level(track, send, level);
            }
        }
        Change::SendPreFader {
            track,
            to,
            pre_fader,
        } => {
            let send = send_to(tx.project(), track, to).or_else(|| add_send(tx, track, to));
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
        Change::MasterGain(gain) => tx.set_master_gain(gain),
        Change::MasterPan(pan) => tx.set_master_pan(pan),
        Change::MasterClipType(clip_type) => tx.set_master_clip_type(clip_type),
    });
}

/// The id of `track`'s send to the track `to`, if it has one.
fn send_to(project: &musicable_model::Project, track: TrackId, to: TrackId) -> Option<SendId> {
    let track = project.track(track)?;
    track
        .sends
        .iter()
        .find(|send| send.to == to)
        .map(|send| send.id)
}

/// Adds a silent send after the fader from `track` to the return track `to`,
/// if `track` can send there, and returns its id.
fn add_send(tx: &mut Transaction<'_>, track: TrackId, to: TrackId) -> Option<SendId> {
    if !tx.project().can_send(track, to) {
        return None;
    }
    let send = tx.new_send(to);
    tx.add_send(track, usize::MAX, send);
    Some(send.id)
}

#[cfg(test)]
mod tests {
    use musicable_model::{Project, Send, TrackKind};

    use super::*;
    use crate::Edit;

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
        change(&mut doc, Change::Gain(drums, 0.5));
        change(&mut doc, Change::Pan(drums, -2.0));
        change(&mut doc, Change::Muted(drums, true));
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
        change(&mut doc, solo(drums, false));
        change(&mut doc, solo(reverb, false));
        change(&mut doc, solo(group, true));
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
        change(&mut doc, level(reverb, 0.0));
        change(&mut doc, level(group, 0.5));
        assert_eq!(doc.take_committed(), []);
        // The first level adds the send, the second changes it.
        change(&mut doc, level(reverb, 0.5));
        change(&mut doc, level(reverb, 0.25));
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
        change(&mut doc, pre_fader(true));
        assert!(doc.project().tracks[0].sends[0].pre_fader);
        change(
            &mut doc,
            Change::RemoveSend {
                track: drums,
                to: reverb,
            },
        );
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
        change(&mut doc, pre_fader(true));
        let sends = &doc.project().track(drums).unwrap().sends;
        assert_eq!((sends[0].level, sends[0].pre_fader), (0.0, true));
    }

    #[test]
    fn outputs_stay_within_the_rules() {
        let (mut doc, [drums, group, reverb]) = document();
        // Into itself, into a track that isn't a group, and a return into a
        // group: all left alone.
        change(&mut doc, Change::Output(group, Bus::Group(group)));
        change(&mut doc, Change::Output(group, Bus::Group(drums)));
        change(&mut doc, Change::Output(reverb, Bus::Group(group)));
        assert_eq!(doc.take_committed(), []);
        change(&mut doc, Change::Output(drums, Bus::Master));
        assert_eq!(doc.project().track(drums).unwrap().output, Bus::Master);
    }

    #[test]
    fn only_audio_and_midi_tracks_arm() {
        let (mut doc, [drums, group, _]) = document();
        change(&mut doc, Change::Armed(group, true));
        change(&mut doc, Change::Armed(drums, true));
        let armed: Vec<bool> = doc.project().tracks.iter().map(|t| t.armed).collect();
        assert_eq!(armed, [true, false, false]);
    }

    #[test]
    fn deleting_a_bus_reroutes_what_fed_it_first() {
        let (mut doc, [drums, group, reverb]) = document();
        let send = Change::SendLevel {
            track: drums,
            to: reverb,
            level: 0.5,
        };
        change(&mut doc, send);
        doc.take_committed();
        let send = doc.project().tracks[0].sends[0];
        change(&mut doc, Change::Delete(group));
        change(&mut doc, Change::Delete(reverb));
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
        change(&mut doc, Change::Gain(drums, f32::NAN));
        change(&mut doc, Change::Pan(drums, f32::NAN));
        change(&mut doc, Change::MasterGain(f32::INFINITY));
        assert_eq!(doc.take_committed(), []);
    }
}
