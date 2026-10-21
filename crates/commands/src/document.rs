//! The open project, and the transactions that change it.
//!
//! Commands change the project only through [`Document::transact`]. A
//! transaction records each [`Edit`] it makes along with the value it
//! replaced, so everything that has to follow the project hooks in here: the
//! engine now, undo and redo in Tier 1, ARA change notifications in Tier 2.

use musicable_model::Project;
use musicable_model::time::{BeatRange, TempoMap};

/// The open project. It hands out `&Project` only, so every change goes
/// through a transaction.
#[derive(Debug, Clone, Default)]
pub struct Document {
    project: Project,
    /// Transactions committed since the last [`take_committed`](Self::take_committed).
    committed: Vec<Committed>,
    /// Whether anything changed since the project was opened or saved.
    modified: bool,
}

impl Document {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            committed: Vec::new(),
            modified: false,
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// Runs `edit` as one transaction. `name` is what the undo menu will
    /// call it, such as "Tempo Up". A transaction that changes nothing
    /// leaves no record.
    pub fn transact<R>(&mut self, name: &str, edit: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
        let mut transaction = Transaction {
            project: &mut self.project,
            edits: Vec::new(),
        };
        let result = edit(&mut transaction);
        let edits = transaction.edits;
        if !edits.is_empty() {
            self.modified = true;
            self.committed.push(Committed {
                name: name.to_owned(),
                edits,
            });
        }
        result
    }

    /// The transactions committed since the last call, oldest first, for
    /// whatever keeps itself in step with the project. The app sends the
    /// engine what they changed.
    pub fn take_committed(&mut self) -> Vec<Committed> {
        std::mem::take(&mut self.committed)
    }

    /// Whether the project changed since it was opened or last saved.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.modified = false;
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
}

/// One change to the project, with the value it replaced.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Tempo { from: TempoMap, to: TempoMap },
    LoopRegion { from: BeatRange, to: BeatRange },
    Looping { from: bool, to: bool },
    Metronome { from: bool, to: bool },
}

/// A transaction that changed the project.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    /// What the undo menu will call it.
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
