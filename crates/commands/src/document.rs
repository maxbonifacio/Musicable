//! The open project, and the transactions that change it.
//!
//! Commands change the project only through [`Document::transact`]. A
//! transaction records each [`Edit`] it makes along with the value it
//! replaced, so everything that has to follow the project hooks in here: the
//! engine and the undo history now, ARA change notifications in Tier 2.

use musicable_model::mixer::Bus;
use musicable_model::time::{BeatRange, TempoMap};
use musicable_model::{
    ClipType, Device, DeviceKind, Project, Send, SendId, Track, TrackId, TrackKind,
};

use crate::history::History;

/// The open project. It hands out `&Project` only, so every change goes
/// through a transaction, and it keeps the history that undo and redo step
/// through.
#[derive(Debug, Clone, Default)]
pub struct Document {
    project: Project,
    /// Transactions committed since the last [`take_committed`](Self::take_committed).
    committed: Vec<Committed>,
    history: History,
}

impl Document {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            committed: Vec::new(),
            history: History::default(),
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Runs `edit` as one transaction. `name` is what the Edit menu calls
    /// it, as in "Undo Tempo Up". A transaction that changes nothing leaves
    /// no record. Any other becomes a step in the undo history, or joins the
    /// last one while a [gesture](Self::begin_gesture) is open.
    pub fn transact<R>(&mut self, name: &str, edit: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
        let mut transaction = Transaction::new(&mut self.project);
        let result = edit(&mut transaction);
        let edits = transaction.edits;
        if !edits.is_empty() {
            self.history.record(name, &edits);
            self.committed.push(Committed {
                name: name.to_owned(),
                edits,
            });
        }
        result
    }

    /// Takes back the last step in the undo history, by setting what it
    /// changed back the way it was. Returns `false` if there's nothing to
    /// undo.
    ///
    /// The change is committed like any other, under the name "Undo" and
    /// the step's, so the engine follows it. Only redo can make the step
    /// again, until a new transaction forgets it.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.history.undo() else {
            return false;
        };
        let mut transaction = Transaction::new(&mut self.project);
        for edit in step.edits.iter().rev() {
            transaction.apply(&edit.inverse());
        }
        let edits = transaction.edits;
        let name = format!("Undo {}", step.name);
        self.commit(name, edits);
        true
    }

    /// Makes the last step that was undone again. Returns `false` if there's
    /// nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(step) = self.history.redo() else {
            return false;
        };
        let mut transaction = Transaction::new(&mut self.project);
        for edit in &step.edits {
            transaction.apply(edit);
        }
        let edits = transaction.edits;
        let name = format!("Redo {}", step.name);
        self.commit(name, edits);
        true
    }

    /// The name of the step [`undo`](Self::undo) would take back, for the
    /// Edit menu, or `None` if there's nothing to undo.
    pub fn undo_name(&self) -> Option<&str> {
        self.history.undo_name()
    }

    /// The name of the step [`redo`](Self::redo) would make again, or `None`.
    pub fn redo_name(&self) -> Option<&str> {
        self.history.redo_name()
    }

    /// Starts a gesture, such as dragging a fader or the tempo field: until
    /// [`end_gesture`](Self::end_gesture), transactions with the same name
    /// join the gesture's step in the undo history, so one undo takes the
    /// whole drag back. A gesture that ends where it started leaves no step.
    /// Undo and redo end the gesture.
    pub fn begin_gesture(&mut self) {
        self.history.begin_gesture();
    }

    pub fn end_gesture(&mut self) {
        self.history.end_gesture();
    }

    /// The transactions committed since the last call, oldest first, for
    /// whatever keeps itself in step with the project. The app sends the
    /// engine what they changed. Undo and redo are in here too.
    pub fn take_committed(&mut self) -> Vec<Committed> {
        std::mem::take(&mut self.committed)
    }

    /// Whether the project changed since it was opened or last saved. Undo
    /// back to where it was saved, and it hasn't.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    fn commit(&mut self, name: String, edits: Vec<Edit>) {
        if !edits.is_empty() {
            self.committed.push(Committed { name, edits });
        }
    }
}

/// Changes to the project, recorded as they're made. [`Document::transact`]
/// hands one out.
///
/// There's a setter for each part of the project that commands change. A
/// setter that doesn't change anything records nothing.
#[derive(Debug)]
pub struct Transaction<'a> {
    project: &'a mut Project,
    edits: Vec<Edit>,
}

impl<'a> Transaction<'a> {
    fn new(project: &'a mut Project) -> Self {
        Self {
            project,
            edits: Vec::new(),
        }
    }
}

impl Transaction<'_> {
    /// The project with the transaction's changes so far.
    pub fn project(&self) -> &Project {
        self.project
    }

    pub fn set_tempo(&mut self, tempo: TempoMap) {
        if tempo != self.project.tempo {
            let from = std::mem::replace(&mut self.project.tempo, tempo.clone());
            self.edits.push(Edit::Tempo { from, to: tempo });
        }
    }

    pub fn set_loop_region(&mut self, region: BeatRange) {
        let from = std::mem::replace(&mut self.project.loop_region, region);
        if from != region {
            self.edits.push(Edit::LoopRegion { from, to: region });
        }
    }

    pub fn set_looping(&mut self, looping: bool) {
        let from = std::mem::replace(&mut self.project.looping, looping);
        if from != looping {
            self.edits.push(Edit::Looping { from, to: looping });
        }
    }

    pub fn set_metronome(&mut self, on: bool) {
        let from = std::mem::replace(&mut self.project.metronome, on);
        if from != on {
            self.edits.push(Edit::Metronome { from, to: on });
        }
    }

    /// A new track with the next track id, for [`add_track`](Self::add_track):
    /// see [`Project::new_track`]. Taking an id isn't an edit, since ids
    /// are never used again, even if the track never makes it in.
    pub fn new_track(&mut self, name: impl Into<String>, kind: TrackKind) -> Track {
        self.project.new_track(name, kind)
    }

    /// A new send to the return track `to`, for [`add_send`](Self::add_send):
    /// see [`Project::new_send`].
    pub fn new_send(&mut self, to: TrackId) -> Send {
        self.project.new_send(to)
    }

    /// A new device, with the next device id: see [`Project::new_device`].
    pub fn new_device(&mut self, kind: DeviceKind) -> Device {
        self.project.new_device(kind)
    }

    /// Puts `track` at `index` in the track list, or at the end if `index`
    /// is past it. A track with the id of one the project has is left out.
    pub fn add_track(&mut self, index: usize, track: Track) {
        if self.project.track(track.id).is_some() {
            return;
        }
        let index = index.min(self.project.tracks.len());
        self.project.tracks.insert(index, track.clone());
        self.edits.push(Edit::AddTrack { index, track });
    }

    /// Removes the track with id `id` and returns it. What refers to it goes
    /// first, each as an edit of its own: tracks feeding it feed the master
    /// instead, and sends to it go.
    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let index = self.project.track_index(id)?;
        let feeding: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.output == Bus::Group(id))
            .map(|t| t.id)
            .collect();
        for track in feeding {
            self.set_track_output(track, Bus::Master);
        }
        let sends: Vec<(TrackId, SendId)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.sends.iter().filter(|s| s.to == id).map(|s| (t.id, s.id)))
            .collect();
        for (track, send) in sends {
            self.remove_send(track, send);
        }
        let track = self.project.tracks.remove(index);
        self.edits.push(Edit::RemoveTrack {
            index,
            track: track.clone(),
        });
        Some(track)
    }

    pub fn set_track_name(&mut self, track: TrackId, name: &str) {
        if let Some(t) = self.project.track_mut(track)
            && t.name != name
        {
            let from = std::mem::replace(&mut t.name, name.to_owned());
            let to = name.to_owned();
            self.edits.push(Edit::TrackName { track, from, to });
        }
    }

    /// Sets the track's fader. Linear, 1.0 is unity. A gain that isn't a
    /// finite number does nothing, since a project file can't hold it.
    pub fn set_track_gain(&mut self, track: TrackId, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.gain, gain) {
            self.edits.push(Edit::TrackGain {
                track,
                from,
                to: gain,
            });
        }
    }

    /// Sets the track's balance, from -1.0 (left) to 1.0 (right). Values
    /// beyond move to the nearest end, and NaN does nothing.
    pub fn set_track_pan(&mut self, track: TrackId, pan: f32) {
        if pan.is_nan() {
            return;
        }
        let pan = pan.clamp(-1.0, 1.0);
        if let Some(from) = self.replace(track, |t| &mut t.pan, pan) {
            self.edits.push(Edit::TrackPan {
                track,
                from,
                to: pan,
            });
        }
    }

    pub fn set_track_muted(&mut self, track: TrackId, muted: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.muted, muted) {
            self.edits.push(Edit::TrackMuted {
                track,
                from,
                to: muted,
            });
        }
    }

    pub fn set_track_solo(&mut self, track: TrackId, solo: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.solo, solo) {
            self.edits.push(Edit::TrackSolo {
                track,
                from,
                to: solo,
            });
        }
    }

    pub fn set_track_solo_safe(&mut self, track: TrackId, solo_safe: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.solo_safe, solo_safe) {
            self.edits.push(Edit::TrackSoloSafe {
                track,
                from,
                to: solo_safe,
            });
        }
    }

    /// Arms the track for recording, or disarms it. Only audio and MIDI
    /// tracks record, so arming any other track does nothing.
    pub fn set_track_armed(&mut self, track: TrackId, armed: bool) {
        let recordable = self.project.track(track).is_some_and(|t| !t.kind.is_bus());
        if !recordable && armed {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.armed, armed) {
            self.edits.push(Edit::TrackArmed {
                track,
                from,
                to: armed,
            });
        }
    }

    /// Feeds the track's output to `output`, if the track can feed it: see
    /// [`Project::can_feed`]. Otherwise it does nothing.
    pub fn set_track_output(&mut self, track: TrackId, output: Bus) {
        if !self.project.can_feed(track, output) {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.output, output) {
            self.edits.push(Edit::TrackOutput {
                track,
                from,
                to: output,
            });
        }
    }

    pub fn set_track_clip_type(&mut self, track: TrackId, clip_type: ClipType) {
        if let Some(from) = self.replace(track, |t| &mut t.clip_type, clip_type) {
            self.edits.push(Edit::TrackClipType {
                track,
                from,
                to: clip_type,
            });
        }
    }

    /// Puts `send` at `index` in the track's sends, or at the end if `index`
    /// is past them, if the track can send to the send's return track: see
    /// [`Project::can_send`]. Otherwise it does nothing.
    pub fn add_send(&mut self, track: TrackId, index: usize, send: Send) {
        if !self.project.can_send(track, send.to) {
            return;
        }
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let index = index.min(t.sends.len());
        t.sends.insert(index, send);
        self.edits.push(Edit::AddSend { track, index, send });
    }

    pub fn remove_send(&mut self, track: TrackId, send: SendId) {
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        if let Some(index) = t.sends.iter().position(|s| s.id == send) {
            let send = t.sends.remove(index);
            self.edits.push(Edit::RemoveSend { track, index, send });
        }
    }

    /// Sets a send's level. Linear, 1.0 is unity. A level that isn't a
    /// finite number does nothing.
    pub fn set_send_level(&mut self, track: TrackId, send: SendId, level: f32) {
        if !level.is_finite() {
            return;
        }
        if let Some(from) = self.replace_send(track, send, |s| &mut s.level, level) {
            self.edits.push(Edit::SendLevel {
                track,
                send,
                from,
                to: level,
            });
        }
    }

    pub fn set_send_pre_fader(&mut self, track: TrackId, send: SendId, pre_fader: bool) {
        if let Some(from) = self.replace_send(track, send, |s| &mut s.pre_fader, pre_fader) {
            self.edits.push(Edit::SendPreFader {
                track,
                send,
                from,
                to: pre_fader,
            });
        }
    }

    /// Sets the master's fader. Linear, 1.0 is unity. A gain that isn't a
    /// finite number does nothing.
    pub fn set_master_gain(&mut self, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        let from = std::mem::replace(&mut self.project.master.gain, gain);
        if from != gain {
            self.edits.push(Edit::MasterGain { from, to: gain });
        }
    }

    /// Sets the master's balance, as for a track.
    pub fn set_master_pan(&mut self, pan: f32) {
        if pan.is_nan() {
            return;
        }
        let pan = pan.clamp(-1.0, 1.0);
        let from = std::mem::replace(&mut self.project.master.pan, pan);
        if from != pan {
            self.edits.push(Edit::MasterPan { from, to: pan });
        }
    }

    pub fn set_master_clip_type(&mut self, clip_type: ClipType) {
        let from = std::mem::replace(&mut self.project.master.clip_type, clip_type);
        if from != clip_type {
            self.edits.push(Edit::MasterClipType {
                from,
                to: clip_type,
            });
        }
    }

    /// Sets the field `field` picks of the track with id `track` to `value`,
    /// and returns what it was if that's different.
    fn replace<T: PartialEq>(
        &mut self,
        track: TrackId,
        field: impl FnOnce(&mut Track) -> &mut T,
        value: T,
    ) -> Option<T> {
        let field = field(self.project.track_mut(track)?);
        (*field != value).then(|| std::mem::replace(field, value))
    }

    /// The same for a field of the track's send with id `send`.
    fn replace_send<T: PartialEq>(
        &mut self,
        track: TrackId,
        send: SendId,
        field: impl FnOnce(&mut Send) -> &mut T,
        value: T,
    ) -> Option<T> {
        let track = self.project.track_mut(track)?;
        let send = track.sends.iter_mut().find(|s| s.id == send)?;
        let field = field(send);
        (*field != value).then(|| std::mem::replace(field, value))
    }

    /// Makes the change `edit` describes, through the setter for what it
    /// changes, as undo and redo do: `apply(&edit.inverse())` takes it back.
    pub fn apply(&mut self, edit: &Edit) {
        match edit {
            Edit::Tempo { to, .. } => self.set_tempo(to.clone()),
            Edit::LoopRegion { to, .. } => self.set_loop_region(*to),
            Edit::Looping { to, .. } => self.set_looping(*to),
            Edit::Metronome { to, .. } => self.set_metronome(*to),
            Edit::AddTrack { index, track } => self.add_track(*index, track.clone()),
            // Undo and redo take back or make again what referred to the
            // track as edits of their own, before this one, so there's
            // nothing left to reroute.
            Edit::RemoveTrack { track, .. } => {
                self.remove_track(track.id);
            }
            Edit::TrackName { track, to, .. } => self.set_track_name(*track, to),
            Edit::TrackGain { track, to, .. } => self.set_track_gain(*track, *to),
            Edit::TrackPan { track, to, .. } => self.set_track_pan(*track, *to),
            Edit::TrackMuted { track, to, .. } => self.set_track_muted(*track, *to),
            Edit::TrackSolo { track, to, .. } => self.set_track_solo(*track, *to),
            Edit::TrackSoloSafe { track, to, .. } => self.set_track_solo_safe(*track, *to),
            Edit::TrackArmed { track, to, .. } => self.set_track_armed(*track, *to),
            Edit::TrackOutput { track, to, .. } => self.set_track_output(*track, *to),
            Edit::TrackClipType { track, to, .. } => self.set_track_clip_type(*track, *to),
            Edit::AddSend { track, index, send } => self.add_send(*track, *index, *send),
            Edit::RemoveSend { track, send, .. } => self.remove_send(*track, send.id),
            Edit::SendLevel {
                track, send, to, ..
            } => self.set_send_level(*track, *send, *to),
            Edit::SendPreFader {
                track, send, to, ..
            } => self.set_send_pre_fader(*track, *send, *to),
            Edit::MasterGain { to, .. } => self.set_master_gain(*to),
            Edit::MasterPan { to, .. } => self.set_master_pan(*to),
            Edit::MasterClipType { to, .. } => self.set_master_clip_type(*to),
        }
    }
}

/// One change to the project, with the value it replaced.
///
/// A new kind of edit needs a setter on [`Transaction`], and an arm in each
/// match below, which say how to make it, take it back and merge two of
/// them. The undo tests need an example of it too.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Tempo {
        from: TempoMap,
        to: TempoMap,
    },
    LoopRegion {
        from: BeatRange,
        to: BeatRange,
    },
    Looping {
        from: bool,
        to: bool,
    },
    Metronome {
        from: bool,
        to: bool,
    },
    /// `track` went in at `index` in the track list.
    AddTrack {
        index: usize,
        track: Track,
    },
    /// `track`, which was at `index` in the track list, went.
    RemoveTrack {
        index: usize,
        track: Track,
    },
    TrackName {
        track: TrackId,
        from: String,
        to: String,
    },
    TrackGain {
        track: TrackId,
        from: f32,
        to: f32,
    },
    TrackPan {
        track: TrackId,
        from: f32,
        to: f32,
    },
    TrackMuted {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackSolo {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackSoloSafe {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackArmed {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackOutput {
        track: TrackId,
        from: Bus,
        to: Bus,
    },
    TrackClipType {
        track: TrackId,
        from: ClipType,
        to: ClipType,
    },
    /// `send` went in at `index` in the sends of the track with id `track`.
    AddSend {
        track: TrackId,
        index: usize,
        send: Send,
    },
    /// `send`, which was at `index` in the track's sends, went.
    RemoveSend {
        track: TrackId,
        index: usize,
        send: Send,
    },
    SendLevel {
        track: TrackId,
        send: SendId,
        from: f32,
        to: f32,
    },
    SendPreFader {
        track: TrackId,
        send: SendId,
        from: bool,
        to: bool,
    },
    MasterGain {
        from: f32,
        to: f32,
    },
    MasterPan {
        from: f32,
        to: f32,
    },
    MasterClipType {
        from: ClipType,
        to: ClipType,
    },
}

impl Edit {
    /// The edit that takes this one back.
    pub fn inverse(&self) -> Edit {
        match self.clone() {
            Edit::Tempo { from, to } => Edit::Tempo { from: to, to: from },
            Edit::LoopRegion { from, to } => Edit::LoopRegion { from: to, to: from },
            Edit::Looping { from, to } => Edit::Looping { from: to, to: from },
            Edit::Metronome { from, to } => Edit::Metronome { from: to, to: from },
            Edit::AddTrack { index, track } => Edit::RemoveTrack { index, track },
            Edit::RemoveTrack { index, track } => Edit::AddTrack { index, track },
            Edit::TrackName { track, from, to } => Edit::TrackName {
                track,
                from: to,
                to: from,
            },
            Edit::TrackGain { track, from, to } => Edit::TrackGain {
                track,
                from: to,
                to: from,
            },
            Edit::TrackPan { track, from, to } => Edit::TrackPan {
                track,
                from: to,
                to: from,
            },
            Edit::TrackMuted { track, from, to } => Edit::TrackMuted {
                track,
                from: to,
                to: from,
            },
            Edit::TrackSolo { track, from, to } => Edit::TrackSolo {
                track,
                from: to,
                to: from,
            },
            Edit::TrackSoloSafe { track, from, to } => Edit::TrackSoloSafe {
                track,
                from: to,
                to: from,
            },
            Edit::TrackArmed { track, from, to } => Edit::TrackArmed {
                track,
                from: to,
                to: from,
            },
            Edit::TrackOutput { track, from, to } => Edit::TrackOutput {
                track,
                from: to,
                to: from,
            },
            Edit::TrackClipType { track, from, to } => Edit::TrackClipType {
                track,
                from: to,
                to: from,
            },
            Edit::AddSend { track, index, send } => Edit::RemoveSend { track, index, send },
            Edit::RemoveSend { track, index, send } => Edit::AddSend { track, index, send },
            Edit::SendLevel {
                track,
                send,
                from,
                to,
            } => Edit::SendLevel {
                track,
                send,
                from: to,
                to: from,
            },
            Edit::SendPreFader {
                track,
                send,
                from,
                to,
            } => Edit::SendPreFader {
                track,
                send,
                from: to,
                to: from,
            },
            Edit::MasterGain { from, to } => Edit::MasterGain { from: to, to: from },
            Edit::MasterPan { from, to } => Edit::MasterPan { from: to, to: from },
            Edit::MasterClipType { from, to } => Edit::MasterClipType { from: to, to: from },
        }
    }

    /// Folds `next`, the edit made right after this one, into it if both
    /// change the same thing. Returns whether it did.
    pub(crate) fn absorb(&mut self, next: &Edit) -> bool {
        match (self, next) {
            (Edit::Tempo { to, .. }, Edit::Tempo { to: next, .. }) => to.clone_from(next),
            (Edit::LoopRegion { to, .. }, Edit::LoopRegion { to: next, .. }) => *to = *next,
            (Edit::Looping { to, .. }, Edit::Looping { to: next, .. }) => *to = *next,
            (Edit::Metronome { to, .. }, Edit::Metronome { to: next, .. }) => *to = *next,
            (
                Edit::TrackName { track, to, .. },
                Edit::TrackName {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => to.clone_from(next),
            (
                Edit::TrackGain { track, to, .. },
                Edit::TrackGain {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackPan { track, to, .. },
                Edit::TrackPan {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackMuted { track, to, .. },
                Edit::TrackMuted {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackSolo { track, to, .. },
                Edit::TrackSolo {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackSoloSafe { track, to, .. },
                Edit::TrackSoloSafe {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackArmed { track, to, .. },
                Edit::TrackArmed {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackOutput { track, to, .. },
                Edit::TrackOutput {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackClipType { track, to, .. },
                Edit::TrackClipType {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::SendLevel {
                    track, send, to, ..
                },
                Edit::SendLevel {
                    track: other,
                    send: other_send,
                    to: next,
                    ..
                },
            ) if track == other && send == other_send => *to = *next,
            (
                Edit::SendPreFader {
                    track, send, to, ..
                },
                Edit::SendPreFader {
                    track: other,
                    send: other_send,
                    to: next,
                    ..
                },
            ) if track == other && send == other_send => *to = *next,
            (Edit::MasterGain { to, .. }, Edit::MasterGain { to: next, .. })
            | (Edit::MasterPan { to, .. }, Edit::MasterPan { to: next, .. }) => *to = *next,
            (Edit::MasterClipType { to, .. }, Edit::MasterClipType { to: next, .. }) => {
                *to = *next;
            }
            // Adding and removing never merge: each is a step of its own.
            (
                Edit::Tempo { .. }
                | Edit::LoopRegion { .. }
                | Edit::Looping { .. }
                | Edit::Metronome { .. }
                | Edit::AddTrack { .. }
                | Edit::RemoveTrack { .. }
                | Edit::TrackName { .. }
                | Edit::TrackGain { .. }
                | Edit::TrackPan { .. }
                | Edit::TrackMuted { .. }
                | Edit::TrackSolo { .. }
                | Edit::TrackSoloSafe { .. }
                | Edit::TrackArmed { .. }
                | Edit::TrackOutput { .. }
                | Edit::TrackClipType { .. }
                | Edit::AddSend { .. }
                | Edit::RemoveSend { .. }
                | Edit::SendLevel { .. }
                | Edit::SendPreFader { .. }
                | Edit::MasterGain { .. }
                | Edit::MasterPan { .. }
                | Edit::MasterClipType { .. },
                _,
            ) => return false,
        }
        true
    }

    /// Whether the edit changes nothing, as when two merged edits cancel out.
    pub(crate) fn is_noop(&self) -> bool {
        match self {
            Edit::Tempo { from, to } => from == to,
            Edit::LoopRegion { from, to } => from == to,
            Edit::Looping { from, to } => from == to,
            Edit::Metronome { from, to } => from == to,
            Edit::AddTrack { .. }
            | Edit::RemoveTrack { .. }
            | Edit::AddSend { .. }
            | Edit::RemoveSend { .. } => false,
            Edit::TrackName { from, to, .. } => from == to,
            Edit::TrackGain { from, to, .. }
            | Edit::TrackPan { from, to, .. }
            | Edit::SendLevel { from, to, .. }
            | Edit::MasterGain { from, to }
            | Edit::MasterPan { from, to } => from == to,
            Edit::TrackMuted { from, to, .. }
            | Edit::TrackSolo { from, to, .. }
            | Edit::TrackSoloSafe { from, to, .. }
            | Edit::TrackArmed { from, to, .. }
            | Edit::SendPreFader { from, to, .. } => from == to,
            Edit::TrackOutput { from, to, .. } => from == to,
            Edit::TrackClipType { from, to, .. } | Edit::MasterClipType { from, to } => from == to,
        }
    }
}

/// A transaction that changed the project.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    /// What the Edit menu calls it. Undo and redo commit theirs as "Undo
    /// Tempo Up" and so on.
    pub name: String,
    /// In the order they were made.
    pub edits: Vec<Edit>,
}

#[cfg(test)]
mod tests {
    use musicable_model::time::Beats;

    use super::*;

    #[test]
    fn transactions_record_their_edits() {
        let mut doc = Document::new(Project::new());
        assert!(!doc.is_modified());
        let region = BeatRange::new(Beats::new(4), Beats::new(8));
        doc.transact("Loop Bar 2", |tx| {
            tx.set_loop_region(region);
            tx.set_looping(true);
            assert!(tx.project().looping);
        });
        assert_eq!(doc.project().loop_region, region);
        assert!(doc.project().looping);
        assert!(doc.is_modified());

        let committed = doc.take_committed();
        assert_eq!(
            committed,
            [Committed {
                name: "Loop Bar 2".into(),
                edits: vec![
                    Edit::LoopRegion {
                        from: Project::new().loop_region,
                        to: region,
                    },
                    Edit::Looping {
                        from: false,
                        to: true,
                    },
                ],
            }]
        );
        assert_eq!(doc.take_committed(), []);
        doc.mark_saved();
        assert!(!doc.is_modified());
    }

    #[test]
    fn a_transaction_that_changes_nothing_leaves_no_record() {
        let mut doc = Document::new(Project::new());
        doc.transact("Nothing", |tx| {
            let tempo = tx.project().tempo.clone();
            tx.set_tempo(tempo);
            tx.set_metronome(false);
        });
        assert_eq!(doc.take_committed(), []);
        assert!(!doc.is_modified());
        assert_eq!(doc.undo_name(), None);
    }

    #[test]
    fn edits_keep_the_values_they_replaced() {
        let mut doc = Document::new(Project::new());
        let mut tempo = TempoMap::default();
        tempo.set_tempo(90.0);
        doc.transact("Tempo", |tx| {
            tx.set_metronome(true);
            tx.set_tempo(tempo.clone());
            tx.set_metronome(false);
        });
        let edits = &doc.take_committed()[0].edits;
        assert_eq!(
            edits,
            &[
                Edit::Metronome {
                    from: false,
                    to: true,
                },
                Edit::Tempo {
                    from: TempoMap::default(),
                    to: tempo,
                },
                Edit::Metronome {
                    from: true,
                    to: false,
                },
            ]
        );
    }
}
