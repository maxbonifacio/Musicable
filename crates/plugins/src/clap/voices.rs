//! The voices a CLAP plugin plays, and the note each plays.

use musicable_engine::NoteId;

use crate::process::SoundingNote;

/// How many voices the node keeps track of on a plugin: notes held, and
/// notes let go whose voices may still be ringing.
pub(crate) const VOICE_CAPACITY: usize = 1024;

/// The voices a plugin plays, each from its note's start until the plugin
/// says it ended (CLAP's note end), which can be well after the note is let
/// go. So modulation for a note reaches its voice for as long as it sounds,
/// and the engine learns which note's voice ended. Allocated once: when
/// it's full, the voice that started longest ago is forgotten.
pub(crate) struct Voices {
    /// Oldest first.
    voices: Vec<SoundingNote>,
    /// The notes whose voices the plugin ended in the block it's
    /// processing.
    pub(crate) ended: Vec<NoteId>,
}

impl Voices {
    pub(crate) fn new() -> Self {
        Self {
            voices: Vec::with_capacity(VOICE_CAPACITY),
            ended: Vec::with_capacity(VOICE_CAPACITY),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.voices.clear();
        self.ended.clear();
    }

    /// A note's voice starts. An older voice of the same note is forgotten,
    /// since the note's id now means the new one.
    pub(crate) fn start(&mut self, sounding: SoundingNote) {
        self.voices.retain(|voice| voice.note != sounding.note);
        if self.voices.len() == self.voices.capacity() {
            self.voices.remove(0);
        }
        self.voices.push(sounding);
    }

    /// The voice playing `note`, while it sounds.
    pub(crate) fn find(&self, note: NoteId) -> Option<SoundingNote> {
        self.voices.iter().find(|voice| voice.note == note).copied()
    }

    /// The plugin ended the voice it played for the note it got with `id`,
    /// or, when `id` is -1, CLAP's none, the oldest voice on `key`. Its
    /// note goes in `ended`, if the node knows the voice.
    pub(crate) fn end(&mut self, id: i32, key: i16) {
        let index = if id >= 0 {
            self.voices.iter().position(|voice| voice.id == id)
        } else {
            self.voices.iter().position(|voice| voice.key == key)
        };
        let Some(index) = index else {
            return;
        };
        let voice = self.voices.remove(index);
        if self.ended.len() < self.ended.capacity() {
            self.ended.push(voice.note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(note: u64, id: i32, key: i16) -> SoundingNote {
        SoundingNote {
            note: NoteId(note),
            id,
            key,
        }
    }

    #[test]
    fn a_voice_lasts_until_the_plugin_ends_it() {
        let mut voices = Voices::new();
        voices.start(voice(1, 0, 60));
        voices.start(voice(2, 1, 60));
        assert_eq!(voices.find(NoteId(1)), Some(voice(1, 0, 60)));
        voices.end(1, 60);
        assert_eq!(voices.ended, [NoteId(2)]);
        assert_eq!(voices.find(NoteId(2)), None);

        // Without an id, the oldest voice on the key ends.
        voices.start(voice(3, 2, 60));
        voices.end(-1, 60);
        assert_eq!(voices.ended, [NoteId(2), NoteId(1)]);
        assert_eq!(voices.find(NoteId(3)), Some(voice(3, 2, 60)));

        // Voices the node doesn't know end without a note.
        voices.end(7, 60);
        voices.end(-1, 61);
        assert_eq!(voices.ended, [NoteId(2), NoteId(1)]);
    }

    #[test]
    fn a_note_started_again_forgets_its_old_voice() {
        let mut voices = Voices::new();
        voices.start(voice(1, 0, 60));
        voices.start(voice(1, 1, 62));
        assert_eq!(voices.find(NoteId(1)), Some(voice(1, 1, 62)));
        voices.end(0, 60);
        assert!(voices.ended.is_empty());
        voices.end(1, 62);
        assert_eq!(voices.ended, [NoteId(1)]);
    }

    #[test]
    fn when_its_full_the_oldest_voice_is_forgotten() {
        let mut voices = Voices::new();
        for i in 0..=VOICE_CAPACITY {
            voices.start(voice(i as u64, i as i32, 60));
        }
        assert_eq!(voices.find(NoteId(0)), None);
        assert_eq!(voices.find(NoteId(1)), Some(voice(1, 1, 60)));
        assert_eq!(voices.voices.capacity(), VOICE_CAPACITY);
    }
}
