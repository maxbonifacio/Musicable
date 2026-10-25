//! The undo history that [`Document::undo`](crate::Document::undo) and
//! [`Document::redo`](crate::Document::redo) step through, and the command
//! that jumps through it, for the History panel.
//!
//! Each committed transaction is a step, with its edits. Undo applies a
//! step's edits backwards, each one's [`inverse`](Edit::inverse), and redo
//! applies them again, both as transactions of their own. A gesture, such as
//! dragging a fader, folds its transactions into one step.
//!
//! [`Steps`] lists the steps by name, and [`GO_TO`] takes the project back
//! or forward to one, undoing or redoing those in between one at a time
//! ([`Document::go_to`](crate::Document::go_to)).

use std::collections::VecDeque;

use crate::{Command, CommandError, CommandId, Edit, Param, Registry, Type, Value};

/// Takes the project back or forward to a step in the undo history, as
/// clicking it in the History panel does.
pub const GO_TO: CommandId = CommandId::new("history.go_to");

/// How many steps undo can go back. Beyond that, the oldest go.
pub(crate) const LIMIT: usize = 1000;

/// Registers the history's command.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(GO_TO, "Go to Step")
            .alone()
            .with_params(&[Param::required(
                "step",
                Type::Int,
                "The step, numbered from 1 for the oldest the history keeps: it and those \
             before it are done, and those after it undone. 0 undoes every step.",
            )]),
        |ctx, args| {
            let step: i64 = args.get("step")?;
            let steps = ctx.document.steps().len();
            let done = usize::try_from(step)
                .ok()
                .filter(|&done| done <= steps)
                .ok_or_else(|| {
                    CommandError::invalid(format!("the history has no step {step}: it has {steps}"))
                })?;
            ctx.document.go_to(done);
            Ok(Value::None)
        },
    );
}

#[derive(Debug, Clone, Default)]
pub(crate) struct History {
    /// What undo takes back, oldest first.
    undo: VecDeque<Step>,
    /// What redo makes again, the next one last.
    redo: Vec<Step>,
    /// The gesture in progress.
    gesture: Option<u64>,
    /// The last id handed out, to a step or a gesture.
    last_id: u64,
    /// The id of the newest step that fell off the end of the history, or 0.
    /// Undoing everything that's left gets back to just after it.
    base: u64,
    /// Where the project was opened or last saved, as a [`position`](Self::position).
    saved: u64,
    /// The batch in progress, if one is.
    batch: Option<Batch>,
}

/// A batch of transactions that make one step.
#[derive(Debug, Clone)]
struct Batch {
    name: String,
    /// What the batch's step has for its gesture, which no other step has.
    gesture: u64,
    /// What redo could make again when the batch began, which it gives
    /// back if it makes no step.
    redo: Vec<Step>,
}

/// A step in the history: one transaction, or a gesture's worth.
#[derive(Debug, Clone)]
pub(crate) struct Step {
    /// Unique to the step and what it holds: a transaction joining the step
    /// gives it a new id, since the project it leads to changes.
    id: u64,
    /// The gesture the step was made in.
    gesture: Option<u64>,
    pub(crate) name: String,
    /// In the order they were made.
    pub(crate) edits: Vec<Edit>,
}

impl History {
    /// Records a committed transaction. Within a gesture, a transaction with
    /// the same name as the gesture's step joins it, and the step goes if
    /// its edits cancel out. Within a batch, every transaction joins the
    /// batch's step, under the batch's name. Either way, what was undone
    /// can't be redone any more.
    pub(crate) fn record(&mut self, name: &str, edits: &[Edit]) {
        self.redo.clear();
        let id = self.next_id();
        let (name, gesture) = match &self.batch {
            Some(batch) => (batch.name.as_str(), Some(batch.gesture)),
            None => (name, self.gesture),
        };
        if let Some(step) = self.undo.back_mut()
            && gesture.is_some()
            && step.gesture == gesture
            && step.name == name
        {
            step.edits.extend_from_slice(edits);
            compact(&mut step.edits);
            step.id = id;
            if step.edits.is_empty() {
                self.undo.pop_back();
            }
            return;
        }
        self.undo.push_back(Step {
            id,
            gesture,
            name: name.to_owned(),
            edits: edits.to_vec(),
        });
        // A batch keeps the oldest step until it ends, in case it's
        // cancelled.
        if self.batch.is_none() {
            self.forget_oldest();
        }
    }

    /// Lets the oldest steps go while there are more than the limit.
    fn forget_oldest(&mut self) {
        while self.undo.len() > LIMIT {
            let oldest = self
                .undo
                .pop_front()
                .expect("the history is over its limit");
            self.base = oldest.id;
        }
    }

    pub(crate) fn begin_batch(&mut self, name: &str) {
        let gesture = self.next_id();
        self.batch = Some(Batch {
            name: name.to_owned(),
            gesture,
            redo: std::mem::take(&mut self.redo),
        });
    }

    /// Ends the batch, keeping its step, if it made one.
    pub(crate) fn end_batch(&mut self) {
        let Some(batch) = self.batch.take() else {
            return;
        };
        if !self.made_by(&batch) {
            self.redo = batch.redo;
        }
        self.forget_oldest();
    }

    /// Ends the batch, dropping its step, if it made one, and giving redo
    /// back what it had.
    pub(crate) fn cancel_batch(&mut self) {
        let Some(batch) = self.batch.take() else {
            return;
        };
        if self.made_by(&batch) {
            self.undo.pop_back();
        }
        self.redo = batch.redo;
    }

    /// Whether the last step is `batch`'s.
    fn made_by(&self, batch: &Batch) -> bool {
        self.undo
            .back()
            .is_some_and(|step| step.gesture == Some(batch.gesture))
    }

    /// Moves the last step over to the redo side and returns it, for the
    /// document to apply its edits backwards.
    pub(crate) fn undo(&mut self) -> Option<&Step> {
        self.gesture = None;
        let step = self.undo.pop_back()?;
        self.redo.push(step);
        self.redo.last()
    }

    /// Moves the last step undone back and returns it, for the document to
    /// apply its edits again.
    pub(crate) fn redo(&mut self) -> Option<&Step> {
        self.gesture = None;
        let step = self.redo.pop()?;
        self.undo.push_back(step);
        self.undo.back()
    }

    pub(crate) fn steps(&self) -> Steps<'_> {
        Steps {
            undo: &self.undo,
            redo: &self.redo,
        }
    }

    pub(crate) fn begin_gesture(&mut self) {
        self.gesture = Some(self.next_id());
    }

    pub(crate) fn end_gesture(&mut self) {
        self.gesture = None;
    }

    pub(crate) fn is_modified(&self) -> bool {
        self.position() != self.saved
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = self.position();
    }

    /// Where the history stands: the id of the last step, which names the
    /// project it leads to.
    fn position(&self) -> u64 {
        self.undo.back().map_or(self.base, |step| step.id)
    }

    fn next_id(&mut self) -> u64 {
        self.last_id += 1;
        self.last_id
    }
}

/// The steps in the undo history, by name, for a list of them such as the
/// History panel's: from the oldest, those undo can take back, then those
/// redo can make again, the next one first.
#[derive(Debug, Clone, Copy)]
pub struct Steps<'a> {
    undo: &'a VecDeque<Step>,
    redo: &'a [Step],
}

impl<'a> Steps<'a> {
    /// How many steps there are, undone ones too.
    pub fn len(self) -> usize {
        self.undo.len() + self.redo.len()
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// How many are done: the first this many, from the oldest. Those after
    /// them were undone, and redo can make them again.
    pub fn done(self) -> usize {
        self.undo.len()
    }

    /// The name of the step at `index`, from 0 for the oldest, or `None`
    /// past the newest.
    pub fn name(self, index: usize) -> Option<&'a str> {
        let step = match index.checked_sub(self.undo.len()) {
            None => &self.undo[index],
            Some(undone) => self.redo.iter().rev().nth(undone)?,
        };
        Some(&step.name)
    }

    /// The name of the step undo would take back.
    pub fn undo_name(self) -> Option<&'a str> {
        self.undo.back().map(|step| step.name.as_str())
    }

    /// The name of the step redo would make again.
    pub fn redo_name(self) -> Option<&'a str> {
        self.redo.last().map(|step| step.name.as_str())
    }
}

impl Default for Steps<'_> {
    /// No steps.
    fn default() -> Self {
        static NONE: VecDeque<Step> = VecDeque::new();
        Self {
            undo: &NONE,
            redo: &[],
        }
    }
}

/// Folds each edit into the one before it where both change the same
/// thing, and drops the ones that end up changing nothing, or that the next
/// edit takes back, such as adding a track and removing it again. Only
/// neighbours merge: an edit in between, such as removing a track, could
/// change what the other two mean.
fn compact(edits: &mut Vec<Edit>) {
    let mut compacted: Vec<Edit> = Vec::with_capacity(edits.len());
    for edit in edits.drain(..) {
        let Some(last) = compacted.last_mut() else {
            compacted.push(edit);
            continue;
        };
        if *last == edit.inverse() {
            compacted.pop();
        } else if !last.absorb(&edit) {
            compacted.push(edit);
        } else if last.is_noop() {
            compacted.pop();
        }
    }
    *edits = compacted;
}

#[cfg(test)]
mod tests {
    use musicable_model::automation::{self, Lane};
    use musicable_model::clip::{
        AudioClip, ClipContent, ClipId, Controller, Expression, Fades, MidiClip, Note, NoteId,
        Offset, Point, Timing,
    };
    use musicable_model::mixer::Bus;
    use musicable_model::native::{NativeDevice, NativeKind, sampler};
    use musicable_model::param::ParamAddress;
    use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings, PluginState};
    use musicable_model::time::{BeatRange, Beats, TempoMap};
    use musicable_model::{
        AudioInput, Chain, ClipType, DeviceId, DeviceKind, Input, LocatorId, Monitor, PluginFormat,
        Project, Send, SendId, TakeId, TrackId, TrackKind,
    };
    use proptest::prelude::*;

    use super::*;
    use crate::{Committed, Document, Transaction};

    /// A plugin device's kind, with a state and a parameter set after it.
    fn plugin(instrument: bool) -> DeviceKind {
        DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.plugin".into(),
            name: "Plugin".into(),
            instrument,
            settings: PluginSettings {
                state: Some(PluginState::new(*b"state")),
                params: vec![PluginParam { id: 1, value: 0.5 }],
            },
        })
    }

    /// The project the edits in `one_of_each` change: a track of each kind,
    /// two with a send, and a spare one that nothing refers to; two audio
    /// clips, one with fades, and a MIDI clip with two notes; two takes of
    /// an audio clip on the audio track; two locators; a plugin on the audio
    /// track, a test device on the master and a sampler on the MIDI track;
    /// and lanes that automate the audio track's volume and the master's
    /// pan.
    fn base() -> Project {
        let mut project = Project::new();
        let audio = project.add_track("Audio", TrackKind::Audio);
        let midi = project.add_track("MIDI", TrackKind::Midi);
        let group = project.add_track("Group", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        project.add_track("Spare", TrackKind::Audio);
        for track in [midi, group] {
            let send = project.new_send(reverb);
            project.track_mut(track).unwrap().sends.push(send);
        }
        let range = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
        let mut faded = AudioClip::new("a.wav");
        faded.fade_in = Beats::new(1);
        faded.fade_out = Beats::new(1);
        let mut bass = MidiClip::default();
        bass.add_note(Beats::ZERO, Beats::new(1), 36, 0.75);
        bass.add_note(Beats::new(1), Beats::new(1), 43, 0.5);
        bass.set_controller(Controller::SUSTAIN, vec![Point::new(Beats::ZERO, 1.0)]);
        // As if a third note had been added and deleted.
        bass.new_note_id();
        let clips = [
            (audio, "Faded", range(0, 8), ClipContent::Audio(faded)),
            (midi, "Bass", range(4, 8), ClipContent::Midi(bass)),
            (
                audio,
                "Take",
                range(8, 12),
                ClipContent::Audio(AudioClip::new("b.wav")),
            ),
        ];
        for (track, name, range, content) in clips {
            let clip = project.new_clip(name, range, content);
            project.track_mut(track).unwrap().clips.push(clip);
        }
        for name in ["Take 1", "Take 2"] {
            let mut take = project.new_take(name);
            let recorded = ClipContent::Audio(AudioClip::new("Recordings/Audio 1.wav"));
            take.clips
                .push(project.new_clip("Audio", range(0, 4), recorded));
            project.track_mut(audio).unwrap().takes.push(take);
        }
        for (name, at) in [("Verse", 0), ("Chorus", 16)] {
            let locator = project.new_locator(name, Beats::new(at));
            project.locators.push(locator);
        }
        let effect = project.new_device(plugin(false));
        project.track_mut(audio).unwrap().devices.push(effect);
        let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        project.master.devices.push(gain);
        let sampler = NativeDevice::new(NativeKind::Sampler);
        let sampler = project.new_device(DeviceKind::Native(sampler));
        project.track_mut(midi).unwrap().devices.push(sampler);
        let lanes = [
            (ParamAddress::Volume(Chain::Track(audio)), fade()),
            (ParamAddress::Pan(Chain::Master), vec![point(0, -0.5)]),
        ];
        for (param, points) in lanes {
            project.automation.push(Lane::new(param, points));
        }
        project
    }

    fn point(at: i64, value: f64) -> automation::Point {
        automation::Point::new(Beats::new(at), value)
    }

    /// The points of the lane on the audio track's volume in [`base`].
    fn fade() -> Vec<automation::Point> {
        vec![point(0, 1.0), point(8, 0.5)]
    }

    /// One edit of each kind, each changing the [`base`] project.
    fn one_of_each() -> Vec<Edit> {
        let mut project = base();
        let mut tempo = project.tempo.clone();
        tempo.insert_tempo(Beats::new(8), 140.0);
        let [audio, midi, group, reverb, spare] = [0, 1, 2, 3, 4].map(TrackId);
        let new = project.new_track("New", TrackKind::Midi);
        let send = Send {
            level: 0.5,
            ..project.new_send(reverb)
        };
        let [faded, bass, take] = [0, 1, 2].map(ClipId);
        let range = BeatRange::new(Beats::new(8), Beats::new(12));
        let new_clip = project.new_clip("New", range, ClipContent::Midi(MidiClip::default()));
        let new_locator = project.new_locator("Bridge", Beats::new(32));
        let mut new_take = project.new_take("Take 1");
        let played = ClipContent::Midi(MidiClip::default());
        new_take.clips.push(project.new_clip("MIDI", range, played));
        let (_, bass_clip) = project.find_clip(bass).unwrap();
        let bass_timing = bass_clip.timing();
        let ClipContent::Midi(bass_notes) = &bass_clip.content else {
            unreachable!("Bass is a MIDI clip");
        };
        let removed_note = bass_notes.notes[1].clone();
        let new_note = Note {
            id: NoteId(2),
            start: Beats::new(2),
            length: Beats::new(2),
            key: 48,
            velocity: 1.0,
            release_velocity: Note::RELEASE_VELOCITY,
            expression: Expression {
                pitch_bend: vec![Point::new(Beats::ZERO, 0.0), Point::new(Beats::new(1), 2.0)],
                ..Expression::default()
            },
        };
        let synth = project.new_device(plugin(true));
        let effect = DeviceId(0);
        let kick = DeviceId(2);
        let effect_settings = project
            .find_device(effect)
            .unwrap()
            .2
            .kind
            .plugin()
            .unwrap();
        let effect_settings = effect_settings.settings.clone();
        let edits = vec![
            Edit::Tempo {
                from: project.tempo.clone(),
                to: tempo,
            },
            Edit::LoopRegion {
                from: project.loop_region,
                to: BeatRange::new(Beats::new(4), Beats::new(12)),
            },
            Edit::Looping {
                from: project.looping,
                to: !project.looping,
            },
            Edit::Metronome {
                from: project.metronome,
                to: !project.metronome,
            },
            Edit::PunchIn {
                from: false,
                to: true,
            },
            Edit::PunchOut {
                from: false,
                to: true,
            },
            Edit::AddTrack {
                index: 5,
                track: new,
            },
            Edit::RemoveTrack {
                index: 4,
                track: project.track(spare).unwrap().clone(),
            },
            Edit::TrackName {
                track: audio,
                from: "Audio".into(),
                to: "Lead".into(),
            },
            Edit::TrackGain {
                track: audio,
                from: 1.0,
                to: 0.5,
            },
            Edit::TrackPan {
                track: audio,
                from: 0.0,
                to: -0.5,
            },
            Edit::TrackMuted {
                track: audio,
                from: false,
                to: true,
            },
            Edit::TrackSolo {
                track: audio,
                from: false,
                to: true,
            },
            Edit::TrackSoloSafe {
                track: audio,
                from: false,
                to: true,
            },
            Edit::TrackArmed {
                track: audio,
                from: false,
                to: true,
            },
            Edit::TrackInput {
                track: audio,
                from: Some(Input::Audio(AudioInput::default())),
                to: Some(Input::Audio(AudioInput {
                    device: "mic".into(),
                    name: "Microphone".into(),
                    channel: 2,
                    stereo: false,
                })),
            },
            Edit::TrackMonitor {
                track: audio,
                from: Monitor::Auto,
                to: Monitor::In,
            },
            Edit::TrackOutput {
                track: audio,
                from: Bus::Master,
                to: Bus::Group(group),
            },
            Edit::TrackClipType {
                track: audio,
                from: ClipType::Off,
                to: ClipType::Soft,
            },
            Edit::AddSend {
                track: audio,
                index: 0,
                send,
            },
            Edit::RemoveSend {
                track: group,
                index: 0,
                send: project.track(group).unwrap().sends[0],
            },
            Edit::SendLevel {
                track: midi,
                send: SendId(0),
                from: 0.0,
                to: 0.5,
            },
            Edit::SendPreFader {
                track: midi,
                send: SendId(0),
                from: false,
                to: true,
            },
            Edit::MasterGain { from: 1.0, to: 0.5 },
            Edit::MasterPan {
                from: 0.0,
                to: 0.25,
            },
            Edit::MasterClipType {
                from: ClipType::Off,
                to: ClipType::Hard,
            },
            Edit::AddClip {
                track: midi,
                index: 1,
                clip: new_clip,
            },
            Edit::RemoveClip {
                track: audio,
                index: 1,
                clip: project.find_clip(take).unwrap().1.clone(),
            },
            Edit::ClipTiming {
                track: midi,
                clip: bass,
                from: bass_timing,
                to: Timing {
                    start: Beats::new(2),
                    length: Beats::new(3),
                    offset: Offset::Beats(Beats::new(-1)),
                },
            },
            Edit::ClipFades {
                track: audio,
                clip: faded,
                from: Fades {
                    fade_in: Beats::new(1),
                    fade_out: Beats::new(1),
                },
                to: Fades {
                    fade_in: Beats::new(2),
                    fade_out: Beats::ZERO,
                },
            },
            Edit::ClipName {
                track: midi,
                clip: bass,
                from: "Bass".into(),
                to: "Lead".into(),
            },
            Edit::AddNote {
                track: midi,
                clip: bass,
                index: 2,
                note: new_note,
            },
            Edit::RemoveNote {
                track: midi,
                clip: bass,
                index: 1,
                note: removed_note,
            },
            Edit::NoteTiming {
                track: midi,
                clip: bass,
                note: NoteId(0),
                from: BeatRange::new(Beats::ZERO, Beats::new(1)),
                to: BeatRange::new(Beats::from_ratio(1, 2), Beats::new(3)),
            },
            Edit::NoteKey {
                track: midi,
                clip: bass,
                note: NoteId(0),
                from: 36,
                to: 38,
            },
            Edit::NoteVelocity {
                track: midi,
                clip: bass,
                note: NoteId(0),
                from: 0.75,
                to: 0.25,
            },
            Edit::Controller {
                track: midi,
                clip: bass,
                controller: Controller::SUSTAIN,
                from: vec![Point::new(Beats::ZERO, 1.0)],
                to: vec![Point::new(Beats::ZERO, 1.0), Point::new(Beats::new(1), 0.0)],
            },
            Edit::AddTake {
                track: midi,
                index: 0,
                take: new_take,
            },
            Edit::TakeName {
                track: audio,
                take: TakeId(1),
                from: "Take 2".into(),
                to: "Best".into(),
            },
            Edit::RemoveTake {
                track: audio,
                index: 0,
                take: project.track(audio).unwrap().takes[0].clone(),
            },
            Edit::AddLocator {
                index: 2,
                locator: new_locator,
            },
            Edit::RemoveLocator {
                index: 1,
                locator: project.locators[1].clone(),
            },
            Edit::LocatorAt {
                locator: LocatorId(0),
                from: Beats::ZERO,
                to: Beats::new(4),
            },
            Edit::LocatorName {
                locator: LocatorId(0),
                from: "Verse".into(),
                to: "Intro".into(),
            },
            Edit::AddDevice {
                chain: Chain::Track(midi),
                index: 0,
                device: synth,
            },
            Edit::RemoveDevice {
                chain: Chain::Master,
                index: 0,
                device: project.master.devices[0].clone(),
            },
            Edit::PluginSettings {
                device: effect,
                from: effect_settings,
                to: PluginSettings {
                    state: Some(PluginState::new(*b"preset")),
                    params: Vec::new(),
                },
            },
            Edit::PluginParam {
                device: effect,
                param: 7,
                from: None,
                to: Some(-3.5),
            },
            Edit::NativeParam {
                device: kick,
                param: sampler::ROOT as u32,
                from: 60.0,
                to: 36.0,
            },
            Edit::NativeSample {
                device: kick,
                from: None,
                to: Some("Samples/Kick.wav".into()),
            },
            Edit::LanePoints {
                param: ParamAddress::Volume(Chain::Track(audio)),
                from: fade(),
                to: vec![point(0, 1.0), point(4, 0.25), point(8, 0.5)],
            },
            Edit::LaneOverridden {
                param: ParamAddress::Volume(Chain::Track(audio)),
                from: false,
                to: true,
            },
            Edit::AddLane {
                index: 2,
                lane: Lane::new(ParamAddress::Send(SendId(0)), vec![point(4, 0.25)]),
            },
            Edit::RemoveLane {
                index: 1,
                lane: project.automation[1].clone(),
            },
        ];
        let mut kinds: Vec<_> = edits.iter().map(kind).collect();
        kinds.dedup();
        assert_eq!(kinds.len(), KINDS, "one example of each kind");
        edits
    }

    /// How many kinds of edit there are.
    const KINDS: usize = 54;

    /// Which kind an edit is. A new kind stops this compiling: give it an
    /// example in `one_of_each` and count it in `KINDS`.
    fn kind(edit: &Edit) -> usize {
        match edit {
            Edit::Tempo { .. } => 0,
            Edit::LoopRegion { .. } => 1,
            Edit::Looping { .. } => 2,
            Edit::Metronome { .. } => 3,
            Edit::AddTrack { .. } => 4,
            Edit::RemoveTrack { .. } => 5,
            Edit::TrackName { .. } => 6,
            Edit::TrackGain { .. } => 7,
            Edit::TrackPan { .. } => 8,
            Edit::TrackMuted { .. } => 9,
            Edit::TrackSolo { .. } => 10,
            Edit::TrackSoloSafe { .. } => 11,
            Edit::TrackArmed { .. } => 12,
            Edit::TrackOutput { .. } => 13,
            Edit::TrackClipType { .. } => 14,
            Edit::AddSend { .. } => 15,
            Edit::RemoveSend { .. } => 16,
            Edit::SendLevel { .. } => 17,
            Edit::SendPreFader { .. } => 18,
            Edit::MasterGain { .. } => 19,
            Edit::MasterPan { .. } => 20,
            Edit::MasterClipType { .. } => 21,
            Edit::AddClip { .. } => 22,
            Edit::RemoveClip { .. } => 23,
            Edit::ClipTiming { .. } => 24,
            Edit::ClipFades { .. } => 25,
            Edit::ClipName { .. } => 26,
            Edit::AddLocator { .. } => 27,
            Edit::RemoveLocator { .. } => 28,
            Edit::LocatorAt { .. } => 29,
            Edit::LocatorName { .. } => 30,
            Edit::AddDevice { .. } => 31,
            Edit::RemoveDevice { .. } => 32,
            Edit::PluginSettings { .. } => 33,
            Edit::PluginParam { .. } => 34,
            Edit::PunchIn { .. } => 35,
            Edit::PunchOut { .. } => 36,
            Edit::TrackInput { .. } => 37,
            Edit::TrackMonitor { .. } => 38,
            Edit::AddTake { .. } => 39,
            Edit::RemoveTake { .. } => 40,
            Edit::TakeName { .. } => 41,
            Edit::AddNote { .. } => 42,
            Edit::RemoveNote { .. } => 43,
            Edit::NoteTiming { .. } => 44,
            Edit::NoteKey { .. } => 45,
            Edit::NoteVelocity { .. } => 46,
            Edit::Controller { .. } => 47,
            Edit::NativeParam { .. } => 48,
            Edit::NativeSample { .. } => 49,
            Edit::AddLane { .. } => 50,
            Edit::RemoveLane { .. } => 51,
            Edit::LanePoints { .. } => 52,
            Edit::LaneOverridden { .. } => 53,
        }
    }

    fn apply(doc: &mut Document, name: &str, edit: &Edit) {
        doc.transact(name, |tx: &mut Transaction<'_>| tx.apply(edit));
    }

    #[test]
    fn undo_and_redo_take_back_and_make_again_every_kind_of_edit() {
        for edit in one_of_each() {
            let before = base();
            let mut doc = Document::new(before.clone());
            apply(&mut doc, "Change", &edit);
            assert_eq!(
                doc.take_committed(),
                [Committed {
                    name: "Change".into(),
                    edits: vec![edit.clone()],
                }]
            );
            let after = doc.project().clone();
            assert_ne!(after, before, "{edit:?} changes the project");
            assert_eq!(doc.undo_name(), Some("Change"));

            assert!(doc.undo());
            assert_eq!(doc.project(), &before, "undo {edit:?}");
            assert_eq!(
                doc.take_committed(),
                [Committed {
                    name: "Undo Change".into(),
                    edits: vec![edit.inverse()],
                }]
            );
            assert!(!doc.is_modified());
            assert_eq!((doc.undo_name(), doc.redo_name()), (None, Some("Change")));
            assert!(!doc.undo());

            assert!(doc.redo());
            assert_eq!(doc.project(), &after, "redo {edit:?}");
            assert_eq!(
                doc.take_committed(),
                [Committed {
                    name: "Redo Change".into(),
                    edits: vec![edit.clone()],
                }]
            );
            assert!(doc.is_modified());
            assert!(!doc.redo());
            assert_eq!(edit.inverse().inverse(), edit);
        }
    }

    #[test]
    fn undo_takes_a_transaction_back_whole() {
        let mut doc = Document::new(base());
        let edits = one_of_each();
        doc.transact("Everything", |tx| {
            for edit in &edits {
                tx.apply(edit);
            }
        });
        let after = doc.project().clone();
        doc.take_committed();

        assert!(doc.undo());
        assert_eq!(doc.project(), &base());
        // Backwards, so each edit finds the project as it left it.
        let inverses: Vec<_> = edits.iter().rev().map(Edit::inverse).collect();
        assert_eq!(doc.take_committed()[0].edits, inverses);
        assert!(doc.redo());
        assert_eq!(doc.project(), &after);
    }

    #[test]
    fn a_new_transaction_forgets_what_was_undone() {
        let mut doc = Document::new(Project::new());
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(true));
        doc.transact("Toggle Loop", |tx| tx.set_looping(true));
        assert!(doc.undo());
        assert_eq!(doc.redo_name(), Some("Toggle Loop"));
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(false));
        assert_eq!(doc.redo_name(), None);
        assert!(!doc.redo());
        assert!(doc.undo());
        assert!(doc.undo());
        assert_eq!(doc.project(), &Project::new());
        assert!(!doc.undo());
    }

    #[test]
    fn the_steps_are_listed_by_name_and_go_to_jumps_to_one() {
        let mut doc = Document::new(Project::new());
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(true));
        doc.transact("Toggle Loop", |tx| tx.set_looping(true));
        doc.transact("Toggle Punch In", |tx| tx.set_punch_in(true));
        doc.take_committed();
        let listed = |doc: &Document| -> (Vec<String>, usize) {
            let steps = doc.steps();
            let names = (0..steps.len()).map(|i| steps.name(i).unwrap().to_owned());
            (names.collect(), steps.done())
        };
        let names = ["Toggle Metronome", "Toggle Loop", "Toggle Punch In"].map(String::from);
        assert_eq!(listed(&doc), (names.to_vec(), 3));
        assert_eq!(doc.steps().name(3), None);

        // Back to the first step, undoing the two after it in turn, which
        // stay listed.
        assert!(doc.go_to(1));
        assert!(doc.project().metronome && !doc.project().looping);
        let committed: Vec<_> = doc.take_committed().into_iter().map(|c| c.name).collect();
        assert_eq!(committed, ["Undo Toggle Punch In", "Undo Toggle Loop"]);
        assert_eq!(listed(&doc), (names.to_vec(), 1));
        assert_eq!(doc.redo_name(), Some("Toggle Loop"));
        // Forward to the last, and back to before them all.
        assert!(doc.go_to(3));
        assert!(doc.project().punch_in);
        assert_eq!(doc.take_committed().len(), 2);
        assert!(doc.go_to(0));
        assert_eq!(doc.project(), &Project::new());
        assert!(!doc.is_modified());
        assert_eq!(listed(&doc), (names.to_vec(), 0));
        // Where it is already, nothing happens; past the last, nothing can.
        doc.take_committed();
        assert!(doc.go_to(0));
        assert!(!doc.go_to(4));
        assert!(doc.take_committed().is_empty());
        // A new step after going back forgets the steps undone.
        assert!(doc.go_to(1));
        doc.transact("Toggle Punch Out", |tx| tx.set_punch_out(true));
        let names = ["Toggle Metronome", "Toggle Punch Out"].map(String::from);
        assert_eq!(listed(&doc), (names.to_vec(), 2));
    }

    #[test]
    fn a_gesture_is_one_step() {
        let mut doc = Document::new(Project::new());
        let tempo = |bpm| {
            let mut tempo = TempoMap::default();
            tempo.set_tempo(bpm);
            tempo
        };
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(true));
        doc.begin_gesture();
        for bpm in [121.0, 125.0, 131.0] {
            doc.transact("Set Tempo", |tx| tx.set_tempo(tempo(bpm)));
        }
        doc.end_gesture();
        // After the gesture, the same name is a step of its own.
        doc.transact("Set Tempo", |tx| tx.set_tempo(tempo(90.0)));

        assert!(doc.undo());
        assert_eq!(doc.project().tempo, tempo(131.0));
        assert!(doc.undo());
        assert_eq!(doc.project().tempo, TempoMap::default());
        assert!(doc.project().metronome);
        // The drag's three transactions came back as one edit.
        let undone = doc.take_committed();
        let last = undone.last().unwrap();
        assert_eq!(
            last.edits,
            [Edit::Tempo {
                from: tempo(131.0),
                to: TempoMap::default(),
            }]
        );
        assert!(doc.redo());
        assert_eq!(doc.project().tempo, tempo(131.0));
    }

    #[test]
    fn a_gesture_keeps_other_transactions_apart() {
        let mut doc = Document::new(Project::new());
        doc.begin_gesture();
        doc.transact("Set Tempo", |tx| {
            let mut tempo = TempoMap::default();
            tempo.set_tempo(100.0);
            tx.set_tempo(tempo);
        });
        // A key press in the middle of a drag.
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(true));
        doc.transact("Set Tempo", |tx| tx.set_tempo(TempoMap::default()));
        doc.end_gesture();
        let mut names = Vec::new();
        while let Some(name) = doc.undo_name() {
            names.push(name.to_owned());
            doc.undo();
        }
        assert_eq!(names, ["Set Tempo", "Toggle Metronome", "Set Tempo"]);
        assert_eq!(doc.project(), &Project::new());
    }

    #[test]
    fn a_gesture_that_ends_where_it_started_leaves_no_step() {
        let mut doc = Document::new(Project::new());
        doc.transact("Toggle Loop", |tx| tx.set_looping(true));
        doc.mark_saved();
        doc.begin_gesture();
        let region = |end| BeatRange::new(Beats::ZERO, Beats::new(end));
        for end in [8, 12, 20] {
            doc.transact("Drag Loop", |tx| tx.set_loop_region(region(end)));
            assert!(doc.is_modified());
        }
        doc.transact("Drag Loop", |tx| {
            tx.set_loop_region(Project::new().loop_region)
        });
        doc.end_gesture();
        assert_eq!(doc.undo_name(), Some("Toggle Loop"));
        assert!(!doc.is_modified());
    }

    #[test]
    fn undo_ends_a_gesture() {
        let mut doc = Document::new(Project::new());
        doc.begin_gesture();
        doc.transact("Toggle", |tx| tx.set_metronome(true));
        doc.transact("Toggle", |tx| tx.set_looping(true));
        assert!(doc.undo());
        assert!(doc.redo());
        doc.transact("Toggle", |tx| tx.set_metronome(false));
        assert!(doc.undo());
        assert!(doc.project().metronome);
        assert!(doc.undo());
        assert_eq!(doc.project(), &Project::new());
    }

    #[test]
    fn the_project_is_modified_until_undo_gets_back_to_where_it_was_saved() {
        let mut doc = Document::new(Project::new());
        assert!(!doc.is_modified());
        doc.transact("Toggle Metronome", |tx| tx.set_metronome(true));
        assert!(doc.is_modified());
        doc.undo();
        assert!(!doc.is_modified());
        doc.redo();
        doc.mark_saved();
        doc.transact("Toggle Loop", |tx| tx.set_looping(true));
        assert!(doc.is_modified());
        doc.undo();
        assert!(!doc.is_modified());
        doc.undo();
        assert!(doc.is_modified());
        doc.redo();
        assert!(!doc.is_modified());

        // Saved with steps to redo, then a new step: the saved project can't
        // come back.
        doc.undo();
        doc.mark_saved();
        doc.transact("Toggle Loop", |tx| tx.set_looping(true));
        doc.undo();
        assert_eq!(doc.project(), &Project::new());
        assert!(!doc.is_modified());
        doc.redo();
        assert!(doc.is_modified());
    }

    #[test]
    fn a_gesture_after_saving_marks_the_project_modified() {
        let mut doc = Document::new(Project::new());
        doc.begin_gesture();
        doc.transact("Set Tempo", |tx| {
            let mut tempo = TempoMap::default();
            tempo.set_tempo(100.0);
            tx.set_tempo(tempo);
        });
        // Saved in the middle of the drag.
        doc.mark_saved();
        doc.transact("Set Tempo", |tx| {
            let mut tempo = TempoMap::default();
            tempo.set_tempo(110.0);
            tx.set_tempo(tempo);
        });
        assert!(doc.is_modified());
    }

    #[test]
    fn the_oldest_steps_go_past_the_limit() {
        let mut doc = Document::new(Project::new());
        for i in 0..LIMIT + 5 {
            doc.transact("Toggle Metronome", |tx| tx.set_metronome(i % 2 == 0));
        }
        let mut undone = 0;
        while doc.undo() {
            undone += 1;
        }
        assert_eq!(undone, LIMIT);
        // Back to where the fifth transaction left it, which isn't where the
        // project started.
        assert!(doc.project().metronome);
        assert!(doc.is_modified());
        let steps = doc.steps();
        assert_eq!((steps.len(), steps.done()), (LIMIT, 0));
    }

    /// What a test does to a document.
    #[derive(Debug, Clone)]
    enum Op {
        /// Applies one of `one_of_each`, or its inverse.
        Edit {
            kind: usize,
            inverse: bool,
        },
        Undo,
        Redo,
        /// Goes to a step, picked by the number.
        GoTo(usize),
        BeginGesture,
        EndGesture,
        Save,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..KINDS, any::<bool>()).prop_map(|(kind, inverse)| Op::Edit { kind, inverse }),
            2 => Just(Op::Undo),
            1 => Just(Op::Redo),
            1 => (0..64_usize).prop_map(Op::GoTo),
            1 => Just(Op::BeginGesture),
            1 => Just(Op::EndGesture),
            1 => Just(Op::Save),
        ]
    }

    /// The history as a list of the projects its steps lead to.
    struct Model {
        /// The project before any step, then after each step, with the
        /// step's name, gesture and id.
        projects: Vec<(Project, String, Option<u32>, u32)>,
        /// How many steps are done: `projects[done]` is the project now.
        done: usize,
        gesture: Option<u32>,
        last_id: u32,
        /// The id where the project was saved.
        saved: u32,
    }

    impl Model {
        fn new() -> Self {
            Self {
                projects: vec![(base(), String::new(), None, 0)],
                done: 0,
                gesture: None,
                last_id: 0,
                saved: 0,
            }
        }

        fn next_id(&mut self) -> u32 {
            self.last_id += 1;
            self.last_id
        }

        /// A transaction called `name` changed the project to `after`.
        fn changed(&mut self, name: String, after: Project) {
            self.projects.truncate(self.done + 1);
            let id = self.next_id();
            let (_, top_name, top_gesture, _) = &self.projects[self.done];
            let joins = self.done > 0
                && self.gesture.is_some()
                && *top_gesture == self.gesture
                && *top_name == name;
            if !joins {
                self.projects.push((after, name, self.gesture, id));
                self.done += 1;
            } else if self.projects[self.done - 1].0 == after {
                // Each name changes one thing, so the gesture's step only
                // cancels out when it gets back to where it started.
                self.projects.pop();
                self.done -= 1;
            } else {
                self.projects[self.done] = (after, name, self.gesture, id);
            }
        }

        fn id(&self) -> u32 {
            self.projects[self.done].3
        }
    }

    proptest! {
        // Like the scheduler's random graphs, without a regressions file.
        #![proptest_config(ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn undo_and_redo_step_through_the_projects_the_history_leads_to(
            ops in prop::collection::vec(op(), 1..60),
        ) {
            let edits = one_of_each();
            let mut doc = Document::new(base());
            let mut model = Model::new();
            let mut saved = base();
            // Follows the committed transactions, as the engine does.
            let mut mirror = Document::new(base());
            for op in ops {
                match op {
                    Op::Edit { kind, inverse } => {
                        let edit = if inverse {
                            edits[kind].inverse()
                        } else {
                            edits[kind].clone()
                        };
                        // One name per kind, so a gesture's step only holds
                        // one kind.
                        let name = format!("Edit {kind}");
                        let before = doc.project().clone();
                        apply(&mut doc, &name, &edit);
                        if *doc.project() != before {
                            model.changed(name, doc.project().clone());
                        }
                    }
                    Op::Undo => {
                        prop_assert_eq!(doc.undo(), model.done > 0);
                        model.done = model.done.saturating_sub(1);
                        model.gesture = None;
                    }
                    Op::Redo => {
                        let can_redo = model.done + 1 < model.projects.len();
                        prop_assert_eq!(doc.redo(), can_redo);
                        if can_redo {
                            model.done += 1;
                        }
                        model.gesture = None;
                    }
                    Op::GoTo(step) => {
                        let step = step % model.projects.len();
                        prop_assert!(doc.go_to(step));
                        // Undoing or redoing ends the gesture.
                        if step != model.done {
                            model.gesture = None;
                        }
                        model.done = step;
                    }
                    Op::BeginGesture => {
                        doc.begin_gesture();
                        model.gesture = Some(model.next_id());
                    }
                    Op::EndGesture => {
                        doc.end_gesture();
                        model.gesture = None;
                    }
                    Op::Save => {
                        doc.mark_saved();
                        model.saved = model.id();
                        saved = doc.project().clone();
                    }
                }
                prop_assert_eq!(doc.project(), &model.projects[model.done].0);
                let modified = model.saved != model.id();
                prop_assert_eq!(doc.is_modified(), modified);
                if !modified {
                    prop_assert_eq!(doc.project(), &saved);
                }
                let undo_name = (model.done > 0).then(|| model.projects[model.done].1.as_str());
                prop_assert_eq!(doc.undo_name(), undo_name);
                let redo_name = model.projects.get(model.done + 1).map(|p| p.1.as_str());
                prop_assert_eq!(doc.redo_name(), redo_name);
                let steps = doc.steps();
                let listed: Vec<_> = (0..steps.len()).filter_map(|i| steps.name(i)).collect();
                let made: Vec<_> = model.projects[1..].iter().map(|p| p.1.as_str()).collect();
                prop_assert_eq!(listed, made);
                prop_assert_eq!(steps.done(), model.done);

                // Each committed edit starts from the value the one before
                // it left, so whatever follows them stays in step.
                for committed in doc.take_committed() {
                    mirror.transact("Replay", |tx| {
                        for edit in &committed.edits {
                            tx.apply(edit);
                        }
                    });
                    let replayed = mirror.take_committed();
                    prop_assert_eq!(&replayed[0].edits, &committed.edits);
                }
                prop_assert_eq!(mirror.project(), doc.project());
            }

            // Undoing everything gets back to the start, and redoing it all
            // gets to the newest step.
            while doc.undo() {}
            prop_assert_eq!(doc.project(), &base());
            while doc.redo() {}
            prop_assert_eq!(doc.project(), &model.projects.last().unwrap().0);
        }
    }
}
