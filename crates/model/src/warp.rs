//! Warping: how an audio clip follows the song's tempo.
//!
//! A warped audio clip plays in its own time, as a MIDI clip's notes do:
//! the markers of its [`Warp`] pin frames of its file to beats of that
//! time, whose beat 0 is the clip's 1.1.1, and the audio between two
//! markers stretches to fill the beats between them. Before the first
//! marker and after the last, the audio plays at the tempo next to it, and
//! a clip with one marker plays at its warp's [`tempo`](Warp::tempo) all
//! through. The clip's offset is a beat of its own time too, as a MIDI
//! clip's is (see [`AudioClip`](crate::clip::AudioClip)), so moving,
//! trimming and splitting the clip never moves its audio against the beats.
//!
//! Beats are whole ticks and frames whole numbers, so a marker stays
//! exactly where it's put. The [`WarpMode`] says how the audio between
//! markers plays: Re-Pitch resamples it, so its pitch follows its speed, as
//! on a turntable, Slice cuts it into slices at its transients, or on a
//! grid of notes, and plays each at the file's own speed from where it
//! lands, as Live's Beats does, Granular plays it as short overlapping
//! grains, as Live's Tones does with Follow pitch on and Texture with it
//! off, and Spectral stretches it in the frequency domain, keeping its
//! pitch, as Live's Complex does, or Complex Pro with High quality on.
//! Every mode but Re-Pitch moves the audio's pitch by the warp's transpose
//! and detune, and keeps its own settings in the warp while another mode
//! plays.
//!
//! A warp also keeps its file's transients, the [`Onsets`]: where the
//! sounds in it start, as analysis found them and the user changed them.
//! They're in frames of the file, so moving markers never moves them, and
//! the clip keeps its own, so a better analyser later never moves them
//! either.

use std::sync::Arc;

use crate::param::ClipParam;
use crate::time::Beats;

/// How a warped clip's file follows the clip's own time.
#[derive(Debug, Clone, PartialEq)]
pub struct Warp {
    pub mode: WarpMode,
    /// At least one, in order: both the frames and the beats rise.
    pub markers: Vec<WarpMarker>,
    /// The clip's tempo while it has one marker: quarter notes a minute of
    /// the file, from [`MIN_TEMPO`](Self::MIN_TEMPO) to
    /// [`MAX_TEMPO`](Self::MAX_TEMPO). With more, the markers say how fast
    /// the file goes, and this waits for all but one of them to go.
    pub tempo: f64,
    /// The file's transients, or `None` until the clip takes them from
    /// analysis, which it does once: they're its own from then on.
    pub onsets: Option<Onsets>,
    /// How far every mode but Re-Pitch moves the audio's pitch, in
    /// semitones, from -[`TRANSPOSE_MAX`](Self::TRANSPOSE_MAX) to
    /// [`TRANSPOSE_MAX`](Self::TRANSPOSE_MAX), and cents, from
    /// -[`DETUNE_MAX`](Self::DETUNE_MAX) to [`DETUNE_MAX`](Self::DETUNE_MAX),
    /// where the clip's curves don't say otherwise.
    pub transpose: i8,
    pub detune: i8,
    /// How Slice cuts the audio and plays the slices.
    pub slice: SliceSettings,
    /// How Granular cuts the audio into grains.
    pub granular: GranularSettings,
    /// Which stretcher Spectral renders with, and how it treats the audio's
    /// formants.
    pub spectral: SpectralSettings,
}

/// A frame of the file, pinned to the beat of the clip's own time it plays
/// at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarpMarker {
    pub frame: u64,
    pub beat: Beats,
}

/// How the audio between a warped clip's markers plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WarpMode {
    /// Resampled at a rate that follows the markers, so its pitch follows
    /// its speed, as on a turntable.
    #[default]
    RePitch,
    /// Cut into slices, each played at the file's own speed from where it
    /// lands, so a hit keeps its sound at any tempo: for drums and other
    /// percussive loops. See [`SliceSettings`].
    Slice,
    /// Played as short overlapping grains, each at the file's own speed
    /// from where it lands, so it keeps its pitch at any speed: for lines
    /// with one note at a time, such as voices and bass lines, with Follow
    /// pitch on, and pads and textures with it off. See
    /// [`GranularSettings`].
    Granular,
    /// Stretched in the frequency domain, so it keeps its pitch at any
    /// speed: for full mixes, chords, voices and anything else with more
    /// than one note at once. It plays audio rendered ahead, as its
    /// stretchers remember what they played. See [`SpectralSettings`].
    Spectral,
}

impl WarpMode {
    /// Every mode, in the order the Sample Editor lists them.
    pub const ALL: [Self; 4] = [Self::RePitch, Self::Slice, Self::Granular, Self::Spectral];

    /// What the Sample Editor calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::RePitch => "Re-Pitch",
            Self::Slice => "Slice",
            Self::Granular => "Granular",
            Self::Spectral => "Spectral",
        }
    }

    /// What scripts call it.
    pub fn key(self) -> &'static str {
        match self {
            Self::RePitch => "re_pitch",
            Self::Slice => "slice",
            Self::Granular => "granular",
            Self::Spectral => "spectral",
        }
    }

    /// The mode scripts call `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == key)
    }

    /// Whether it moves the audio's pitch by the warp's transpose and
    /// detune: every mode but Re-Pitch, whose pitch follows its speed.
    pub fn transposes(self) -> bool {
        self != Self::RePitch
    }

    /// Whether it plays audio rendered ahead, rather than the file as it
    /// plays: Spectral, whose stretcher remembers what it played, so a
    /// frame it plays live would depend on where playback started.
    pub fn renders(self) -> bool {
        self == Self::Spectral
    }
}

/// How Slice cuts a clip's audio into slices and plays them, as Live's
/// Beats does. A clip keeps them while another mode plays.
///
/// Each slice starts playing where its start lands in the clip's own time,
/// at the file's own speed, or faster or slower by the transpose. Where the
/// song plays the file faster than that, a slice is cut short by the next
/// one, and where it plays slower, the slice's audio runs out before the
/// next starts, and [`transient_loop`](Self::transient_loop) says what
/// fills the gap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SliceSettings {
    /// Where the audio is cut.
    pub preserve: Preserve,
    pub transient_loop: TransientLoop,
    /// Transient Envelope: how much each slice fades as it plays, from 0, a
    /// very quick decay, to [`ENVELOPE_MAX`](Self::ENVELOPE_MAX), no fade.
    pub envelope: f64,
}

impl SliceSettings {
    pub const ENVELOPE_MAX: f64 = 100.0;
}

impl Default for SliceSettings {
    /// Live's: cut at the transients, loop forward, and no fade.
    fn default() -> Self {
        Self {
            preserve: Preserve::Transients,
            transient_loop: TransientLoop::Forward,
            envelope: Self::ENVELOPE_MAX,
        }
    }
}

/// How Granular cuts a clip's audio into grains, as Live's Tones does with
/// Follow pitch on and Texture with it off. A clip keeps them while another
/// mode plays.
///
/// Each grain plays the file at its own speed, or faster or slower by the
/// transpose, from where the song has got to in the file, rising as the
/// grain before it falls, so the grains overlap two at a time. Where the
/// song plays the file slower than its own speed, the grains go over some
/// of the audio again, and where faster, they leave some out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GranularSettings {
    /// Whether the grains follow the audio's pitch, as Live's Tones does:
    /// where analysis found a pitch, each grain is a whole number of its
    /// periods long and starts where one does, so a line with one note at a
    /// time keeps its sound. Off, the grains are cut on a steady grid,
    /// spread by [`fluctuation`](Self::fluctuation), as Live's Texture
    /// cuts pads and noise.
    pub follow_pitch: bool,
    /// How long each grain is, in milliseconds, from
    /// [`GRAIN_SIZE_MIN`](Self::GRAIN_SIZE_MIN) to
    /// [`GRAIN_SIZE_MAX`](Self::GRAIN_SIZE_MAX): with Follow pitch on, a
    /// whole number of periods, within a period of this, and two at least.
    pub grain_size: f64,
    /// While Follow pitch is off, how far each grain strays from where and
    /// when it would play, as a share of its length, from 0, a steady grid,
    /// to [`FLUCTUATION_MAX`](Self::FLUCTUATION_MAX). The spread is seeded,
    /// so it's the same every time.
    pub fluctuation: f64,
}

impl GranularSettings {
    pub const GRAIN_SIZE_MIN: f64 = 5.0;
    pub const GRAIN_SIZE_MAX: f64 = 500.0;
    pub const GRAIN_SIZE_DEFAULT: f64 = 30.0;
    pub const FLUCTUATION_MAX: f64 = 100.0;
    pub const FLUCTUATION_DEFAULT: f64 = 25.0;
}

impl Default for GranularSettings {
    /// Following the pitch, with 30 ms grains, and a spread of 25% for when
    /// it doesn't, as Live's Texture's Flux starts.
    fn default() -> Self {
        Self {
            follow_pitch: true,
            grain_size: Self::GRAIN_SIZE_DEFAULT,
            fluctuation: Self::FLUCTUATION_DEFAULT,
        }
    }
}

/// Which stretcher Spectral renders a clip's audio with, and how it treats
/// its formants, the peaks in the outline of its spectrum that give a voice
/// its character, as it transposes. A clip keeps them while another mode
/// plays.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralSettings {
    /// Whether it renders with Rubber Band's R3 engine, the best quality and
    /// slower to render, as Live's Complex Pro, rather than Signalsmith
    /// Stretch, as Live's Complex.
    pub high_quality: bool,
    /// How far the formants stay where they were as the pitch moves, from 0,
    /// moving with it as a record's do played faster, to
    /// [`FORMANTS_MAX`](Self::FORMANTS_MAX), staying in place, so a voice
    /// transposed sounds like the same voice.
    pub formants: f64,
    /// While High quality is on, how fine the outline of the spectrum is
    /// that the formants are kept by, from
    /// [`ENVELOPE_MIN`](Self::ENVELOPE_MIN) to
    /// [`ENVELOPE_MAX`](Self::ENVELOPE_MAX): 128 suits most audio, lower
    /// suits high voices, whose harmonics are far apart, and higher low
    /// ones.
    pub envelope: f64,
}

impl SpectralSettings {
    pub const FORMANTS_MAX: f64 = 100.0;
    pub const ENVELOPE_MIN: f64 = 8.0;
    pub const ENVELOPE_MAX: f64 = 512.0;
    pub const ENVELOPE_DEFAULT: f64 = 128.0;

    /// How far the stretcher moves the formants from where they were, for
    /// a transpose of `ratio`: not at all, 1, with the formants all the way
    /// in place, and as far as the pitch, `ratio`, with them at 0.
    pub fn formant_ratio(formants: f64, ratio: f64) -> f64 {
        ratio.powf(1.0 - formants / Self::FORMANTS_MAX)
    }
}

impl Default for SpectralSettings {
    /// Signalsmith Stretch, the quicker to render, with formants kept in
    /// place and the envelope as Live's Complex Pro has them.
    fn default() -> Self {
        Self {
            high_quality: false,
            formants: Self::FORMANTS_MAX,
            envelope: Self::ENVELOPE_DEFAULT,
        }
    }
}

/// Where Slice cuts a clip's audio: at its transients, or on a grid of
/// notes in the clip's own time, whatever the audio, from its 1.1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preserve {
    /// At each transient in sight, or on a grid of sixteenth notes until
    /// the audio's analysis gives the clip its transients.
    #[default]
    Transients,
    Bar,
    Half,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
}

impl Preserve {
    /// Every one, in the order the Sample Editor lists them, which is the
    /// order of their steps in automation, from 0.
    pub const ALL: [Self; 7] = [
        Self::Transients,
        Self::Bar,
        Self::Half,
        Self::Quarter,
        Self::Eighth,
        Self::Sixteenth,
        Self::ThirtySecond,
    ];

    /// What the Sample Editor calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Transients => "Transients",
            Self::Bar => "1 Bar",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
        }
    }

    /// What scripts and project files call it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Transients => "transients",
            Self::Bar => "bar",
            Self::Half => "1/2",
            Self::Quarter => "1/4",
            Self::Eighth => "1/8",
            Self::Sixteenth => "1/16",
            Self::ThirtySecond => "1/32",
        }
    }

    /// The one scripts call `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    /// Its step in automation: its place in [`ALL`](Self::ALL).
    pub fn step(self) -> usize {
        Self::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    /// The one at step `value` of its automation, to the nearest step.
    pub fn at_step(value: f64) -> Self {
        Self::ALL[nearest_step(value, Self::ALL.len())]
    }

    /// How long each slice of the grid is, with a bar `bar` long, or `None`
    /// for the transients.
    pub fn length(self, bar: Beats) -> Option<Beats> {
        let notes = |n: i64| Beats::from_ticks(4 * Beats::TICKS_PER_BEAT / n);
        match self {
            Self::Transients => None,
            Self::Bar => Some(bar),
            Self::Half => Some(notes(2)),
            Self::Quarter => Some(notes(4)),
            Self::Eighth => Some(notes(8)),
            Self::Sixteenth => Some(notes(16)),
            Self::ThirtySecond => Some(notes(32)),
        }
    }
}

/// What a slice plays once its audio runs out before the next slice
/// starts, as it does where the song plays the file slower than its own
/// speed, as Live's Transient Loop Mode does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransientLoop {
    /// Nothing: silence until the next slice.
    Off,
    /// The second half of the slice, over and over.
    #[default]
    Forward,
    /// The second half of the slice, backwards and then forwards again.
    BackAndForth,
}

impl TransientLoop {
    /// Every one, in the order the Sample Editor lists them, which is the
    /// order of their steps in automation, from 0.
    pub const ALL: [Self; 3] = [Self::Off, Self::Forward, Self::BackAndForth];

    /// What the Sample Editor calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Forward => "Forward",
            Self::BackAndForth => "Back-and-Forth",
        }
    }

    /// What scripts and project files call it.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Forward => "forward",
            Self::BackAndForth => "back_and_forth",
        }
    }

    /// The one scripts call `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.key() == key)
    }

    /// Its step in automation: its place in [`ALL`](Self::ALL).
    pub fn step(self) -> usize {
        Self::ALL.iter().position(|&l| l == self).unwrap_or(0)
    }

    /// The one at step `value` of its automation, to the nearest step.
    pub fn at_step(value: f64) -> Self {
        Self::ALL[nearest_step(value, Self::ALL.len())]
    }
}

/// The step nearest `value` of `steps` of them, from 0.
fn nearest_step(value: f64, steps: usize) -> usize {
    match value.is_nan() {
        true => 0,
        false => value.round().clamp(0.0, (steps - 1) as f64) as usize,
    }
}

/// How much faster a transpose of `semitones` and `cents` plays audio:
/// twice as fast an octave up.
pub fn pitch_ratio(semitones: f64, cents: f64) -> f64 {
    ((semitones + cents / 100.0) / 12.0).exp2()
}

/// A transient: where a sound in the file starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Onset {
    /// The frame of the file its attack starts at.
    pub frame: u64,
    /// How much the spectrum rose there, as a fraction of the file's
    /// largest rise, from 0 to 1.
    pub strength: f32,
    /// How sharp its attack is, from 0 for a soft start to 1 for a hit.
    pub percussiveness: f32,
    pub origin: Origin,
}

/// Where a transient came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Analysis found it.
    Found,
    /// The user added it, or moved it there.
    Added,
    /// Analysis found it, and the user deleted it or moved it away. It's
    /// kept out of sight, so Reset Transients can bring it back.
    Hidden,
}

/// A warped clip's transients, in order of frame, one at most at a frame.
/// Clones share them, so the undo history and copies of the clip hold
/// thousands without copying them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Onsets(Arc<[Onset]>);

impl Onsets {
    /// What the user's transients count as: a sharp hit, as strong as the
    /// file's strongest.
    pub const ADDED_STRENGTH: f32 = 1.0;
    pub const ADDED_PERCUSSIVENESS: f32 = 1.0;

    /// `onsets`, in order of frame, keeping the first at each frame.
    pub fn new(onsets: impl IntoIterator<Item = Onset>) -> Self {
        let mut onsets: Vec<Onset> = onsets.into_iter().collect();
        onsets.sort_by_key(|o| o.frame);
        onsets.dedup_by_key(|o| o.frame);
        Self(onsets.into())
    }

    /// Every one, the hidden ones too, in order.
    pub fn all(&self) -> &[Onset] {
        &self.0
    }

    /// The ones in sight, found or added, in order.
    pub fn visible(&self) -> impl DoubleEndedIterator<Item = &Onset> + '_ {
        self.0.iter().filter(|o| o.origin != Origin::Hidden)
    }

    /// Whether they keep their rules: frames rising, and strengths and
    /// percussiveness from 0 to 1.
    pub fn is_valid(&self) -> bool {
        let unit = |value: f32| (0.0..=1.0).contains(&value);
        self.0.windows(2).all(|w| w[0].frame < w[1].frame)
            && self
                .0
                .iter()
                .all(|o| unit(o.strength) && unit(o.percussiveness))
    }

    /// The place among all of the one at `frame`, if there's one.
    fn find(&self, frame: u64) -> Result<usize, usize> {
        self.0.binary_search_by_key(&frame, |o| o.frame)
    }

    /// The place among all of the one in sight at `frame`, if there's one.
    fn find_visible(&self, frame: u64) -> Option<usize> {
        self.find(frame)
            .ok()
            .filter(|&i| self.0[i].origin != Origin::Hidden)
    }

    /// Adds a transient at `frame`, as the user's: one hidden there comes
    /// back as it was found. Returns `false`, changing nothing, where
    /// there's one in sight already.
    pub fn insert(&mut self, frame: u64) -> bool {
        let mut onsets = self.0.to_vec();
        match self.find(frame) {
            Ok(i) if onsets[i].origin == Origin::Hidden => onsets[i].origin = Origin::Found,
            Ok(_) => return false,
            Err(i) => onsets.insert(i, Self::added(frame)),
        }
        self.0 = onsets.into();
        true
    }

    /// A transient at `frame` that the user added.
    fn added(frame: u64) -> Onset {
        Onset {
            frame,
            strength: Self::ADDED_STRENGTH,
            percussiveness: Self::ADDED_PERCUSSIVENESS,
            origin: Origin::Added,
        }
    }

    /// The frames the transient in sight at `frame` can move between, not
    /// taking them in: those of the ones in sight either side, or the
    /// file's ends.
    pub fn room(&self, frame: u64) -> Option<(Option<u64>, Option<u64>)> {
        let i = self.find_visible(frame)?;
        let before = self.0[..i]
            .iter()
            .rev()
            .find(|o| o.origin != Origin::Hidden);
        let after = self.0[i + 1..].iter().find(|o| o.origin != Origin::Hidden);
        Some((before.map(|o| o.frame), after.map(|o| o.frame)))
    }

    /// Moves the transient in sight at `from` to `to`, keeping its strength
    /// and how percussive it is. A found one is hidden where it was and
    /// added where it goes, so Reset Transients can bring it back, and one
    /// hidden at `to` comes back instead. Returns `false`, changing
    /// nothing, if there's none at `from`, or `to` isn't between the frames
    /// of those in sight either side.
    pub fn move_to(&mut self, from: u64, to: u64) -> bool {
        let Some((before, after)) = self.room(from) else {
            return false;
        };
        let between = before.is_none_or(|b| b < to) && after.is_none_or(|a| to < a);
        if from == to || !between {
            return false;
        }
        let mut onsets = self.0.to_vec();
        let i = self.find(from).expect("one is in sight there");
        let moved = onsets[i];
        match moved.origin {
            Origin::Found => onsets[i].origin = Origin::Hidden,
            _ => {
                onsets.remove(i);
            }
        }
        match onsets.binary_search_by_key(&to, |o| o.frame) {
            // Hidden: nothing in sight is between.
            Ok(j) => onsets[j].origin = Origin::Found,
            Err(j) => onsets.insert(
                j,
                Onset {
                    frame: to,
                    origin: Origin::Added,
                    ..moved
                },
            ),
        }
        self.0 = onsets.into();
        true
    }

    /// Deletes the transients in sight at `frames`: found ones are hidden,
    /// so Reset Transients can bring them back, and added ones go. Returns
    /// how many there were.
    pub fn delete(&mut self, frames: &[u64]) -> usize {
        let mut onsets = self.0.to_vec();
        let mut deleted = 0;
        for &frame in frames {
            if let Ok(i) = onsets.binary_search_by_key(&frame, |o| o.frame) {
                match onsets[i].origin {
                    Origin::Found => onsets[i].origin = Origin::Hidden,
                    Origin::Added => {
                        onsets.remove(i);
                    }
                    Origin::Hidden => continue,
                }
                deleted += 1;
            }
        }
        if deleted > 0 {
            self.0 = onsets.into();
        }
        deleted
    }

    /// Goes back to the transients analysis found: the added ones go, and
    /// the hidden ones come back.
    pub fn reset(&mut self) {
        let onsets: Vec<Onset> = (self.0.iter())
            .filter(|o| o.origin != Origin::Added)
            .map(|&o| Onset {
                origin: Origin::Found,
                ..o
            })
            .collect();
        self.0 = onsets.into();
    }
}

/// A stretch of a warped clip's file that plays at one speed: from a
/// marker to the next, or for a clip with one marker, all of it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    /// The marker it starts at: the beat of the clip's own time and the
    /// frame of the file.
    pub beat: Beats,
    pub frame: u64,
    /// Frames of the file for each beat of the clip's own time.
    pub frames_per_beat: f64,
}

impl Segment {
    /// The frame of the file, fractional, at `beat` of the clip's own time.
    pub fn frame_at(&self, beat: f64) -> f64 {
        self.frame as f64 + (beat - self.beat.to_f64()) * self.frames_per_beat
    }

    /// The beat of the clip's own time, fractional, that the file's `frame`
    /// plays at.
    pub fn beat_at(&self, frame: f64) -> f64 {
        self.beat.to_f64() + (frame - self.frame as f64) / self.frames_per_beat
    }

    /// The tempo the file plays at in the segment, at `sample_rate`.
    pub fn tempo(&self, sample_rate: u32) -> f64 {
        60.0 * f64::from(sample_rate) / self.frames_per_beat
    }
}

impl Warp {
    /// The slowest and the fastest a clip's tempo goes, as in Live.
    pub const MIN_TEMPO: f64 = 5.0;
    pub const MAX_TEMPO: f64 = 999.0;

    /// The furthest a warp transposes either way: four octaves.
    pub const TRANSPOSE_MAX: i8 = 48;
    /// The furthest it detunes either way, in cents.
    pub const DETUNE_MAX: i8 = 50;

    /// A Re-Pitch warp with one marker, pinning `frame` to `beat`, at
    /// `tempo`, clamped to the range, with no transpose and the other
    /// modes' settings as their defaults say.
    pub fn new(frame: u64, beat: Beats, tempo: f64) -> Self {
        Self {
            mode: WarpMode::RePitch,
            markers: vec![WarpMarker { frame, beat }],
            tempo: clamp_tempo(tempo),
            onsets: None,
            transpose: 0,
            detune: 0,
            slice: SliceSettings::default(),
            granular: GranularSettings::default(),
            spectral: SpectralSettings::default(),
        }
    }

    /// Whether the warp keeps its rules: a marker or more, their frames and
    /// beats rising, a tempo in range, transients that keep theirs, and a
    /// transpose, detune, Transient Envelope, Grain Size, Fluctuation,
    /// Formants and Envelope in range.
    pub fn is_valid(&self) -> bool {
        let envelope = 0.0..=SliceSettings::ENVELOPE_MAX;
        let grain_size = GranularSettings::GRAIN_SIZE_MIN..=GranularSettings::GRAIN_SIZE_MAX;
        let fluctuation = 0.0..=GranularSettings::FLUCTUATION_MAX;
        let formants = 0.0..=SpectralSettings::FORMANTS_MAX;
        let spectral_envelope = SpectralSettings::ENVELOPE_MIN..=SpectralSettings::ENVELOPE_MAX;
        !self.markers.is_empty()
            && self
                .markers
                .windows(2)
                .all(|w| w[0].frame < w[1].frame && w[0].beat < w[1].beat)
            && (Self::MIN_TEMPO..=Self::MAX_TEMPO).contains(&self.tempo)
            && self.onsets.as_ref().is_none_or(Onsets::is_valid)
            && self.transpose.unsigned_abs() <= Self::TRANSPOSE_MAX.unsigned_abs()
            && self.detune.unsigned_abs() <= Self::DETUNE_MAX.unsigned_abs()
            && envelope.contains(&self.slice.envelope)
            && grain_size.contains(&self.granular.grain_size)
            && fluctuation.contains(&self.granular.fluctuation)
            && formants.contains(&self.spectral.formants)
            && spectral_envelope.contains(&self.spectral.envelope)
    }

    /// The value of the setting the clip parameter `param` is, as its
    /// automation has it: `None` for Gain, which is the clip's own.
    pub fn param(&self, param: ClipParam) -> Option<f64> {
        match param {
            ClipParam::Gain => None,
            ClipParam::Transpose => Some(f64::from(self.transpose)),
            ClipParam::Detune => Some(f64::from(self.detune)),
            ClipParam::TransientEnvelope => Some(self.slice.envelope),
            ClipParam::Preserve => Some(self.slice.preserve.step() as f64),
            ClipParam::TransientLoop => Some(self.slice.transient_loop.step() as f64),
            ClipParam::GrainSize => Some(self.granular.grain_size),
            ClipParam::Fluctuation => Some(self.granular.fluctuation),
            ClipParam::Formants => Some(self.spectral.formants),
            ClipParam::Envelope => Some(self.spectral.envelope),
        }
    }

    /// Sets the setting the clip parameter `param` is to `value`, kept in
    /// its range and rounded to a whole semitone, cent or step. Gain, and
    /// values that aren't numbers, change nothing.
    pub fn set_param(&mut self, param: ClipParam, value: f64) {
        if value.is_nan() {
            return;
        }
        let value = param.spec().clamp(value);
        match param {
            ClipParam::Gain => {}
            ClipParam::Transpose => self.transpose = value.round() as i8,
            ClipParam::Detune => self.detune = value.round() as i8,
            ClipParam::TransientEnvelope => self.slice.envelope = value,
            ClipParam::Preserve => self.slice.preserve = Preserve::at_step(value),
            ClipParam::TransientLoop => self.slice.transient_loop = TransientLoop::at_step(value),
            ClipParam::GrainSize => self.granular.grain_size = value,
            ClipParam::Fluctuation => self.granular.fluctuation = value,
            ClipParam::Formants => self.spectral.formants = value,
            ClipParam::Envelope => self.spectral.envelope = value,
        }
    }

    /// The transients in sight, found or added, in order: none until the
    /// clip takes them from analysis.
    pub fn transients(&self) -> impl DoubleEndedIterator<Item = &Onset> + '_ {
        self.onsets.iter().flat_map(Onsets::visible)
    }

    /// The stretches that play at one speed, in order, for a file at
    /// `sample_rate`: one from each marker but the last, or one for a clip
    /// with one marker. The first covers the audio before it too, and the
    /// last the audio after it.
    pub fn segments(&self, sample_rate: u32) -> impl Iterator<Item = Segment> + '_ {
        let one = (self.markers.len() == 1).then(|| {
            let marker = self.markers[0];
            Segment {
                beat: marker.beat,
                frame: marker.frame,
                frames_per_beat: 60.0 * f64::from(sample_rate) / self.tempo,
            }
        });
        let between = self.markers.windows(2).map(|w| Segment {
            beat: w[0].beat,
            frame: w[0].frame,
            frames_per_beat: (w[1].frame - w[0].frame) as f64 / (w[1].beat - w[0].beat).to_f64(),
        });
        one.into_iter().chain(between)
    }

    /// The segment that plays `beat` of the clip's own time.
    pub fn segment_at(&self, beat: f64, sample_rate: u32) -> Segment {
        // The markers after the first that come at or before the beat say
        // which segment it's in, the last's segment being the one before it.
        let interior = &self.markers[1..self.markers.len().saturating_sub(1).max(1)];
        let index = interior.partition_point(|m| m.beat.to_f64() <= beat);
        self.segment(index, sample_rate)
    }

    /// The segment that plays the file's `frame`.
    pub fn segment_of_frame(&self, frame: f64, sample_rate: u32) -> Segment {
        let interior = &self.markers[1..self.markers.len().saturating_sub(1).max(1)];
        let index = interior.partition_point(|m| m.frame as f64 <= frame);
        self.segment(index, sample_rate)
    }

    /// Segment `index`, from the marker with that index.
    fn segment(&self, index: usize, sample_rate: u32) -> Segment {
        let marker = self.markers[index];
        let frames_per_beat = match self.markers.get(index + 1) {
            Some(next) => (next.frame - marker.frame) as f64 / (next.beat - marker.beat).to_f64(),
            None => 60.0 * f64::from(sample_rate) / self.tempo,
        };
        Segment {
            beat: marker.beat,
            frame: marker.frame,
            frames_per_beat,
        }
    }

    /// The frame of the file, fractional, that plays at `beat` of the
    /// clip's own time, for a file at `sample_rate`.
    pub fn frame_at(&self, beat: f64, sample_rate: u32) -> f64 {
        self.segment_at(beat, sample_rate).frame_at(beat)
    }

    /// The beat of the clip's own time, fractional, that the file's `frame`
    /// plays at: the inverse of [`frame_at`](Self::frame_at).
    pub fn beat_at(&self, frame: f64, sample_rate: u32) -> f64 {
        self.segment_of_frame(frame, sample_rate).beat_at(frame)
    }

    /// The clip's tempo as the Sample Editor shows it: its
    /// [`tempo`](Self::tempo) with one marker, and with more, the tempo
    /// from the first to the last.
    pub fn clip_tempo(&self, sample_rate: u32) -> f64 {
        match (self.markers.first(), self.markers.last()) {
            (Some(first), Some(last)) if first != last => {
                let beats = (last.beat - first.beat).to_f64();
                60.0 * f64::from(sample_rate) * beats / (last.frame - first.frame) as f64
            }
            _ => self.tempo,
        }
    }

    /// The index of the marker at `beat`, if there's one.
    pub fn marker_at(&self, beat: Beats) -> Option<usize> {
        self.markers.binary_search_by_key(&beat, |m| m.beat).ok()
    }

    /// Adds a marker at `beat` of the clip's own time, pinning the frame
    /// that plays there now, to the nearest whole one, so nothing moves.
    /// Returns its index, or `None`, changing nothing, where there's a
    /// marker already, where the frame is before the file's start, and
    /// where it's the frame of the marker beside it.
    pub fn insert(&mut self, beat: Beats, sample_rate: u32) -> Option<usize> {
        let index = match self.markers.binary_search_by_key(&beat, |m| m.beat) {
            Ok(_) => return None,
            Err(index) => index,
        };
        let frame = self.frame_at(beat.to_f64(), sample_rate).round();
        if frame < 0.0 || frame >= u64::MAX as f64 {
            return None;
        }
        let frame = frame as u64;
        let fits = (index == 0 || self.markers[index - 1].frame < frame)
            && self
                .markers
                .get(index)
                .is_none_or(|next| frame < next.frame);
        if !fits {
            return None;
        }
        self.markers.insert(index, WarpMarker { frame, beat });
        Some(index)
    }

    /// Adds a marker pinning the file's `frame` where it plays now, to the
    /// nearest tick, as for a marker on a transient. Returns its index, or
    /// `None`, changing nothing, where there's a marker at that frame or
    /// that tick already.
    pub fn insert_at_frame(&mut self, frame: u64, sample_rate: u32) -> Option<usize> {
        let beat = Beats::from_f64(self.beat_at(frame as f64, sample_rate));
        let index = self.markers.partition_point(|m| m.frame < frame);
        let fits = (index == 0 || self.markers[index - 1].beat < beat)
            && self
                .markers
                .get(index)
                .is_none_or(|next| frame < next.frame && beat < next.beat);
        if !fits {
            return None;
        }
        self.markers.insert(index, WarpMarker { frame, beat });
        Some(index)
    }

    /// How far the markers with indices `indices` can move together from
    /// `by` on the way there: as far as `by` goes, short of the beats of
    /// the markers either side that stay, so they stay in order.
    pub fn markers_can_move(&self, indices: &[usize], by: Beats) -> Beats {
        let moving = |i: usize| indices.contains(&i);
        let tick = Beats::from_ticks(1);
        let mut by = by;
        for &i in indices {
            let Some(marker) = self.markers.get(i) else {
                continue;
            };
            if by > Beats::ZERO
                && let Some(next) = self.markers.get(i + 1)
                && !moving(i + 1)
            {
                by = by.min(next.beat - marker.beat - tick);
            }
            if by < Beats::ZERO
                && let Some(before) = i.checked_sub(1)
                && !moving(before)
            {
                by = by.max(self.markers[before].beat - marker.beat + tick);
            }
        }
        by
    }

    /// Moves the markers with indices `indices` by `by`, keeping their
    /// frames, so the audio either side stretches. Returns `false`,
    /// changing nothing, if that would take one past a marker that stays
    /// (see [`markers_can_move`](Self::markers_can_move)), or for an index
    /// with no marker.
    pub fn move_markers(&mut self, indices: &[usize], by: Beats) -> bool {
        if indices.iter().any(|&i| i >= self.markers.len())
            || self.markers_can_move(indices, by) != by
        {
            return false;
        }
        for &i in indices {
            self.markers[i].beat += by;
        }
        true
    }

    /// Moves the audio under marker `index`, which stays on its beat, so
    /// the file's `frame` plays there. Returns `false`, changing nothing,
    /// if the frame isn't between the frames of the markers either side,
    /// or there's no such marker.
    pub fn move_audio(&mut self, index: usize, frame: u64) -> bool {
        let Some(&marker) = self.markers.get(index) else {
            return false;
        };
        let after_before = index == 0 || self.markers[index - 1].frame < frame;
        let before_next = self.markers.get(index + 1).is_none_or(|n| frame < n.frame);
        if !(after_before && before_next) {
            return false;
        }
        self.markers[index] = WarpMarker { frame, ..marker };
        true
    }

    /// Deletes the markers with indices `indices`, keeping the first if
    /// they're all of them, as a warp always has one. The audio between the
    /// markers either side stretches to fill them. A marker left on its
    /// own keeps the tempo of the stretch it started, or else of the one
    /// it ended, so the audio around it plays on as it did.
    pub fn delete(&mut self, indices: &[usize], sample_rate: u32) {
        let mut keep: Vec<bool> = (0..self.markers.len())
            .map(|i| !indices.contains(&i))
            .collect();
        if !keep.contains(&true) {
            keep[0] = true;
        }
        let kept = keep.iter().position(|&k| k).expect("one is kept");
        if keep.iter().filter(|&&k| k).count() == 1 && self.markers.len() > 1 {
            let index = kept.min(self.markers.len() - 2);
            self.tempo = clamp_tempo(self.segment(index, sample_rate).tempo(sample_rate));
        }
        let mut i = 0;
        self.markers.retain(|_| {
            i += 1;
            keep[i - 1]
        });
    }

    /// Makes the clip's tempo `factor` times what it is, keeping the frame
    /// that plays at `about`, a beat of the clip's own time, where it is:
    /// the markers spread out from there by `factor`, and the tempo of a
    /// clip with one marker goes up by it, so the file plays `factor` times
    /// slower. Returns `false`, changing nothing, if that takes the clip's
    /// tempo, as [`clip_tempo`](Self::clip_tempo) gives it for a file at
    /// `sample_rate`, out of range, or brings two markers onto one tick.
    pub fn scale(&mut self, factor: f64, about: Beats, sample_rate: u32) -> bool {
        if !(factor.is_finite() && factor > 0.0) {
            return false;
        }
        // A hair either side, for a tempo typed in that the division to
        // the factor and back leaves a hair off.
        let range = Self::MIN_TEMPO * (1.0 - 1e-9)..=Self::MAX_TEMPO * (1.0 + 1e-9);
        if !range.contains(&(self.clip_tempo(sample_rate) * factor)) {
            return false;
        }
        let tempo = self.tempo * factor;
        let spread = |beat: Beats| {
            let from = (beat - about).ticks() as f64 * factor;
            about + Beats::from_ticks(from.round() as i64)
        };
        let markers: Vec<WarpMarker> = self
            .markers
            .iter()
            .map(|m| WarpMarker {
                beat: spread(m.beat),
                ..*m
            })
            .collect();
        if markers.windows(2).any(|w| w[0].beat >= w[1].beat) {
            return false;
        }
        self.markers = markers;
        self.tempo = clamp_tempo(tempo);
        true
    }
}

fn clamp_tempo(tempo: f64) -> f64 {
    if tempo.is_nan() {
        return 120.0;
    }
    tempo.clamp(Warp::MIN_TEMPO, Warp::MAX_TEMPO)
}

/// A tempo a file's name gives.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NamedTempo {
    pub bpm: f64,
    /// Whether "bpm" stands next to it, as in "Drums 120 BPM": otherwise
    /// it's a number on its own, as in "Bass_Am_128", and might be
    /// something else.
    pub marked: bool,
}

/// The tempo `name`, a file's name, gives, if it gives one: a number from
/// 20 to 999 next to "bpm", as in "Drums 120 BPM.wav", "loop_95bpm.aif" or
/// "BPM92.5 Keys.wav", or else a number from 40 to 300 on its own between
/// spaces or punctuation, as in "Bass_Am_128.wav". The first marked one
/// wins, and then the first on its own.
pub fn tempo_in_name(name: &str) -> Option<NamedTempo> {
    // The name without its folder or its extension.
    let stem = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = match stem.rfind('.') {
        Some(dot) if dot > 0 => &stem[..dot],
        _ => stem,
    };
    let lower = stem.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let separator = |b: u8| matches!(b, b' ' | b'_' | b'-');
    let mut on_its_own = None;
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        let end = i;
        let Ok(bpm) = lower[start..end].parse::<f64>() else {
            continue;
        };
        let mut after = end;
        while after < bytes.len() && separator(bytes[after]) {
            after += 1;
        }
        let mut before = start;
        while before > 0 && separator(bytes[before - 1]) {
            before -= 1;
        }
        let marked = lower[after..].starts_with("bpm") || lower[..before].ends_with("bpm");
        if marked && (20.0..=999.0).contains(&bpm) {
            return Some(NamedTempo { bpm, marked });
        }
        let alone = |at: Option<&u8>| at.is_none_or(|&b| !b.is_ascii_alphanumeric() && b != b'.');
        let alone = alone(start.checked_sub(1).map(|b| &bytes[b])) && alone(bytes.get(end));
        if alone && on_its_own.is_none() && (40.0..=300.0).contains(&bpm) {
            on_its_own = Some(NamedTempo { bpm, marked: false });
        }
    }
    on_its_own
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn beats(beats: f64) -> Beats {
        Beats::from_f64(beats)
    }

    fn marker(frame: u64, beat: f64) -> WarpMarker {
        WarpMarker {
            frame,
            beat: beats(beat),
        }
    }

    /// Markers at 120 BPM for two beats, then at 60.
    fn two_tempos() -> Warp {
        Warp {
            markers: vec![marker(0, 0.0), marker(48_000, 2.0), marker(96_000, 3.0)],
            ..Warp::new(0, Beats::ZERO, 100.0)
        }
    }

    #[test]
    fn one_marker_plays_the_file_at_the_clips_tempo() {
        let warp = Warp::new(1_000, beats(1.0), 120.0);
        assert!(warp.is_valid());
        // A beat at 120 BPM is half a second of the file.
        assert_eq!(warp.frame_at(1.0, RATE), 1_000.0);
        assert_eq!(warp.frame_at(2.0, RATE), 25_000.0);
        assert_eq!(warp.frame_at(0.5, RATE), -11_000.0);
        assert_eq!(warp.beat_at(25_000.0, RATE), 2.0);
        assert_eq!(warp.clip_tempo(RATE), 120.0);
        assert_eq!(warp.segments(RATE).count(), 1);
    }

    #[test]
    fn markers_stretch_the_audio_between_them_and_beyond_at_their_neighbours_tempo() {
        let warp = two_tempos();
        assert!(warp.is_valid());
        assert_eq!(warp.frame_at(1.0, RATE), 24_000.0);
        assert_eq!(warp.frame_at(2.5, RATE), 72_000.0);
        // Before the first marker, at the first stretch's 120 BPM, and after
        // the last, at the last's 60.
        assert_eq!(warp.frame_at(-1.0, RATE), -24_000.0);
        assert_eq!(warp.frame_at(4.0, RATE), 144_000.0);
        for frame in [-24_000.0, 0.0, 30_000.0, 48_000.0, 90_000.0, 200_000.0] {
            let beat = warp.beat_at(frame, RATE);
            let back = warp.frame_at(beat, RATE);
            assert!((back - frame).abs() < 1e-6, "frame {frame}: {back}");
        }
        let tempos: Vec<f64> = warp.segments(RATE).map(|s| s.tempo(RATE)).collect();
        assert_eq!(tempos, [120.0, 60.0]);
        // Three beats over two seconds.
        assert_eq!(warp.clip_tempo(RATE), 90.0);
    }

    #[test]
    fn a_new_marker_pins_what_plays_there_already() {
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        assert_eq!(warp.insert(beats(2.0), RATE), Some(1));
        assert_eq!(warp.markers[1], marker(48_000, 2.0));
        assert_eq!(warp.insert(beats(2.0), RATE), None, "a marker is there");
        assert_eq!(warp.insert(beats(-1.0), RATE), None, "before the file");
        assert_eq!(warp.insert(beats(1.0), RATE), Some(1));
        // Nothing moved.
        assert_eq!(warp.frame_at(3.0, RATE), 72_000.0);
        assert_eq!(warp.frame_at(1.5, RATE), 36_000.0);
        // Too close to the marker beside it to have a frame of its own.
        assert_eq!(warp.insert(Beats::from_ticks(3), RATE), None);
    }

    #[test]
    fn markers_move_together_as_far_as_their_neighbours_let_them() {
        let mut warp = two_tempos();
        assert_eq!(warp.markers_can_move(&[1], beats(5.0)), beats(1.0) - tick());
        assert_eq!(
            warp.markers_can_move(&[1], beats(-5.0)),
            beats(-2.0) + tick()
        );
        assert_eq!(warp.markers_can_move(&[1, 2], beats(5.0)), beats(5.0));
        assert!(!warp.move_markers(&[1], beats(1.0)));
        assert!(warp.move_markers(&[1], beats(0.5)));
        // The second's frame is on beat 2.5 now: the second of audio before
        // it fills two and a half beats, at 150 BPM, and the second after it
        // half a beat, at 30.
        assert_eq!(warp.frame_at(2.5, RATE), 48_000.0);
        let tempos: Vec<f64> = warp.segments(RATE).map(|s| s.tempo(RATE)).collect();
        assert_eq!(tempos, [150.0, 30.0]);
        assert!(warp.move_markers(&[0, 1, 2], beats(-10.0)), "all of them");
        assert!(!warp.move_markers(&[3], beats(1.0)), "no such marker");
    }

    fn tick() -> Beats {
        Beats::from_ticks(1)
    }

    #[test]
    fn moving_the_audio_under_a_marker_keeps_its_beat() {
        let mut warp = two_tempos();
        assert!(warp.move_audio(1, 60_000));
        assert_eq!(warp.markers[1], marker(60_000, 2.0));
        assert!(!warp.move_audio(1, 96_000), "onto the next marker's frame");
        assert!(!warp.move_audio(0, 60_000));
        assert!(
            warp.move_audio(2, 1_000_000),
            "the last goes as far as it likes"
        );
        assert!(!warp.move_audio(5, 10));
    }

    #[test]
    fn deleting_markers_keeps_one_at_the_tempo_around_it() {
        let mut warp = two_tempos();
        warp.delete(&[1], RATE);
        assert_eq!(warp.markers, [marker(0, 0.0), marker(96_000, 3.0)]);
        let mut warp = two_tempos();
        warp.delete(&[1, 2], RATE);
        assert_eq!(warp.markers, [marker(0, 0.0)]);
        assert_eq!(warp.tempo, 120.0, "the first stretch's");
        let mut warp = two_tempos();
        warp.delete(&[0, 1], RATE);
        assert_eq!(warp.markers, [marker(96_000, 3.0)]);
        assert_eq!(warp.tempo, 60.0, "the stretch it ended");
        let mut warp = two_tempos();
        warp.delete(&[0, 1, 2], RATE);
        assert_eq!(warp.markers, [marker(0, 0.0)], "one always stays");
    }

    #[test]
    fn scaling_the_tempo_keeps_the_frame_it_scales_about() {
        let mut warp = Warp::new(0, Beats::ZERO, 100.0);
        assert!(warp.scale(2.0, beats(1.0), RATE));
        assert_eq!(warp.tempo, 200.0);
        // Beat 1 played frame 28,800 at 100 BPM, and still does.
        assert_eq!(warp.frame_at(1.0, RATE), 28_800.0);
        assert_eq!(warp.markers[0].beat, beats(-1.0));
        assert!(!warp.scale(10.0, Beats::ZERO, RATE), "past 999 BPM");
        // Exactly to the end of the range, however the factor rounds.
        assert!(warp.scale(999.0 / 200.0, Beats::ZERO, RATE));
        assert_eq!(warp.tempo, 999.0);
        let mut warp = two_tempos();
        assert!(warp.scale(0.5, Beats::ZERO, RATE));
        assert_eq!(warp.markers[2], marker(96_000, 1.5));
        assert_eq!(warp.clip_tempo(RATE), 45.0);
        // With more markers, the clip's tempo from the first to the last
        // stays in range.
        assert!(!warp.scale(1_000.0 / 45.0, Beats::ZERO, RATE));
        assert!(!warp.scale(f64::NAN, Beats::ZERO, RATE));
    }

    #[test]
    fn a_marker_on_a_frame_pins_it_where_it_plays() {
        let mut warp = two_tempos();
        // Halfway through the second stretch, at 60 BPM.
        assert_eq!(warp.insert_at_frame(72_000, RATE), Some(2));
        assert_eq!(warp.markers[2], marker(72_000, 2.5));
        assert_eq!(warp.insert_at_frame(72_000, RATE), None, "one is there");
        // Before the first, at its stretch's 120 BPM, and after the last.
        assert_eq!(warp.insert_at_frame(200_000, RATE), Some(4));
        assert_eq!(warp.markers[4].beat, beats(3.0 + 104_000.0 / 48_000.0));
        let mut warp = Warp::new(24_000, beats(1.0), 120.0);
        assert_eq!(warp.insert_at_frame(0, RATE), Some(0));
        assert_eq!(warp.markers[0], marker(0, 0.0));
        assert!(warp.is_valid());
    }

    fn found(frame: u64) -> Onset {
        Onset {
            frame,
            strength: 0.5,
            percussiveness: 0.25,
            origin: Origin::Found,
        }
    }

    fn visible(onsets: &Onsets) -> Vec<(u64, Origin)> {
        onsets.visible().map(|o| (o.frame, o.origin)).collect()
    }

    #[test]
    fn transients_are_kept_in_order_once_each() {
        let onsets = Onsets::new([found(300), found(100), found(300), found(200)]);
        let frames: Vec<u64> = onsets.all().iter().map(|o| o.frame).collect();
        assert_eq!(frames, [100, 200, 300]);
        assert!(onsets.is_valid());
        let loud = Onset {
            strength: 1.5,
            ..found(400)
        };
        assert!(!Onsets::new([loud]).is_valid());
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        assert_eq!(warp.transients().count(), 0, "none until analysis");
        warp.onsets = Some(onsets);
        assert_eq!(warp.transients().count(), 3);
        assert!(warp.is_valid());
    }

    #[test]
    fn deleted_and_moved_transients_hide_what_analysis_found_until_a_reset() {
        let mut onsets = Onsets::new([found(100), found(200), found(300)]);
        assert_eq!(onsets.delete(&[200, 250]), 1);
        assert_eq!(
            visible(&onsets),
            [(100, Origin::Found), (300, Origin::Found)]
        );
        assert_eq!(onsets.delete(&[200]), 0, "hidden already");
        // Moving one keeps its strength where it goes, and it can't pass
        // those in sight either side.
        assert!(!onsets.move_to(100, 300));
        assert!(!onsets.move_to(100, 100));
        assert!(onsets.move_to(100, 150));
        assert_eq!(onsets.all().len(), 4);
        let moved = onsets.visible().next().copied().unwrap();
        assert_eq!((moved.frame, moved.origin), (150, Origin::Added));
        assert_eq!((moved.strength, moved.percussiveness), (0.5, 0.25));
        // Past the hidden one at 200, which stays hidden.
        assert!(onsets.move_to(150, 250));
        assert_eq!(
            visible(&onsets),
            [(250, Origin::Added), (300, Origin::Found)]
        );
        // Onto it, which brings it back instead.
        assert!(onsets.move_to(250, 200));
        assert_eq!(
            visible(&onsets),
            [(200, Origin::Found), (300, Origin::Found)]
        );
        // An added one goes for good, and a hidden one comes back where one
        // is added.
        assert!(onsets.insert(50));
        let added = onsets.visible().next().copied().unwrap();
        assert_eq!(added.origin, Origin::Added);
        assert_eq!(added.strength, Onsets::ADDED_STRENGTH);
        assert!(!onsets.insert(50));
        assert!(onsets.insert(100));
        assert_eq!(onsets.visible().nth(1).unwrap().origin, Origin::Found);
        onsets.reset();
        let frames: Vec<u64> = onsets.visible().map(|o| o.frame).collect();
        assert_eq!(frames, [100, 200, 300]);
        assert_eq!(onsets, Onsets::new([found(100), found(200), found(300)]));
    }

    #[test]
    fn names_give_tempos_marked_or_on_their_own() {
        let bpm = |name| tempo_in_name(name).map(|t| (t.bpm, t.marked));
        assert_eq!(bpm("Drums 120 BPM.wav"), Some((120.0, true)));
        assert_eq!(bpm("loops/loop_95bpm.aif"), Some((95.0, true)));
        assert_eq!(bpm("BPM92.5 Keys.wav"), Some((92.5, true)));
        assert_eq!(bpm("Bass_Am_128.wav"), Some((128.0, false)));
        assert_eq!(bpm("128-Bass 140bpm.flac"), Some((140.0, true)));
        assert_eq!(bpm("Kick 808.wav"), None);
        assert_eq!(bpm("Kick01.wav"), None);
        assert_eq!(bpm("Pad 2.wav"), None);
        assert_eq!(bpm("Take 3.wav"), None);
        assert_eq!(
            bpm("C:\\Samples\\120\\Clap.wav"),
            None,
            "the folder's no clue"
        );
        assert_eq!(bpm("Vocal 1.25.wav"), None);
        assert_eq!(bpm(""), None);
    }

    #[test]
    fn modes_and_slice_settings_have_keys_steps_and_lengths() {
        for mode in WarpMode::ALL {
            assert_eq!(WarpMode::from_key(mode.key()), Some(mode));
        }
        assert!(!WarpMode::RePitch.transposes() && WarpMode::Slice.transposes());
        assert!(WarpMode::Granular.transposes() && !WarpMode::Granular.renders());
        assert_eq!(WarpMode::from_key("granular"), Some(WarpMode::Granular));
        for (i, preserve) in Preserve::ALL.into_iter().enumerate() {
            assert_eq!(Preserve::from_key(preserve.key()), Some(preserve));
            assert_eq!(preserve.step(), i);
            assert_eq!(Preserve::at_step(i as f64 + 0.4), preserve);
        }
        for (i, looping) in TransientLoop::ALL.into_iter().enumerate() {
            assert_eq!(TransientLoop::from_key(looping.key()), Some(looping));
            assert_eq!(looping.step(), i);
            assert_eq!(TransientLoop::at_step(i as f64 - 0.4), looping);
        }
        // Beyond the steps, the nearest end, and not a number, the first.
        assert_eq!(Preserve::at_step(-3.0), Preserve::Transients);
        assert_eq!(Preserve::at_step(99.0), Preserve::ThirtySecond);
        assert_eq!(TransientLoop::at_step(f64::NAN), TransientLoop::Off);
        let bar = Beats::new(3);
        assert_eq!(Preserve::Transients.length(bar), None);
        assert_eq!(Preserve::Bar.length(bar), Some(bar));
        assert_eq!(Preserve::Half.length(bar), Some(Beats::new(2)));
        assert_eq!(Preserve::Quarter.length(bar), Some(Beats::new(1)));
        assert_eq!(Preserve::ThirtySecond.length(bar), Some(beats(0.125)));
        assert_eq!(pitch_ratio(12.0, 0.0), 2.0);
        assert_eq!(pitch_ratio(0.0, 0.0), 1.0);
        assert!((pitch_ratio(-7.0, 50.0) - (-6.5f64 / 12.0).exp2()).abs() < 1e-12);
    }

    #[test]
    fn a_warp_s_transpose_detune_envelopes_grains_and_formants_stay_in_range() {
        let warp = Warp::new(0, Beats::ZERO, 120.0);
        assert_eq!((warp.transpose, warp.detune), (0, 0));
        assert_eq!(warp.slice, SliceSettings::default());
        assert_eq!(warp.slice.transient_loop, TransientLoop::Forward);
        assert_eq!(warp.slice.envelope, SliceSettings::ENVELOPE_MAX);
        assert!(warp.granular.follow_pitch);
        assert_eq!(warp.granular.grain_size, 30.0);
        assert_eq!(warp.granular.fluctuation, 25.0);
        assert!(!warp.spectral.high_quality);
        assert_eq!(warp.spectral.formants, SpectralSettings::FORMANTS_MAX);
        assert_eq!(warp.spectral.envelope, 128.0);
        let ends = |spectral_envelope| Warp {
            transpose: -Warp::TRANSPOSE_MAX,
            detune: Warp::DETUNE_MAX,
            slice: SliceSettings {
                envelope: 0.0,
                ..warp.slice
            },
            granular: GranularSettings {
                follow_pitch: false,
                grain_size: GranularSettings::GRAIN_SIZE_MAX,
                fluctuation: GranularSettings::FLUCTUATION_MAX,
            },
            spectral: SpectralSettings {
                high_quality: true,
                formants: 0.0,
                envelope: spectral_envelope,
            },
            ..warp.clone()
        };
        assert!(ends(SpectralSettings::ENVELOPE_MIN).is_valid());
        assert!(ends(SpectralSettings::ENVELOPE_MAX).is_valid());
        let envelope = |envelope| Warp {
            slice: SliceSettings {
                envelope,
                ..warp.slice
            },
            ..warp.clone()
        };
        let grains = |grain_size, fluctuation| Warp {
            granular: GranularSettings {
                grain_size,
                fluctuation,
                ..warp.granular
            },
            ..warp.clone()
        };
        assert!(grains(GranularSettings::GRAIN_SIZE_MIN, 0.0).is_valid());
        let formants = |formants| Warp {
            spectral: SpectralSettings {
                formants,
                ..warp.spectral
            },
            ..warp.clone()
        };
        let spectral_envelope = |envelope| Warp {
            spectral: SpectralSettings {
                envelope,
                ..warp.spectral
            },
            ..warp.clone()
        };
        for wrong in [
            Warp {
                transpose: 49,
                ..warp.clone()
            },
            Warp {
                detune: -51,
                ..warp.clone()
            },
            envelope(100.5),
            envelope(-0.5),
            envelope(f64::NAN),
            grains(4.9, 0.0),
            grains(501.0, 0.0),
            grains(f64::NAN, 0.0),
            grains(30.0, -1.0),
            grains(30.0, 100.5),
            grains(30.0, f64::NAN),
            formants(-1.0),
            formants(f64::NAN),
            spectral_envelope(7.5),
            spectral_envelope(513.0),
            spectral_envelope(f64::NAN),
        ] {
            assert!(!wrong.is_valid(), "{wrong:?}");
        }
    }

    #[test]
    fn the_clip_s_parameters_set_its_warp_s_settings_in_range_and_in_steps() {
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        for (param, value) in [
            (ClipParam::Transpose, 7.4),
            (ClipParam::Detune, -80.0),
            (ClipParam::TransientEnvelope, 37.5),
            (ClipParam::Preserve, 4.6),
            (ClipParam::TransientLoop, 9.0),
            (ClipParam::GrainSize, 1.0),
            (ClipParam::Fluctuation, 62.5),
            (ClipParam::Formants, 62.5),
            (ClipParam::Envelope, 1000.0),
            (ClipParam::Gain, 0.5),
            (ClipParam::Transpose, f64::NAN),
        ] {
            warp.set_param(param, value);
        }
        assert_eq!((warp.transpose, warp.detune), (7, -50));
        assert_eq!(warp.slice.envelope, 37.5);
        assert_eq!(warp.slice.preserve, Preserve::Sixteenth);
        assert_eq!(warp.slice.transient_loop, TransientLoop::BackAndForth);
        assert_eq!(warp.granular.grain_size, GranularSettings::GRAIN_SIZE_MIN);
        assert_eq!(warp.granular.fluctuation, 62.5);
        assert_eq!(warp.spectral.formants, 62.5);
        assert_eq!(warp.spectral.envelope, SpectralSettings::ENVELOPE_MAX);
        assert!(warp.is_valid());
        let values = ClipParam::ALL.map(|param| warp.param(param));
        assert_eq!(
            values,
            [
                None,
                Some(7.0),
                Some(-50.0),
                Some(37.5),
                Some(5.0),
                Some(2.0),
                Some(5.0),
                Some(62.5),
                Some(62.5),
                Some(512.0),
            ]
        );
    }

    #[test]
    fn formants_in_place_move_none_of_the_way_and_at_0_all_of_it() {
        let ratio = 1.5;
        assert_eq!(SpectralSettings::formant_ratio(100.0, ratio), 1.0);
        assert_eq!(SpectralSettings::formant_ratio(0.0, ratio), ratio);
        let half = SpectralSettings::formant_ratio(50.0, ratio);
        assert!((half - ratio.sqrt()).abs() < 1e-12, "{half}");
        // Whatever Formants is, nothing moves where nothing's transposed.
        assert_eq!(SpectralSettings::formant_ratio(37.0, 1.0), 1.0);
    }
}
