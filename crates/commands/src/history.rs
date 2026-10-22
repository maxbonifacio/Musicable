//! The undo history that [`Document::undo`](crate::Document::undo) and
//! [`Document::redo`](crate::Document::redo) step through.
//!
//! Each committed transaction is a step, with its edits. Undo applies a
//! step's edits backwards, each one's [`inverse`](Edit::inverse), and redo
//! applies them again, both as transactions of their own. A gesture, such as
//! dragging a fader, folds its transactions into one step.

use std::collections::VecDeque;

use crate::Edit;

/// How many steps undo can go back. Beyond that, the oldest go.
pub(crate) const LIMIT: usize = 1000;

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
    /// its edits cancel out. Either way, what was undone can't be redone
    /// any more.
    pub(crate) fn record(&mut self, name: &str, edits: &[Edit]) {
        self.redo.clear();
        let id = self.next_id();
        let gesture = self.gesture;
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
        if self.undo.len() > LIMIT {
            let oldest = self
                .undo
                .pop_front()
                .expect("the history is over its limit");
            self.base = oldest.id;
        }
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

    pub(crate) fn undo_name(&self) -> Option<&str> {
        self.undo.back().map(|step| step.name.as_str())
    }

    pub(crate) fn redo_name(&self) -> Option<&str> {
        self.redo.last().map(|step| step.name.as_str())
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
    use musicable_model::clip::{AudioClip, ClipContent, ClipId, Fades, MidiClip, Offset, Timing};
    use musicable_model::mixer::Bus;
    use musicable_model::time::{BeatRange, Beats, TempoMap};
    use musicable_model::{ClipType, LocatorId, Project, Send, SendId, TrackId, TrackKind};
    use proptest::prelude::*;

    use super::*;
    use crate::{Committed, Document, Transaction};

    /// The project the edits in `one_of_each` change: a track of each kind,
    /// two with a send, and a spare one that nothing refers to; two audio
    /// clips, one with fades, and a MIDI clip; and two locators.
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
        let clips = [
            (audio, "Faded", range(0, 8), ClipContent::Audio(faded)),
            (
                midi,
                "Bass",
                range(4, 8),
                ClipContent::Midi(MidiClip::default()),
            ),
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
        for (name, at) in [("Verse", 0), ("Chorus", 16)] {
            let locator = project.new_locator(name, Beats::new(at));
            project.locators.push(locator);
        }
        project
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
        let bass_timing = project.find_clip(bass).unwrap().1.timing();
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
        ];
        let mut kinds: Vec<_> = edits.iter().map(kind).collect();
        kinds.dedup();
        assert_eq!(kinds.len(), KINDS, "one example of each kind");
        edits
    }

    /// How many kinds of edit there are.
    const KINDS: usize = 31;

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
        BeginGesture,
        EndGesture,
        Save,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..KINDS, any::<bool>()).prop_map(|(kind, inverse)| Op::Edit { kind, inverse }),
            2 => Just(Op::Undo),
            1 => Just(Op::Redo),
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
