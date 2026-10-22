//! Clips: what audio and MIDI tracks play.
//!
//! A clip sits on a track at a song position and plays for its length, both
//! in beats. An audio clip plays part of an audio file, and a MIDI clip plays
//! notes. Notes carry per-note expression, as MPE controllers send it, from
//! the start, so recording it and playing it back loses nothing.

use crate::time::{BeatRange, Beats, TempoMap};

/// Stable identifier for a clip, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClipId(pub u32);

/// A clip on a track. Clips can overlap: overlapping clips play together,
/// and where audio clips' fades overlap, they crossfade.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub id: ClipId,
    pub name: String,
    /// Where the clip starts in the song.
    pub start: Beats,
    /// How long it plays. Always more than zero.
    pub length: Beats,
    pub content: ClipContent,
}

impl Clip {
    /// Where the clip ends, just after its last tick.
    pub fn end(&self) -> Beats {
        self.start + self.length
    }

    /// The stretch of the song the clip plays over.
    pub fn range(&self) -> BeatRange {
        BeatRange::new(self.start, self.end())
    }

    /// Where the clip plays, and where in its content it starts.
    pub fn timing(&self) -> Timing {
        Timing {
            start: self.start,
            length: self.length,
            offset: self.content.offset(),
        }
    }

    /// Moves and trims the clip to `timing`. Returns `false`, changing
    /// nothing, if the timing doesn't fit the clip: a length that isn't more
    /// than zero, an offset of the other kind from the clip's content, or a
    /// length too short for an audio clip's fades. Shorten the fades first.
    pub fn set_timing(&mut self, timing: Timing) -> bool {
        if timing.length <= Beats::ZERO {
            return false;
        }
        match (&mut self.content, timing.offset) {
            (ClipContent::Audio(audio), Offset::Frames(offset)) => {
                if audio.fade_in + audio.fade_out > timing.length {
                    return false;
                }
                audio.offset = offset;
            }
            (ClipContent::Midi(midi), Offset::Beats(offset)) => midi.offset = offset,
            _ => return false,
        }
        self.start = timing.start;
        self.length = timing.length;
        true
    }

    /// The timing that plays what the clip plays now, over `range`: the clip
    /// with its edges moved to the range's, its content staying where it is
    /// in the song, as trimming and splitting need. Audio files play at
    /// their own speed, so an audio clip's offset moves by the frames of its
    /// file between the old start and the new, at the file's `sample_rate`.
    ///
    /// An audio clip can't start before the first frame of its file: the
    /// timing starts there at the earliest, and is `None` if that leaves
    /// nothing of `range`.
    pub fn timing_over(
        &self,
        range: BeatRange,
        tempo: &TempoMap,
        sample_rate: u32,
    ) -> Option<Timing> {
        let (start, offset) = match &self.content {
            ClipContent::Midi(midi) => {
                let offset = midi.offset + (range.start - self.start);
                (range.start, Offset::Beats(offset))
            }
            ClipContent::Audio(audio) => {
                let frames = |at: Beats| tempo.frame_at(at, sample_rate);
                // The song's frame that the file's first frame plays on.
                let first = frames(self.start) - audio.offset as i64;
                let mut start = range.start;
                if frames(start) < first {
                    let at = tempo.beats_at(first as f64, sample_rate);
                    start = Beats::from_f64(at).max(range.start);
                    while frames(start) < first {
                        start += Beats::from_ticks(1);
                    }
                }
                (start, Offset::Frames((frames(start) - first) as u64))
            }
        };
        let length = range.end - start;
        (length > Beats::ZERO).then_some(Timing {
            start,
            length,
            offset,
        })
    }
}

/// Where a clip plays in the song, and where in its content it starts
/// playing: what moving, trimming and splitting a clip change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub start: Beats,
    pub length: Beats,
    pub offset: Offset,
}

impl Timing {
    pub fn end(&self) -> Beats {
        self.start + self.length
    }

    pub fn range(&self) -> BeatRange {
        BeatRange::new(self.start, self.end())
    }
}

/// Where a clip's content starts playing at the clip's start: a frame of an
/// audio clip's file, or a position in a MIDI clip's own time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offset {
    Frames(u64),
    Beats(Beats),
}

/// What a clip plays. Audio tracks hold audio clips, and MIDI tracks MIDI
/// clips.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipContent {
    Audio(AudioClip),
    Midi(MidiClip),
}

impl ClipContent {
    /// Where the content starts playing at the clip's start.
    pub fn offset(&self) -> Offset {
        match self {
            Self::Audio(audio) => Offset::Frames(audio.offset),
            Self::Midi(midi) => Offset::Beats(midi.offset),
        }
    }
}

/// Part of an audio file.
///
/// Tier 1 doesn't warp: the file plays at its own speed from `offset`,
/// whatever the tempo, resampled if its sample rate isn't the device's. A
/// clip that runs on past the end of the file is silent from there.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioClip {
    /// The file's path: relative to the folder the project file is in, with
    /// `/` between names, or absolute.
    pub file: String,
    /// The frame of the file the clip starts playing from.
    pub offset: u64,
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    /// How long the clip takes to fade in from silence, from its start.
    pub fade_in: Beats,
    /// How long it takes to fade out to silence, up to its end. The fades
    /// fit in the clip together: `fade_in + fade_out` is at most its length.
    pub fade_out: Beats,
}

impl AudioClip {
    pub fn fades(&self) -> Fades {
        Fades {
            fade_in: self.fade_in,
            fade_out: self.fade_out,
        }
    }

    /// All of `file` from the start, at unity, without fades.
    pub fn new(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            offset: 0,
            gain: 1.0,
            fade_in: Beats::ZERO,
            fade_out: Beats::ZERO,
        }
    }
}

/// How long an audio clip takes to fade in from its start, and to fade out
/// to its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fades {
    pub fade_in: Beats,
    pub fade_out: Beats,
}

impl Fades {
    /// How long the two take together.
    pub fn total(&self) -> Beats {
        self.fade_in + self.fade_out
    }
}

/// Notes, placed in the clip's own time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MidiClip {
    /// Where in the clip's own time it starts playing: a note at `offset`
    /// plays at the clip's start. Notes that start before it, or at or after
    /// `offset` plus the clip's length, don't play, and a note that's still
    /// sounding when the clip ends stops there.
    pub offset: Beats,
    /// The id the next note added gets. Ids are never reused in a clip.
    pub next_note_id: u32,
    /// In any order. Two notes on the same key can overlap: they sound as
    /// two notes.
    pub notes: Vec<Note>,
}

impl MidiClip {
    /// Adds a note without expression, let go at no particular speed, with
    /// a new id, and returns it.
    pub fn add_note(&mut self, start: Beats, length: Beats, key: u8, velocity: f32) -> &mut Note {
        let id = NoteId(crate::next_id(&mut self.next_note_id, "note"));
        self.notes.push(Note {
            id,
            start,
            length,
            key,
            velocity,
            release_velocity: Note::RELEASE_VELOCITY,
            expression: Expression::default(),
        });
        self.notes.last_mut().expect("the note just added")
    }

    pub fn note(&self, id: NoteId) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub fn note_mut(&mut self, id: NoteId) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }
}

/// Stable identifier for a note, unique within its clip, so selections,
/// edits and undo can name a note however the notes around it change. In
/// the project, a note is its clip's id and its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NoteId(pub u32);

/// A note, with its per-note expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: NoteId,
    /// Where the note starts, in its clip's own time.
    pub start: Beats,
    /// Always more than zero.
    pub length: Beats,
    /// The MIDI note number, from 0 to 127: 60 is middle C, and 69 the A at
    /// 440 Hz.
    pub key: u8,
    /// How hard the note starts, from 0.0 to 1.0.
    pub velocity: f32,
    /// How quickly the note is let go, from 0.0 to 1.0. Keyboards that don't
    /// measure it send [`Note::RELEASE_VELOCITY`].
    pub release_velocity: f32,
    pub expression: Expression,
}

impl Note {
    /// The release velocity of a note let go at no particular speed.
    pub const RELEASE_VELOCITY: f32 = 0.5;

    /// The highest MIDI note number.
    pub const MAX_KEY: u8 = 127;

    pub fn end(&self) -> Beats {
        self.start + self.length
    }
}

/// How a note changes while it sounds: the three per-note dimensions that
/// MPE controllers send and that VST3 and CLAP note expression carry.
///
/// Each dimension is a curve of steps: a point's value holds from its time
/// until the next point's. A dimension without points leaves the instrument
/// to start it where it likes, usually at rest.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Expression {
    /// In semitones from the note's key, from −120 to 120: the range VST3
    /// and CLAP tuning expression take.
    pub pitch_bend: Vec<Point>,
    /// Pressure on the key after it went down (aftertouch), from 0.0 to 1.0.
    pub pressure: Vec<Point>,
    /// MPE's third dimension, usually moving a finger up and down the key
    /// (CC 74, "slide"), from 0.0 to 1.0.
    pub timbre: Vec<Point>,
}

impl Expression {
    /// The largest pitch bend either way, in semitones.
    pub const MAX_BEND: f32 = 120.0;

    pub fn curve(&self, dimension: Dimension) -> &[Point] {
        match dimension {
            Dimension::PitchBend => &self.pitch_bend,
            Dimension::Pressure => &self.pressure,
            Dimension::Timbre => &self.timbre,
        }
    }

    pub fn curve_mut(&mut self, dimension: Dimension) -> &mut Vec<Point> {
        match dimension {
            Dimension::PitchBend => &mut self.pitch_bend,
            Dimension::Pressure => &mut self.pressure,
            Dimension::Timbre => &mut self.timbre,
        }
    }

    pub fn is_empty(&self) -> bool {
        Dimension::ALL.iter().all(|&d| self.curve(d).is_empty())
    }
}

/// One of the dimensions of per-note expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dimension {
    PitchBend,
    Pressure,
    Timbre,
}

impl Dimension {
    pub const ALL: [Dimension; 3] = [Self::PitchBend, Self::Pressure, Self::Timbre];

    /// The values the dimension takes.
    pub fn range(self) -> (f32, f32) {
        match self {
            Self::PitchBend => (-Expression::MAX_BEND, Expression::MAX_BEND),
            Self::Pressure | Self::Timbre => (0.0, 1.0),
        }
    }
}

/// A step in an expression curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// When the value starts to hold, from the note's start: zero or more.
    /// A curve's points are in order of `at`, and a later point at the same
    /// time wins. Points at or after the note's end don't play, so a note
    /// that's shortened and lengthened again keeps them.
    pub at: Beats,
    pub value: f32,
}

impl Point {
    pub const fn new(at: Beats, value: f32) -> Self {
        Self { at, value }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_end_after_their_length() {
        let clip = Clip {
            id: ClipId(0),
            name: String::new(),
            start: Beats::new(4),
            length: Beats::new(2),
            content: ClipContent::Midi(MidiClip::default()),
        };
        assert_eq!(clip.end(), Beats::new(6));
        assert!(clip.range().contains(Beats::new(4)));
        assert!(!clip.range().contains(Beats::new(6)));
    }

    fn audio_clip(start: i64, length: i64, offset: u64) -> Clip {
        Clip {
            id: ClipId(1),
            name: "Take".into(),
            start: Beats::new(start),
            length: Beats::new(length),
            content: ClipContent::Audio(AudioClip {
                offset,
                ..AudioClip::new("take.wav")
            }),
        }
    }

    #[test]
    fn timing_moves_and_trims_clips_within_their_fades() {
        let mut clip = audio_clip(4, 4, 0);
        let ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!()
        };
        audio.fade_in = Beats::new(1);
        audio.fade_out = Beats::new(1);
        let timing = Timing {
            start: Beats::new(8),
            length: Beats::new(2),
            offset: Offset::Frames(100),
        };
        assert!(clip.set_timing(timing));
        assert_eq!(clip.timing(), timing);
        assert_eq!(
            timing.range(),
            BeatRange::new(Beats::new(8), Beats::new(10))
        );
        // Too short for the fades, no length, and the wrong kind of offset.
        let short = Timing {
            length: Beats::from_ratio(3, 2),
            ..timing
        };
        let empty = Timing {
            length: Beats::ZERO,
            ..timing
        };
        let beats = Timing {
            offset: Offset::Beats(Beats::ZERO),
            ..timing
        };
        for refused in [short, empty, beats] {
            assert!(!clip.set_timing(refused));
            assert_eq!(clip.timing(), timing);
        }
    }

    #[test]
    fn trimming_keeps_the_content_where_it_plays() {
        // At 120 BPM a beat is half a second: 24,000 frames at 48 kHz.
        let tempo = TempoMap::default();
        let clip = audio_clip(4, 4, 1000);
        let range = BeatRange::new(Beats::new(5), Beats::new(7));
        let trimmed = clip.timing_over(range, &tempo, 48_000).unwrap();
        assert_eq!(trimmed.range(), range);
        assert_eq!(trimmed.offset, Offset::Frames(25_000));
        // A file at another rate moves by its own frames.
        let trimmed = clip.timing_over(range, &tempo, 44_100).unwrap();
        assert_eq!(trimmed.offset, Offset::Frames(23_050));

        // Back past the file's first frame, it stops there.
        let earlier = BeatRange::new(Beats::new(2), Beats::new(8));
        let extended = clip.timing_over(earlier, &tempo, 48_000).unwrap();
        assert_eq!(extended.offset, Offset::Frames(0));
        assert_eq!(tempo.frame_at(extended.start, 48_000), 4 * 24_000 - 1000);
        let before = BeatRange::new(Beats::new(1), Beats::new(3));
        assert_eq!(clip.timing_over(before, &tempo, 48_000), None);

        // MIDI clips move their offset in beats, before zero too.
        let midi = Clip {
            content: ClipContent::Midi(MidiClip::default()),
            ..audio_clip(4, 4, 0)
        };
        let earlier = midi.timing_over(earlier, &tempo, 48_000).unwrap();
        assert_eq!(earlier.offset, Offset::Beats(Beats::new(-2)));
        assert_eq!(
            earlier.range(),
            BeatRange::new(Beats::new(2), Beats::new(8))
        );
    }

    #[test]
    fn notes_keep_their_ids_and_ids_are_never_reused() {
        let mut midi = MidiClip::default();
        let a = midi.add_note(Beats::ZERO, Beats::new(1), 60, 0.5).id;
        let b = midi.add_note(Beats::ZERO, Beats::new(1), 60, 0.5).id;
        assert_ne!(a, b);
        midi.notes.retain(|n| n.id != a);
        midi.note_mut(b).unwrap().key = 64;
        let c = midi.add_note(Beats::new(2), Beats::new(1), 67, 1.0).id;
        assert!(c != a && c != b);
        assert_eq!(midi.note(b).unwrap().key, 64);
        assert_eq!(midi.note(a), None);
        assert_eq!(
            midi.note(c).unwrap().release_velocity,
            Note::RELEASE_VELOCITY
        );
    }

    #[test]
    fn expression_has_a_curve_for_each_dimension() {
        let mut expression = Expression::default();
        assert!(expression.is_empty());
        for (i, dimension) in Dimension::ALL.into_iter().enumerate() {
            let point = Point::new(Beats::ZERO, i as f32 / 4.0);
            expression.curve_mut(dimension).push(point);
            assert_eq!(expression.curve(dimension), [point]);
        }
        assert_eq!(expression.pitch_bend[0].value, 0.0);
        assert_eq!(expression.pressure[0].value, 0.25);
        assert_eq!(expression.timbre[0].value, 0.5);
        assert!(!expression.is_empty());
    }
}
