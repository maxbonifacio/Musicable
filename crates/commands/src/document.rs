//! The open project, and the transactions that change it.
//!
//! Commands change the project only through [`Document::transact`]. A
//! transaction records each [`Edit`] it makes along with the value it
//! replaced, so everything that has to follow the project hooks in here: the
//! engine and the undo history now, ARA change notifications in Tier 2.

use musicable_model::Project;
use musicable_model::time::{BeatRange, TempoMap};

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

    /// Makes the change `edit` describes, through the setter for what it
    /// changes, as undo and redo do: `apply(&edit.inverse())` takes it back.
    pub fn apply(&mut self, edit: &Edit) {
        match edit {
            Edit::Tempo { to, .. } => self.set_tempo(to.clone()),
            Edit::LoopRegion { to, .. } => self.set_loop_region(*to),
            Edit::Looping { to, .. } => self.set_looping(*to),
            Edit::Metronome { to, .. } => self.set_metronome(*to),
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
    Tempo { from: TempoMap, to: TempoMap },
    LoopRegion { from: BeatRange, to: BeatRange },
    Looping { from: bool, to: bool },
    Metronome { from: bool, to: bool },
}

impl Edit {
    /// The edit that takes this one back.
    pub fn inverse(&self) -> Edit {
        match self.clone() {
            Edit::Tempo { from, to } => Edit::Tempo { from: to, to: from },
            Edit::LoopRegion { from, to } => Edit::LoopRegion { from: to, to: from },
            Edit::Looping { from, to } => Edit::Looping { from: to, to: from },
            Edit::Metronome { from, to } => Edit::Metronome { from: to, to: from },
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
                Edit::Tempo { .. }
                | Edit::LoopRegion { .. }
                | Edit::Looping { .. }
                | Edit::Metronome { .. },
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
