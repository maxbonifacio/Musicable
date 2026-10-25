//! Granular: a warped clip's audio played as short overlapping grains,
//! each at the file's own speed from where it lands in the song, as Live's
//! Tones plays it with Follow pitch on and Texture with it off.
//!
//! A clip's [`Grains`] are placed when the graph is built, at the song's
//! tempo, in seconds, and play at the device's rate, whatever it is. Each
//! grain is centred somewhere in the clip's own time and reads the file
//! around where the warp puts it, at the file's own speed, or faster or
//! slower by its transpose and detune, resampled.
//! It rises from the grain before's centre to its own and falls to the
//! next's, through raised cosines, so two grains play at once and their
//! levels add up to one. Where the song plays the file slower than its own
//! speed, the grains go over some of its audio again, and where faster,
//! they leave some out, so the audio keeps its pitch at any speed.
//!
//! The grains are half their Grain Size apart, on a lattice from the
//! clip's own time 0, so splitting a clip leaves its grains where they
//! were. With Follow pitch off, Fluctuation spreads each grain, earlier or
//! later and reading from further back or on, by a share of its length
//! drawn from its place on the lattice, so it's the same every time. With
//! Follow pitch on, where analysis has found the file's pitch marks, where
//! each period of its pitch starts, each grain is a whole number of
//! periods on from the one before, about half its Grain Size, and reads
//! from the mark nearest where the warp puts it (TD-PSOLA), so the periods
//! of grains that overlap line up, and a line with one note at a time keeps
//! its sound. Elsewhere, and until analysis has found the marks, the
//! grains keep to the lattice.
//!
//! Each grain plays with the settings its clip's curves have at its
//! centre, or else its warp's, so automation moves them a grain at a time.
//! Where the song plays the file at its own speed wherever the clip plays,
//! and nothing transposes, the clip plays the file as Re-Pitch does,
//! sample for sample. When the tempo changes, the grains keep their places
//! in the clip's own time until the graph is built again, so they move
//! with its beats.

use std::collections::VecDeque;

use musicable_model::clip::{AudioClip, Clip, ClipContent};
use musicable_model::param::ClipParam;
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::{Warp, WarpMode, pitch_ratio};

use super::Run;
use super::slice::{Ramp, Shape, Song};
use super::spectral::{Map, Value};

/// How far Fluctuation moves a grain's centre at most, at 100, as a share
/// of the way to the nearer of its neighbours on the lattice: so grains
/// stay in order, and none rises or falls in less than a fifth of the way.
const SPREAD_IN_TIME: f64 = 0.4;

/// How near two grains are centred at least where marks and the lattice
/// meet, in seconds: no grain rises or falls faster.
const CLOSEST_SECONDS: f64 = 0.0005;

/// What every clip's spread is drawn from, with each grain's place on the
/// lattice.
const SEED: u64 = 0x9e6c_63d0_676a_9a99;

/// How far apart, in seconds of the file, a clip's reads can be at once in
/// what a stream holds anyway, which leaves the rest of it for the song to
/// play on into while the disk catches up.
const HELD_SECONDS: f64 = 0.5;

/// Where a file's pitch periods start, in the stretches of it with a
/// pitch, as analysis found them: what Granular fits its grains to with
/// Follow pitch on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PitchMarks {
    /// In order, apart: each the frames of the file its marks are on,
    /// fractional, rising, two at least.
    runs: Vec<Vec<f64>>,
}

impl PitchMarks {
    /// The marks of `runs`, each the frames of the file its periods start
    /// on, rising. Runs of fewer than two marks are left out.
    pub fn new(runs: Vec<Vec<f64>>) -> Self {
        let runs = runs.into_iter().filter(|run| run.len() >= 2).collect();
        Self { runs }
    }

    pub fn runs(&self) -> &[Vec<f64>] {
        &self.runs
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// A clip's grains, as Granular plays them, placed at one tempo map.
#[derive(Debug)]
pub(super) struct Grains {
    /// The song's tempo the grains were placed at, up to the clip's end.
    tempo: TempoMap,
    until: Beats,
    /// The file's sample rate.
    rate: u32,
    /// Whether the clip plays the file as it is at that tempo.
    passthrough: bool,
    /// In order of their centres, from before the clip starts to after it
    /// ends, the first and last only shaping their neighbours.
    grains: Vec<Grain>,
}

/// One grain: where it's centred, and how it reads the file.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grain {
    /// Its centre, in the clip's own time.
    own: f64,
    /// The file's position it reads on its centre, fractional.
    at: f64,
    /// How much faster than the file's own speed it reads it.
    pitch: f64,
}

/// A grain as it's placed: centred on grid frame `centre`, fractional, the
/// song's frames at the file's sample rate.
#[derive(Debug, Clone, Copy)]
struct PlacedGrain {
    centre: f64,
    grain: Grain,
}

/// A clip's Transpose, Detune, Grain Size and Fluctuation, as its curves
/// and its warp have them.
struct Settings {
    transpose: Value,
    detune: Value,
    grain_size: Value,
    fluctuation: Value,
}

impl Settings {
    fn new(audio: &AudioClip, warp: &Warp) -> Self {
        let value = |param, otherwise| Value::new(audio, param, otherwise);
        Self {
            transpose: value(ClipParam::Transpose, f64::from(warp.transpose)),
            detune: value(ClipParam::Detune, f64::from(warp.detune)),
            grain_size: value(ClipParam::GrainSize, warp.granular.grain_size),
            fluctuation: value(ClipParam::Fluctuation, warp.granular.fluctuation),
        }
    }

    /// How much faster than its own speed a grain at `own` in the clip's
    /// own time plays the file.
    fn pitch(&self, own: f64) -> f64 {
        pitch_ratio(self.transpose.at(own), self.detune.at(own))
    }
}

impl Grains {
    /// The grains of `clip`, playing a file at `file_rate`, fitted to the
    /// file's `marks` with Follow pitch on, in a song whose tempo is
    /// `tempo`. `None` for a clip that isn't warped in Granular.
    pub(super) fn new(
        clip: &Clip,
        marks: Option<&PitchMarks>,
        file_rate: u32,
        tempo: &TempoMap,
    ) -> Option<Self> {
        let (placed, passthrough) = place(clip, marks, file_rate, tempo)?;
        Some(Self {
            tempo: tempo.clone(),
            until: clip.end(),
            rate: file_rate,
            passthrough,
            grains: placed.into_iter().map(|p| p.grain).collect(),
        })
    }

    /// Whether, at `tempo`, the clip plays the file as Re-Pitch does: as it
    /// did when its grains were placed, if `tempo` places the song's beats
    /// as theirs did up to the clip's end.
    pub(super) fn plays_as_is(&self, tempo: &TempoMap) -> bool {
        self.passthrough && super::same_tempo(&self.tempo, tempo, self.until)
    }

    /// Calls `each` with the runs that play on the song's frames from
    /// `from` up to `to`: a grain each, in order, each overlapping the one
    /// before and the one after.
    pub(super) fn runs(&self, song: &Song<'_>, from: i64, to: i64, mut each: impl FnMut(Run)) {
        let grains = &self.grains;
        if grains.len() < 3 {
            return;
        }
        // The song frame, fractional, each grain is centred on.
        let centre = |j: usize| song.frame_of(grains[j].own);
        // The file's frames a song frame plays at the file's own speed.
        let speed = f64::from(self.rate) / f64::from(song.rate);
        // The last grain centred on or before `from`, whose fall it's in.
        let mut j = (grains.partition_point(|g| song.frame_of(g.own) <= from as f64))
            .saturating_sub(1)
            .max(1);
        let (mut before, mut middle) = (centre(j - 1), centre(j));
        while j + 1 < grains.len() {
            let after = centre(j + 1);
            let first = before.ceil() as i64;
            if first >= to {
                break;
            }
            let (a, b) = (from.max(first), to.min(after.ceil() as i64));
            if a < b {
                let grain = &grains[j];
                let step = grain.pitch * speed;
                let anchor = middle.ceil() as i64;
                let shape = Shape::default()
                    .with(Ramp::rising(before, middle - before))
                    .with(Ramp::falling(middle, after - middle));
                each(Run {
                    from: a,
                    to: b,
                    anchor,
                    at: grain.at + (anchor as f64 - middle) * step,
                    step,
                    shape,
                    back_to: None,
                });
            }
            (before, middle) = (middle, after);
            j += 1;
        }
    }
}

/// The grains of `clip`, as [`Grains::new`] places them, in order, and
/// whether the clip plays the file as it is.
fn place(
    clip: &Clip,
    marks: Option<&PitchMarks>,
    file_rate: u32,
    tempo: &TempoMap,
) -> Option<(Vec<PlacedGrain>, bool)> {
    let ClipContent::Audio(audio) = &clip.content else {
        return None;
    };
    let warp = audio
        .warp
        .as_ref()
        .filter(|w| w.mode == WarpMode::Granular)?;
    let offset = audio.offset.beats()?.to_f64();
    if file_rate == 0 {
        return None;
    }
    let shift = offset - clip.start.to_f64();
    let map = Map {
        shift,
        segments: warp.segments(file_rate).collect(),
        rate: file_rate,
    };
    let song = Song {
        tempo,
        rate: file_rate,
        shift,
    };
    let settings = Settings::new(audio, warp);
    let passthrough = settings.transpose.is_zero()
        && settings.detune.is_zero()
        && map.plays_as_is(tempo, clip.start, clip.end());
    let (start, end) = (
        song.frame_of(offset),
        song.frame_of(offset + clip.length.to_f64()),
    );
    let placing = Placing {
        song,
        map: &map,
        settings: &settings,
    };
    let marks = marks.filter(|_| warp.granular.follow_pitch);
    let chains: Vec<Vec<PlacedGrain>> = (marks.map(PitchMarks::runs).unwrap_or_default().iter())
        .filter_map(|run| placing.chain(run, start, end))
        .collect();
    let spread = !warp.granular.follow_pitch;
    let mut placed = placing.lattice(start, end, spread, &chains);
    placed.extend(chains.into_iter().flatten());
    placed.sort_by(|a, b| a.centre.total_cmp(&b.centre));
    // Where marks and the lattice meet, no two grains too near.
    let closest = CLOSEST_SECONDS * f64::from(file_rate);
    placed.dedup_by(|later, earlier| later.centre - earlier.centre < closest);
    Some((placed, passthrough))
}

/// How much more of its file than other clips' a stream of `clip`'s file
/// holds, in seconds of the file, for its grains, which read further apart
/// at once the longer they are and the faster or slower than the warp they
/// read: as far apart as they read, placed for a file at `file_rate` with
/// pitch `marks` at `tempo`, in steps of twice as far, so an edit seldom
/// needs a new stream. 0 where they read no further apart than streams hold
/// anyway, and for a clip that isn't in Granular. See
/// [`Streamer::stream_spread`](crate::media::Streamer::stream_spread).
pub fn stream_spread(
    clip: &Clip,
    marks: Option<&PitchMarks>,
    file_rate: u32,
    tempo: &TempoMap,
) -> f64 {
    let Some((placed, _)) = place(clip, marks, file_rate, tempo) else {
        return 0.0;
    };
    let rate = f64::from(file_rate);
    let spread = widest(&placed, super::GRAINS_AHEAD_SECONDS * rate) / rate;
    match spread <= HELD_SECONDS {
        true => 0.0,
        false => spread.log2().ceil().exp2(),
    }
}

/// How far apart, in frames of the file, the grains `placed` read at most
/// at once: from one grain's centre to the next's, the two that play read
/// from where they are on to where they are at the later centre, and the
/// stream is also told where those that start in the next `ahead` grid
/// frames read first.
fn widest(placed: &[PlacedGrain], ahead: f64) -> f64 {
    let count = placed.len();
    if count < 3 {
        return 0.0;
    }
    // Where each grain reads first, rising from the one before's centre.
    let first: Vec<f64> = (0..count)
        .map(|i| {
            let PlacedGrain { centre, grain } = placed[i];
            grain.at + grain.pitch * (placed[i.saturating_sub(1)].centre - centre)
        })
        .collect();
    // Those that start next, from the one after next on, lowest first read
    // in front.
    let mut starting = VecDeque::<usize>::new();
    let mut next = 2;
    let mut most: f64 = 0.0;
    for i in 0..count - 1 {
        let (now, then) = (placed[i], placed[i + 1]);
        while next < count && placed[next - 1].centre <= then.centre + ahead {
            while starting.back().is_some_and(|&j| first[j] >= first[next]) {
                starting.pop_back();
            }
            starting.push_back(next);
            next += 1;
        }
        while starting.front().is_some_and(|&j| j < i + 2) {
            starting.pop_front();
        }
        let falls_to = now.grain.at + now.grain.pitch * (then.centre - now.centre);
        let high = falls_to.max(then.grain.at);
        let low = (starting.front().map(|&j| first[j]))
            .unwrap_or(f64::INFINITY)
            .min(now.grain.at)
            .min(first[i + 1]);
        most = most.max(high - low);
    }
    most
}

/// What placing a clip's grains needs: the song on grid frames.
struct Placing<'a> {
    song: Song<'a>,
    map: &'a Map,
    settings: &'a Settings,
}

impl Placing<'_> {
    /// Half the Grain Size at grid frame `frame`, in grid frames: how far
    /// apart grains centre there.
    fn half(&self, frame: f64) -> f64 {
        let size = self.settings.grain_size.at(self.song.own_at(frame));
        size / 2000.0 * f64::from(self.song.rate)
    }

    /// The grain centred on grid frame `centre`, reading `offset` frames of
    /// the file on from where the warp puts the song there.
    fn grain(&self, centre: f64, offset: f64) -> PlacedGrain {
        let own = self.song.own_at(centre);
        let grain = Grain {
            own,
            at: self.map.frame_of(own) + offset,
            pitch: self.settings.pitch(own),
        };
        PlacedGrain { centre, grain }
    }

    /// The grains on the lattice around grid frames `start` to `end`, as
    /// [`points`](Self::points) places them, but those the `chains` of
    /// grains on pitch marks take the place of: spread by Fluctuation if
    /// `spread`.
    fn lattice(
        &self,
        start: f64,
        end: f64,
        spread: bool,
        chains: &[Vec<PlacedGrain>],
    ) -> Vec<PlacedGrain> {
        let points = self.points(start, end);
        // Where each chain plays, from its first centre to its last.
        let spans: Vec<(f64, f64)> = (chains.iter())
            .map(|c| (c[0].centre, c[c.len() - 1].centre))
            .collect();
        let taken = |at: f64, half: f64| {
            let k = spans.partition_point(|s| s.1 + half / 2.0 < at);
            spans.get(k).is_some_and(|s| s.0 - half / 2.0 <= at)
        };
        let mut grains = Vec::with_capacity(points.len());
        for (i, &(k, at)) in points.iter().enumerate() {
            let half = self.half(at);
            if taken(at, half) {
                continue;
            }
            let fluctuation = match spread {
                true => self.settings.fluctuation.at(self.song.own_at(at)) / 100.0,
                false => 0.0,
            };
            if fluctuation <= 0.0 {
                grains.push(self.grain(at, 0.0));
                continue;
            }
            // Within the nearer of its neighbours on the lattice.
            let gap = |j: Option<usize>| j.and_then(|j| points.get(j)).map(|p| (p.1 - at).abs());
            let room = match (gap(i.checked_sub(1)), gap(Some(i + 1))) {
                (Some(a), Some(b)) => a.min(b),
                (a, b) => a.or(b).unwrap_or(half),
            };
            let (early, back) = spread_of(k);
            let centre = at + SPREAD_IN_TIME * fluctuation * early * room;
            grains.push(self.grain(centre, fluctuation * back * half));
        }
        grains
    }

    /// The lattice's points around grid frames `start` to `end`, as their
    /// places on it and their grid frames: from the clip's own time 0, each
    /// half the Grain Size there on from the one before, or back from the
    /// one after before it, from three before the last on or before `start`
    /// to two after the first on or after `end`.
    fn points(&self, start: f64, end: f64) -> Vec<(i64, f64)> {
        let origin = self.song.frame_of(0.0);
        if let Value::Fixed(_) = self.settings.grain_size {
            let half = self.half(origin);
            let first = ((start - origin) / half).floor() as i64 - 3;
            let last = ((end - origin) / half).ceil() as i64 + 2;
            return (first..=last)
                .map(|k| (k, origin + k as f64 * half))
                .collect();
        }
        // With a curve, walked from the origin each way, so a point is the
        // same wherever the clip starts: back to the fourth on or before
        // `start`, and on to the third on or after `end`, keeping the last
        // four on or before `start` on the way.
        let mut back = Vec::new();
        let (mut k, mut at) = (0i64, origin);
        let mut before = 0;
        while before < 4 {
            at -= self.half(at);
            k -= 1;
            back.push((k, at));
            before += usize::from(at <= start);
        }
        back.reverse();
        let mut on = VecDeque::new();
        let (mut k, mut at) = (0i64, origin);
        let mut after = 0;
        loop {
            on.push_back((k, at));
            if at <= start && on.len() > 4 {
                on.pop_front();
            }
            after += usize::from(at >= end);
            if after == 3 {
                break;
            }
            at += self.half(at);
            k += 1;
        }
        let mut points: Vec<_> = back.into_iter().chain(on).collect();
        let first_after = (points.iter().position(|p| p.1 >= end)).unwrap_or(points.len());
        points.truncate(first_after + 3);
        let last_before = (points.iter().rposition(|p| p.1 <= start)).unwrap_or(0);
        points.drain(..last_before.saturating_sub(3));
        points
    }

    /// The grains on the pitch marks of `run`, frames of the file, that
    /// shape what the clip plays between grid frames `start` and `end`:
    /// each a whole number of periods on from the one before, about half
    /// its Grain Size, reading from the mark nearest where the warp puts
    /// the song. `None` where the clip doesn't play the run.
    fn chain(&self, run: &[f64], start: f64, end: f64) -> Option<Vec<PlacedGrain>> {
        let last = run.len() - 1;
        let frame_of = |mark: f64| self.song.frame_of(self.map.own_of(mark));
        let (from, to) = (start - 3.0 * self.half(start), end + 3.0 * self.half(end));
        let until = frame_of(run[last]);
        let mut centre = frame_of(run[0]);
        if until < from || centre > to {
            return None;
        }
        // Walked from the run's start, so a grain is the same wherever the
        // clip starts, and kept from two before `from`, three halves of a
        // grain before the clip, to two after `to`, as they shape those
        // between.
        let mut chain: Vec<PlacedGrain> = Vec::new();
        let mut past = 0;
        while centre <= until && past < 2 {
            let own = self.song.own_at(centre);
            let position = self.map.frame_of(own);
            let i = nearest_mark(run, position);
            let pitch = self.settings.pitch(own);
            let grain = Grain {
                own,
                at: run[i],
                pitch,
            };
            if centre < from && chain.len() == 2 {
                chain.remove(0);
            }
            if centre > to {
                past += 1;
            }
            chain.push(PlacedGrain { centre, grain });
            // The period after the mark, or before the last, as many as
            // fit half the Grain Size where they play, at the grain's pitch.
            let k = (i + 1).min(last);
            let period = run[k] - run[k - 1];
            let periods = (self.half(centre) * pitch / period).round().max(1.0) as usize;
            let on = match run.get(i + periods) {
                Some(&mark) => mark - run[i],
                None => run[last] - run[i] + (i + periods - last) as f64 * period,
            };
            centre += on / pitch;
        }
        Some(chain)
    }
}

/// The mark of `run` nearest the file's `position`.
fn nearest_mark(run: &[f64], position: f64) -> usize {
    let after = run.partition_point(|&m| m < position);
    match (after.checked_sub(1), run.get(after)) {
        (Some(before), Some(&next)) if next - position < position - run[before] => after,
        (Some(before), _) => before,
        (None, _) => 0,
    }
}

/// How far the grain at place `k` on the lattice strays, each from -1 to 1:
/// how early or late, and how far back or on it reads.
fn spread_of(k: i64) -> (f64, f64) {
    // SplitMix64.
    let mut state = (k as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ SEED;
    let mut next = || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let unit = |bits: u64| (bits >> 11) as f64 / (1u64 << 52) as f64 - 1.0;
    (unit(next()), unit(next()))
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;
    use std::sync::Arc;

    use musicable_model::automation::Point;
    use musicable_model::clip::{ClipId, Offset};
    use musicable_model::time::TimeSignature;

    use super::*;
    use crate::clips::{AudioClips, Reading};
    use crate::media::{AudioData, DataReader, Source, Streamer};
    use crate::{AudioBuffer, EngineConfig, Inputs, Node, ProcessContext, TransportState, rt};

    const RATE: u32 = 48_000;

    /// The tone the files play, in Hz: a period every 320 frames.
    const TONE: f64 = 150.0;

    fn song(bpm: f64) -> TempoMap {
        TempoMap::new(bpm, TimeSignature::FOUR_FOUR)
    }

    /// The frame `seconds` in.
    fn at(seconds: f64) -> usize {
        (seconds * f64::from(RATE)).round() as usize
    }

    /// A mono file, `seconds` long, of a tone at `frequency` from `from` to
    /// `to` seconds, silent elsewhere.
    fn file_of(frequency: f64, seconds: f64, from: f64, to: f64) -> Arc<AudioData> {
        let wave = |t: f64| match (from..to).contains(&t) {
            true => 0.5 * (TAU * frequency * (t - from)).sin(),
            false => 0.0,
        };
        let samples = (0..at(seconds)).map(|i| wave(i as f64 / f64::from(RATE)) as f32);
        Arc::new(AudioData::new(RATE, vec![samples.collect()]))
    }

    /// A file of [`TONE`], as [`file_of`] makes one.
    fn file(seconds: f64, from: f64, to: f64) -> Arc<AudioData> {
        file_of(TONE, seconds, from, to)
    }

    /// The pitch marks of a tone at `frequency` from `from` to `to` seconds
    /// of a file: where each of its periods starts.
    fn marks_of(frequency: f64, from: f64, to: f64) -> PitchMarks {
        let period = f64::from(RATE) / frequency;
        let count = ((to - from) * frequency).round() as usize;
        let start = from * f64::from(RATE);
        PitchMarks::new(vec![
            (0..=count).map(|k| start + k as f64 * period).collect(),
        ])
    }

    fn marks(from: f64, to: f64) -> PitchMarks {
        marks_of(TONE, from, to)
    }

    /// A clip from beat 0, `length` beats long, of a file warped in Granular
    /// at `tempo` from its start, following the pitch if `follow_pitch`,
    /// spread by `fluctuation`.
    fn granular(length: f64, tempo: f64, follow_pitch: bool, fluctuation: f64) -> Clip {
        let mut audio = AudioClip::new("tone.wav");
        let mut warp = Warp::new(0, Beats::ZERO, tempo);
        warp.mode = WarpMode::Granular;
        warp.granular.follow_pitch = follow_pitch;
        warp.granular.fluctuation = fluctuation;
        audio.warp = Some(warp);
        audio.offset = Offset::Beats(Beats::ZERO);
        Clip {
            id: ClipId(0),
            name: String::new(),
            start: Beats::ZERO,
            length: Beats::from_f64(length),
            content: ClipContent::Audio(audio),
        }
    }

    fn audio_of(clip: &mut Clip) -> &mut AudioClip {
        match &mut clip.content {
            ClipContent::Audio(audio) => audio,
            ClipContent::Midi(_) => unreachable!("an audio clip"),
        }
    }

    fn warp_of(clip: &mut Clip) -> &mut Warp {
        audio_of(clip).warp.as_mut().expect("a warped clip")
    }

    fn point(beat: f64, value: f64) -> Point {
        Point::new(Beats::from_f64(beat), value)
    }

    /// A player of `clips`, playing `source`, with its file's pitch `marks`,
    /// at `tempo`.
    fn player_of(
        clips: &[&Clip],
        source: Source,
        marks: Option<&PitchMarks>,
        tempo: &TempoMap,
    ) -> AudioClips {
        let mut player = AudioClips::new();
        for clip in clips {
            player.add_rendered(clip, Some(source.clone()), None, marks, tempo);
        }
        player
    }

    fn player(
        clips: &[&Clip],
        data: &Arc<AudioData>,
        marks: Option<&PitchMarks>,
        tempo: &TempoMap,
    ) -> AudioClips {
        player_of(clips, Source::Whole(data.clone()), marks, tempo)
    }

    /// Plays `player` from the song's start for `frames` frames, in blocks
    /// of up to `block`, at `tempo`: its left channel.
    fn play(mut player: AudioClips, tempo: &TempoMap, frames: usize, block: usize) -> Vec<f32> {
        let config = EngineConfig {
            sample_rate: RATE,
            max_block: block,
        };
        player.prepare(&config);
        let mut buffer = AudioBuffer::new(2, block);
        let mut out = Vec::with_capacity(frames);
        while out.len() < frames {
            let n = block.min(frames - out.len());
            let ctx = ProcessContext {
                frames: n,
                sample_rate: RATE,
                time: 0,
                transport: TransportState {
                    playing: true,
                    position: out.len() as u64,
                    ..TransportState::default()
                },
                tempo,
                events: &[],
                input: crate::AudioRef::empty(n),
                midi: &[],
            };
            rt::scope(|| player.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n)));
            out.extend_from_slice(buffer.view(n).channel(0));
        }
        out
    }

    fn level(signal: &[f32]) -> f64 {
        let power: f64 = signal.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        (power / signal.len() as f64).sqrt()
    }

    /// The lowest and highest of `signal`'s levels over each of the tone's
    /// periods.
    fn levels(signal: &[f32]) -> (f64, f64) {
        let period = (f64::from(RATE) / TONE).round() as usize;
        (signal.chunks_exact(period).map(level)).fold((f64::INFINITY, 0.0), |(low, high), l| {
            (low.min(l), high.max(l))
        })
    }

    /// How strong `signal` is at `frequency`, in Hz, through a Hann window.
    fn strength(signal: &[f32], frequency: f64) -> f64 {
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &sample) in signal.iter().enumerate() {
            let window = 0.5 - 0.5 * (TAU * i as f64 / n).cos();
            let phase = TAU * frequency * i as f64 / f64::from(RATE);
            re += f64::from(sample) * window * phase.cos();
            im += f64::from(sample) * window * phase.sin();
        }
        re.hypot(im)
    }

    /// How often `signal` crosses zero upwards, in Hz: from the first time
    /// to the last, between samples.
    fn frequency(signal: &[f32]) -> f64 {
        let ups: Vec<f64> = (signal.windows(2).enumerate())
            .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
            .map(|(i, w)| i as f64 + f64::from(w[0] / (w[0] - w[1])))
            .collect();
        let seconds = (ups[ups.len() - 1] - ups[0]) / f64::from(RATE);
        (ups.len() - 1) as f64 / seconds
    }

    /// The largest step from one sample to the next.
    fn steepest(signal: &[f32]) -> f32 {
        (signal.windows(2))
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_file_at_its_own_speed_untransposed_plays_sample_for_sample() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.0, 4.0);
        let tempo = song(120.0);
        for follow_pitch in [false, true] {
            // From beat 1 of the song, the file from its start.
            let mut clip = granular(6.0, 120.0, follow_pitch, 100.0);
            clip.start = Beats::from_f64(1.0);
            let grains = Grains::new(&clip, Some(&marks), RATE, &tempo).expect("in Granular");
            assert!(grains.plays_as_is(&tempo));
            assert!(!grains.plays_as_is(&song(100.0)));
            let out = play(
                player(&[&clip], &data, Some(&marks), &tempo),
                &tempo,
                96_000,
                512,
            );
            let beat = 24_000;
            assert!(out[..beat].iter().all(|&s| s == 0.0));
            assert!(out[beat..] == data.channel(0)[..3 * beat], "{follow_pitch}");
            // Not transposed.
            warp_of(&mut clip).transpose = 1;
            let grains = Grains::new(&clip, Some(&marks), RATE, &tempo).expect("in Granular");
            assert!(!grains.plays_as_is(&tempo));
        }
    }

    #[test]
    fn grains_keep_a_tones_pitch_at_any_speed() {
        // The file's eight beats at 120 BPM, played at 0.8 and 1.25 times
        // its speed: a low tone on its marks keeps its pitch exactly, and a
        // high one on the lattice to within a grain's worth, as grains
        // overlapping out of step blur it.
        for (tone, follow_pitch, within) in [(TONE, true, 0.002), (1100.0, false, 0.05)] {
            let data = file_of(tone, 4.0, 0.0, 4.0);
            let marks = marks_of(tone, 0.0, 4.0);
            for bpm in [96.0, 150.0] {
                let tempo = song(bpm);
                let clip = granular(8.0, 120.0, follow_pitch, 25.0);
                let player = player(&[&clip], &data, Some(&marks), &tempo);
                let heard = frequency(&play(player, &tempo, at(2.5), 512)[at(0.5)..at(2.5)]);
                assert!(
                    (heard / tone - 1.0).abs() < within,
                    "{bpm} BPM, Follow pitch {follow_pitch}: {heard} Hz for {tone}"
                );
            }
        }
    }

    #[test]
    fn following_the_pitch_lines_up_the_periods_of_grains_that_overlap() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.0, 4.0);
        for bpm in [96.0, 150.0] {
            let tempo = song(bpm);
            let levels_of = |follow_pitch: bool| {
                let clip = granular(8.0, 120.0, follow_pitch, 0.0);
                let player = player(&[&clip], &data, Some(&marks), &tempo);
                levels(&play(player, &tempo, at(2.5), 512)[at(0.5)..at(2.5)])
            };
            // On the lattice, grains that overlap read the tone out of
            // step, and cancel out where they cross.
            let (low, high) = levels_of(false);
            assert!(
                low < 0.5 * high,
                "{bpm} BPM, on the lattice: {low} to {high}"
            );
            // On its marks, they read it whole periods apart.
            let (low, high) = levels_of(true);
            assert!(
                low > 0.98 * high,
                "{bpm} BPM, on the marks: {low} to {high}"
            );
            assert!((high - 0.5 / 2f64.sqrt()).abs() < 0.01, "{bpm} BPM: {high}");
        }
    }

    #[test]
    fn grains_on_the_marks_are_whole_periods_apart_and_read_from_a_mark() {
        let marks = marks(0.0, 4.0);
        let period = f64::from(RATE) / TONE;
        for (bpm, transpose) in [(96.0, 0), (150.0, 0), (120.0, 7), (96.0, -5)] {
            let mut clip = granular(8.0, 120.0, true, 0.0);
            warp_of(&mut clip).transpose = transpose;
            let tempo = song(bpm);
            let grains = Grains::new(&clip, Some(&marks), RATE, &tempo).expect("in Granular");
            let pitch = pitch_ratio(f64::from(transpose), 0.0);
            // At this tempo, a beat of the clip's own time is this many
            // seconds.
            let beat = 60.0 / bpm;
            let middle = (grains.grains.iter()).filter(|g| (1.0..7.0).contains(&g.own));
            let middle: Vec<_> = middle.collect();
            assert!(middle.len() > 100);
            for g in &middle {
                let k = g.at / period;
                assert!(
                    (k - k.round()).abs() < 1e-6,
                    "{bpm} BPM: a grain reads from {}",
                    g.at
                );
            }
            for w in middle.windows(2) {
                // How many periods of the tone at its pitch apart they are.
                let apart = (w[1].own - w[0].own) * beat * f64::from(RATE) * pitch / period;
                assert!(
                    (apart - apart.round()).abs() < 1e-6,
                    "{bpm} BPM: {apart} periods"
                );
                // About half a grain: 15 ms, from 13.3 to 20 ms.
                let ms = (w[1].own - w[0].own) * beat * 1000.0;
                assert!(
                    (10.0..=21.0).contains(&ms),
                    "{bpm} BPM, {transpose}: {ms} ms"
                );
            }
        }
    }

    #[test]
    fn transposed_grains_play_higher_or_lower() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.0, 4.0);
        let tempo = song(120.0);
        for follow_pitch in [false, true] {
            for (transpose, ratio) in [(12, 2.0), (-12, 0.5), (7, pitch_ratio(7.0, 0.0))] {
                let mut clip = granular(8.0, 120.0, follow_pitch, 25.0);
                warp_of(&mut clip).transpose = transpose;
                let player = player(&[&clip], &data, Some(&marks), &tempo);
                let out = play(player, &tempo, at(2.5), 512);
                let middle = &out[at(0.5)..at(2.5)];
                let (moved, kept) = (strength(middle, TONE * ratio), strength(middle, TONE));
                assert!(
                    moved > 10.0 * kept,
                    "{transpose}, Follow pitch {follow_pitch}: {moved} moved, {kept} kept"
                );
            }
        }
    }

    #[test]
    fn grains_play_the_file_where_the_warp_puts_it() {
        // The tone from 1 s to 1.5 s of the file, its beats 2 to 3, played
        // at half its speed: from 2 s to 3 s of the song.
        let data = file(3.0, 1.0, 1.5);
        let marks = marks(1.0, 1.5);
        let tempo = song(60.0);
        for (follow_pitch, fluctuation) in [(false, 0.0), (false, 100.0), (true, 0.0)] {
            let clip = granular(6.0, 120.0, follow_pitch, fluctuation);
            let player = player(&[&clip], &data, Some(&marks), &tempo);
            let out = play(player, &tempo, at(4.0), 512);
            let level_of = |from: f64, to: f64| level(&out[at(from)..at(to)]);
            let case = format!("Follow pitch {follow_pitch}, Fluctuation {fluctuation}");
            assert!(level_of(0.0, 1.9) == 0.0, "{case}: before");
            assert!(level_of(2.1, 2.9) > 0.25, "{case}: {}", level_of(2.1, 2.9));
            assert!(level_of(3.1, 4.0) == 0.0, "{case}: after");
        }
    }

    #[test]
    fn grains_meet_without_clicks() {
        // The tone, starting and stopping where it crosses zero, so the
        // file has no clicks of its own.
        let data = file(3.0, 1.0, 2.0);
        let marks = marks(1.0, 2.0);
        let steepest_in_file = steepest(data.channel(0));
        for bpm in [60.0, 96.0, 150.0, 240.0] {
            let tempo = song(bpm);
            for (follow_pitch, fluctuation) in [(false, 100.0), (true, 0.0)] {
                for transpose in [0, 12] {
                    let mut clip = granular(6.0, 120.0, follow_pitch, fluctuation);
                    warp_of(&mut clip).transpose = transpose;
                    let player = player(&[&clip], &data, Some(&marks), &tempo);
                    let out = play(
                        player,
                        &tempo,
                        tempo.frame_at(Beats::from_f64(6.0), RATE) as usize,
                        512,
                    );
                    let step = steepest(&out) / pitch_ratio(f64::from(transpose), 0.0) as f32;
                    assert!(
                        step < 3.0 * steepest_in_file,
                        "{bpm} BPM, Follow pitch {follow_pitch}, {transpose}: a step of {step}"
                    );
                }
            }
        }
    }

    #[test]
    fn grains_play_the_same_however_the_blocks_fall() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.5, 3.0);
        let tempo = song(96.0);
        for follow_pitch in [false, true] {
            let mut clip = granular(8.0, 120.0, follow_pitch, 100.0);
            let audio = audio_of(&mut clip);
            audio.set_lane(ClipParam::Transpose, vec![point(1.0, 0.0), point(3.0, 7.0)]);
            audio.set_lane(
                ClipParam::GrainSize,
                vec![point(0.0, 10.0), point(4.0, 120.0)],
            );
            let play_in = |block: usize| {
                play(
                    player(&[&clip], &data, Some(&marks), &tempo),
                    &tempo,
                    at(2.0),
                    block,
                )
            };
            let expected = play_in(64);
            assert!(level(&expected) > 0.2);
            for block in [1, 7, 1000] {
                assert!(
                    play_in(block) == expected,
                    "Follow pitch {follow_pitch}, blocks of {block}"
                );
            }
        }
    }

    #[test]
    fn fluctuation_spreads_the_grains_the_same_every_time() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.0, 4.0);
        let tempo = song(96.0);
        let out = |follow_pitch: bool, fluctuation: f64, marks: Option<&PitchMarks>| {
            let clip = granular(8.0, 120.0, follow_pitch, fluctuation);
            play(player(&[&clip], &data, marks, &tempo), &tempo, at(2.0), 512)
        };
        let spread = out(false, 100.0, None);
        assert!(spread == out(false, 100.0, None));
        let lattice = out(false, 0.0, None);
        assert!(spread != lattice);
        // Following the pitch, the grains aren't spread, and until analysis
        // has found the marks, they keep to the lattice.
        assert!(out(true, 100.0, None) == lattice);
        let on_marks = out(true, 0.0, Some(&marks));
        assert!(on_marks != lattice);
        assert!(out(true, 100.0, Some(&marks)) == on_marks);
    }

    #[test]
    fn grains_are_half_their_size_apart_where_they_play() {
        let tempo = song(120.0);
        // From 10 ms at the clip's start to 100 ms at its beat 8, evenly in
        // the scale between, as a curve moves: on a log scale.
        let mut clip = granular(8.0, 120.0, false, 0.0);
        audio_of(&mut clip).set_lane(
            ClipParam::GrainSize,
            vec![point(0.0, 10.0), point(8.0, 100.0)],
        );
        let grains = Grains::new(&clip, None, RATE, &tempo).expect("in Granular");
        let size = |own: f64| 10.0 * 10f64.powf(own / 8.0);
        let apart = |w: &[Grain]| (w[1].own - w[0].own) * 0.5 * 1000.0;
        let gaps = grains
            .grains
            .windows(2)
            .filter(|w| (0.0..8.0).contains(&w[0].own));
        let gaps: Vec<_> = gaps.collect();
        assert!(gaps.len() > 100);
        for w in gaps {
            let (gap, half) = (apart(w), size(w[0].own) / 2.0);
            assert!(
                (gap - half).abs() < 1e-6 * half,
                "at {}: {gap} ms",
                w[0].own
            );
        }
        // The lattice runs from three grains before the clip to two after:
        // 10 ms and 100 ms, or 0.02 and 0.2 beats, at 120 BPM.
        let (first, last) = (
            grains.grains[0].own,
            grains.grains[grains.grains.len() - 1].own,
        );
        assert!((first + 0.03).abs() < 1e-9, "{first}");
        assert!((8.2..8.3).contains(&last), "{last}");
    }

    #[test]
    fn a_split_clip_plays_as_the_whole_one_did() {
        let data = file(4.0, 0.0, 4.0);
        let marks = marks(0.5, 3.5);
        let tempo = song(96.0);
        for follow_pitch in [false, true] {
            let mut whole = granular(8.0, 120.0, follow_pitch, 100.0);
            let curve = vec![point(0.0, 20.0), point(8.0, 80.0)];
            audio_of(&mut whole).set_lane(ClipParam::GrainSize, curve);
            // Split at its beat 3.
            let (mut first, mut second) = (whole.clone(), whole.clone());
            first.length = Beats::from_f64(3.0);
            second.id = ClipId(1);
            second.start = Beats::from_f64(3.0);
            second.length = Beats::from_f64(5.0);
            audio_of(&mut second).offset = Offset::Beats(Beats::from_f64(3.0));
            let expected = play(
                player(&[&whole], &data, Some(&marks), &tempo),
                &tempo,
                at(4.5),
                512,
            );
            let split = player(&[&first, &second], &data, Some(&marks), &tempo);
            assert!(
                play(split, &tempo, at(4.5), 512) == expected,
                "Follow pitch {follow_pitch}"
            );
        }
    }

    #[test]
    fn a_new_tempo_moves_the_grains_with_the_beats_until_theyre_placed_again() {
        // The tone at the file's beats 2 to 3, its grains placed at 60 BPM,
        // from 2 s to 3 s of the song, played at 120 BPM before they're
        // placed again: from 1 s to 1.5 s.
        let data = file(3.0, 1.0, 1.5);
        let clip = granular(6.0, 120.0, false, 0.0);
        let placed_at_60 = player(&[&clip], &data, None, &song(60.0));
        let out = play(placed_at_60, &song(120.0), at(3.0), 512);
        assert!(level(&out[..at(0.95)]) == 0.0);
        assert!(level(&out[at(1.05)..at(1.45)]) > 0.3);
        assert!(level(&out[at(1.55)..]) == 0.0);
        // Placed where it plays the file as it is, at another tempo, its
        // grains play.
        let grains = Grains::new(&clip, None, RATE, &song(120.0)).expect("in Granular");
        assert!(grains.plays_as_is(&song(120.0)));
        let placed_at_120 = player(&[&clip], &data, None, &song(120.0));
        let out = play(placed_at_120, &song(60.0), at(4.0), 512);
        assert!(level(&out[..at(1.9)]) == 0.0);
        assert!(level(&out[at(2.1)..at(2.9)]) > 0.25);
    }

    #[test]
    fn granular_clips_stream_what_their_grains_read_next() {
        let data = file(12.0, 0.0, 12.0);
        let tempo = song(96.0);
        let clip = granular(24.0, 120.0, false, 100.0);
        let expected = play(player(&[&clip], &data, None, &tempo), &tempo, at(6.0), 64);
        let streamer = Streamer::spawn(true);
        for block in [65_536, 512] {
            let stream = streamer.stream(Box::new(DataReader(data.clone())));
            let player = player_of(&[&clip], Source::Stream(stream.clone()), None, &tempo);
            assert!(
                play(player, &tempo, at(6.0), block) == expected,
                "blocks of {block}"
            );
            assert_eq!(stream.missed(), 0);
        }
        // Every block's reads fall within what a stream holds from where it
        // was last told the clip reads, or before that, a tenth of a second
        // at most, as the disk catches up: a second of the file, and as much
        // more as the grains read apart. Spread all they go, grains of
        // 100 ms an octave up or down need no more, and grains of half a
        // second four octaves up do.
        let block = 512;
        for (transpose, grain_size) in [(0, 30.0), (12, 100.0), (-12, 100.0), (48, 500.0)] {
            let mut clip = granular(24.0, 120.0, false, 100.0);
            warp_of(&mut clip).transpose = transpose;
            warp_of(&mut clip).granular.grain_size = grain_size;
            let spread = stream_spread(&clip, None, RATE, &tempo);
            assert_eq!(spread > 0.0, transpose == 48, "{transpose}: {spread} s");
            let held = ((1.0 + spread) * f64::from(RATE)) as i64;
            let mut player = player(&[&clip], &data, None, &tempo);
            player.prepare(&EngineConfig {
                sample_rate: RATE,
                max_block: block,
            });
            let placed = &player.clips[0];
            for n in (0..at(14.0) as i64).step_by(block) {
                let (mut first, mut last) = (i64::MAX, i64::MIN);
                placed.runs(&tempo, RATE, n, n + block as i64, |_, run| {
                    let (a, b) = match placed.reading(&run) {
                        Reading::Copy { by, .. } => (run.from + by, run.to - 1 + by),
                        Reading::Resample(r) => {
                            let reach = r.reach() as i64;
                            let a = run.position(run.from).floor() as i64 - reach + 1;
                            (a, run.position(run.to - 1).floor() as i64 + reach)
                        }
                    };
                    (first, last) = (first.min(a), last.max(b));
                });
                for m in [n, n - 2048, n - 4096].into_iter().filter(|&m| m >= 0) {
                    let hint = placed.read_start(&tempo, RATE, m);
                    assert!(
                        hint <= first,
                        "{transpose}, {grain_size} ms, frame {n}, hinted at {m}"
                    );
                    assert!(
                        last < hint + held,
                        "{transpose}, {grain_size} ms, frame {n}, hinted at {m}: {}",
                        last - hint
                    );
                }
            }
        }
    }

    #[test]
    fn a_stream_holds_as_much_more_of_the_file_as_the_grains_read_apart() {
        let tempo = song(120.0);
        let spread = |transpose: i8, grain_size: f64, fluctuation: f64| {
            let mut clip = granular(48.0, 120.0, false, fluctuation);
            warp_of(&mut clip).transpose = transpose;
            warp_of(&mut clip).granular.grain_size = grain_size;
            stream_spread(&clip, None, RATE, &tempo)
        };
        // As a clip starts, and an octave up, they read close together.
        assert_eq!(spread(0, 30.0, 25.0), 0.0);
        assert_eq!(spread(12, 100.0, 100.0), 0.0);
        // Longer and higher, further apart, in steps of twice as far.
        assert_eq!(spread(24, 250.0, 100.0), 2.0);
        assert_eq!(spread(48, 500.0, 100.0), 16.0);
        // Following a voice's pitch, without the spread, nearer.
        let mut clip = granular(48.0, 120.0, true, 0.0);
        warp_of(&mut clip).transpose = 24;
        warp_of(&mut clip).granular.grain_size = 250.0;
        let marks = marks(0.0, 24.0);
        assert_eq!(stream_spread(&clip, Some(&marks), RATE, &tempo), 1.0);
        // A clip in another mode holds no more.
        warp_of(&mut clip).mode = WarpMode::Spectral;
        assert_eq!(stream_spread(&clip, Some(&marks), RATE, &tempo), 0.0);
    }
}
