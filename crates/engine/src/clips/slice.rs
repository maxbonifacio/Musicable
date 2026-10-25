//! Slice: a warped clip's audio cut into slices, each played at the file's
//! own speed from where it lands in the song, as Live's Beats plays it.
//!
//! A clip's slices start at its transients, or on a grid of notes in its own
//! time (see [`Preserve`]), and each starts where its warp puts its first
//! frame. From there it plays its stretch of the file at the file's own
//! speed, resampled faster or slower by its transpose and detune, until the
//! next slice starts. A slice the song plays faster than the file is cut
//! short by the next, and one it plays slower runs out of audio first, and
//! its [`TransientLoop`] fills the gap: silence, the second half of the
//! slice over and over, or that half backwards and forwards. Its Transient
//! Envelope fades it as it plays.
//!
//! Like the rest of the player, a slice's every frame comes from where the
//! frame is in the song, so slices render the same however the blocks fall.
//! Each slice fades in over a couple of milliseconds just before it starts,
//! as the one before fades out, so its transient lands at full level, with
//! no click at the cut. Where two slices meet seamlessly, as when the song
//! plays the file at its own speed, the cut has no crossfade, so the clip
//! plays the file sample for sample, as Re-Pitch would.
//!
//! The settings each slice plays with are those its clip's curves have
//! where it starts, or else its warp's, so automation moves them a slice at
//! a time. Preserve's curve picks where the slices are: a place on the grid
//! of a Preserve, or a transient, starts a slice where the curve holds that
//! Preserve.

use std::f64::consts::PI;

use musicable_model::clip::{AudioClip, Clip};
use musicable_model::param::ClipParam;
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::{Preserve, SliceSettings, TransientLoop, Warp, pitch_ratio};

use super::{Run, SAME_SPEED, nearest};

/// How long the crossfade into each slice is, and the seam of a loop: two
/// milliseconds.
const CROSSFADE_SECONDS: f64 = 0.002;

/// The longest stretch of the file a slice loops, from the middle of its
/// audio to its end: half a second, which a stream keeps in memory.
const LOOP_SECONDS: f64 = 0.5;

/// The quickest a slice's Transient Envelope fades: by e in a millisecond.
const QUICKEST_DECAY_SECONDS: f64 = 0.001;

/// How far past its audio's end a slice reads on the last frame before the
/// next one starts, in the file's frames, once it has run out: half a frame,
/// so a slice the song plays at the file's own speed never has, however its
/// positions round.
const RUN_OUT: f64 = 0.5;

/// A warped clip's slices, as Slice plays them.
#[derive(Debug)]
pub(super) struct Slices {
    /// In order of where they start, then one more where the last slice's
    /// audio ends, which doesn't play.
    slices: Vec<Slice>,
    /// The file's sample rate.
    file_rate: u32,
}

/// Where one slice starts, and what it plays with.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Slice {
    /// Where it starts in the clip's own time, in beats, and the frame of
    /// the file it starts on, fractional on a grid.
    own: f64,
    frame: f64,
    /// How much faster than the file's own speed it plays, for its transpose
    /// and detune.
    pitch: f64,
    /// Its Transient Envelope, from 0 to 100, and what fills the gap where
    /// its audio runs out.
    envelope: f64,
    transient_loop: TransientLoop,
}

/// Where a clip's slices play: the song's tempo at the device's rate, and
/// how far the clip's own time is ahead of the song's beats.
#[derive(Debug, Clone, Copy)]
pub(super) struct Song<'a> {
    pub tempo: &'a TempoMap,
    pub rate: u32,
    pub shift: f64,
}

impl Song<'_> {
    /// The song frame, fractional, where `own` in the clip's own time plays.
    pub(super) fn frame_of(&self, own: f64) -> f64 {
        let at = own - self.shift;
        let tick = Beats::from_f64(at);
        let rest = (at - tick.to_f64()) * 60.0 * f64::from(self.rate) / self.tempo.bpm_at(tick);
        self.tempo.sample_at(tick, self.rate) + rest
    }

    /// The clip's own time on song frame `frame`, fractional.
    pub(super) fn own_at(&self, frame: f64) -> f64 {
        self.tempo.beats_at(frame, self.rate) + self.shift
    }

    /// How long the crossfades are, in song frames.
    fn crossfade(&self) -> i64 {
        (CROSSFADE_SECONDS * f64::from(self.rate)).round() as i64
    }
}

/// One slice as it plays in the song, at its tempo.
#[derive(Debug, Clone, Copy)]
struct Timing {
    /// Where it starts, in song frames, fractional, and the first song
    /// frame it plays on, and the next slice's.
    start: f64,
    first: i64,
    next: i64,
    /// How many frames it fades in over before `first`, and out over before
    /// `next`.
    fade_in: i64,
    fade_out: i64,
    /// The file's frames it moves on each song frame.
    step: f64,
    /// Where its audio starts and ends in the file.
    from: f64,
    end: f64,
    /// Whether its audio runs out before the next slice starts, and what
    /// fills the gap.
    runs_out: bool,
    transient_loop: TransientLoop,
    /// How many song frames its envelope takes to fall by e, if it fades.
    decay: Option<f64>,
    /// How long its crossfades and its loop's seams are, in song frames.
    crossfade: f64,
}

impl Timing {
    /// Where in the file it plays on song frame `frame`, fractional, if it
    /// didn't run out.
    fn position(&self, frame: f64) -> f64 {
        self.from + (frame - self.start) * self.step
    }

    /// The song frame, fractional, on which it plays the file's `position`.
    fn frame_of(&self, position: f64) -> f64 {
        self.start + (position - self.from) / self.step
    }
}

/// The first song frame on or after `frame`, which a slice starting there
/// starts on, however the blocks fall.
fn first_on(frame: f64) -> i64 {
    frame.ceil() as i64
}

impl Slices {
    /// The slices of `clip`, which plays `audio` warped by `warp`, of a file
    /// `frames` long at `file_rate`, in a song whose tempo is `tempo`. The
    /// grid's bars are as long as the song's at the clip's start. `None` if
    /// there are none, as for an empty file.
    pub(super) fn new(
        clip: &Clip,
        audio: &AudioClip,
        warp: &Warp,
        file_rate: u32,
        frames: u64,
        tempo: &TempoMap,
    ) -> Option<Self> {
        let offset = audio.offset.beats()?.to_f64();
        let bar = tempo.signature_at(clip.start).bar_length();
        let starts = Starts {
            warp,
            rate: file_rate,
            frames,
            bar,
            own: (offset, offset + clip.length.to_f64()),
        };
        let mut found = match audio.lane(ClipParam::Preserve) {
            // Each Preserve on the curve, where the curve holds it.
            Some(lane) => {
                let mut preserves: Vec<Preserve> = (lane.points.iter())
                    .map(|p| Preserve::at_step(p.value))
                    .collect();
                preserves.sort_by_key(|p| p.step());
                preserves.dedup();
                let held = |own: f64| Preserve::at_step(lane.value_at(Beats::from_f64(own)));
                let mut found: Vec<(f64, f64)> = preserves
                    .into_iter()
                    .flat_map(|preserve| {
                        let starts = starts.of(preserve);
                        starts
                            .into_iter()
                            .filter(move |&(own, _)| held(own) == preserve)
                    })
                    .collect();
                found.sort_by(|a, b| a.0.total_cmp(&b.0));
                found
            }
            None => starts.of(warp.slice.preserve),
        };
        // Less than a frame of the file apart, two are one.
        found.dedup_by(|later, earlier| later.1 - earlier.1 < 1.0);
        // And where the last one's audio ends: the file's end, if it's past
        // the last start.
        let end = frames as f64;
        if found.last().is_none_or(|&(_, frame)| frame < end) {
            found.push((warp.beat_at(end, file_rate), end));
        }
        if found.len() < 2 {
            return None;
        }
        let value = |param: ClipParam, own: f64, otherwise: f64| {
            audio
                .lane(param)
                .map_or(otherwise, |lane| lane.value_at(Beats::from_f64(own)))
        };
        let settings = &warp.slice;
        let slices = found
            .into_iter()
            .map(|(own, frame)| {
                let transpose = value(ClipParam::Transpose, own, f64::from(warp.transpose));
                let detune = value(ClipParam::Detune, own, f64::from(warp.detune));
                let loop_step = settings.transient_loop.step() as f64;
                Slice {
                    own,
                    frame,
                    pitch: pitch_ratio(transpose, detune),
                    envelope: value(ClipParam::TransientEnvelope, own, settings.envelope),
                    transient_loop: TransientLoop::at_step(value(
                        ClipParam::TransientLoop,
                        own,
                        loop_step,
                    )),
                }
            })
            .collect();
        Some(Self { slices, file_rate })
    }

    /// How many slices play: all but the last entry, where the last one's
    /// audio ends.
    fn count(&self) -> usize {
        self.slices.len() - 1
    }

    /// Where slice `k` starts in song frames, fractional: `k` can be the
    /// last entry, where the last slice's audio ends.
    fn start(&self, song: &Song<'_>, k: usize) -> f64 {
        song.frame_of(self.slices[k].own)
    }

    /// The file's frames slice `k` moves on each song frame.
    fn step(&self, song: &Song<'_>, k: usize) -> f64 {
        self.slices[k].pitch * f64::from(self.file_rate) / f64::from(song.rate)
    }

    /// Calls `each` with the runs that play on the song's frames from `from`
    /// up to `to`, in order of the slices they're of. A slice's runs can
    /// overlap: as it fades out into the next, and at its loop's seams.
    pub(super) fn runs(&self, song: &Song<'_>, from: i64, to: i64, mut each: impl FnMut(Run)) {
        let mut k = self.slice_on(song, from);
        while k < self.count() {
            let timing = self.timing(song, k);
            if timing.first - timing.fade_in >= to {
                break;
            }
            self.slice_runs(&timing, from, to, &mut each);
            k += 1;
        }
    }

    /// The slice that plays on song frame `frame`, from its first frame
    /// until the next slice's, or the first slice, before them all.
    fn slice_on(&self, song: &Song<'_>, frame: i64) -> usize {
        let own = song.own_at(frame as f64);
        let starts = &self.slices[..self.count()];
        let mut k = starts.partition_point(|s| s.own <= own).saturating_sub(1);
        // As the frames fall, which can differ from the beats by a hair.
        let first = |k: usize| first_on(self.start(song, k));
        while k > 0 && first(k) > frame {
            k -= 1;
        }
        while k + 1 < self.count() && first(k + 1) <= frame {
            k += 1;
        }
        k
    }

    /// How slice `k` plays at the song's tempo.
    fn timing(&self, song: &Song<'_>, k: usize) -> Timing {
        let slice = &self.slices[k];
        let start = self.start(song, k);
        let next_start = self.start(song, k + 1);
        let (first, next) = (first_on(start), first_on(next_start));
        let step = self.step(song, k);
        let (from, end) = (slice.frame, self.slices[k + 1].frame);
        let crossfade = song.crossfade();
        let runs_out = from + ((next - 1) as f64 - start) * step >= end + RUN_OUT;
        let fade_in = match k {
            0 => 0,
            _ => self.crossfade_into(song, k, self.start(song, k - 1), start),
        };
        let fade_out = if k + 1 < self.count() {
            self.crossfade_into(song, k + 1, start, next_start)
        } else {
            // The last fades out as it stops, unless it plays its audio
            // through to the end.
            let through = !runs_out && from + (next as f64 - start) * step >= end - 1.0;
            match through {
                true => 0,
                false => crossfade.min((next - first) / 2),
            }
        };
        let decay = (slice.envelope < SliceSettings::ENVELOPE_MAX).then(|| {
            let quickest = QUICKEST_DECAY_SECONDS * f64::from(song.rate);
            let length = next_start - start;
            quickest.max(length * 10f64.powf((slice.envelope - 60.0) / 20.0))
        });
        Timing {
            start,
            first,
            next,
            fade_in,
            fade_out,
            step,
            from,
            end,
            runs_out,
            transient_loop: slice.transient_loop,
            decay,
            crossfade: crossfade as f64,
        }
    }

    /// How many song frames slice `k` fades in over before it starts, as
    /// the one before fades out, with the slice before starting at `before`
    /// and `k` at `start`, fractional: none where the two meet seamlessly,
    /// and else the crossfade, but no more than half the slice before.
    fn crossfade_into(&self, song: &Song<'_>, k: usize, before: f64, start: f64) -> i64 {
        let (last, this) = (&self.slices[k - 1], &self.slices[k]);
        let (last_first, first) = (first_on(before), first_on(start));
        let (last_step, step) = (self.step(song, k - 1), self.step(song, k));
        // Where each plays the file on a frame, the one before running out
        // of audio where this one's starts.
        let last_at = |frame: i64| last.frame + (frame as f64 - before) * last_step;
        let at = |frame: i64| this.frame + (frame as f64 - start) * step;
        let ran_out = last_at(first - 1) >= this.frame + RUN_OUT;
        let reads_the_same = match (step - 1.0).abs() < SAME_SPEED {
            // Sample for sample, the same frames of the file, as each
            // reads them from its first frame.
            true => nearest(last_at(last_first)) - last_first == nearest(at(first)) - first,
            false => (last_at(first) - at(first)).abs() < 1e-6,
        };
        let full = SliceSettings::ENVELOPE_MAX;
        let seamless = last.envelope >= full
            && this.envelope >= full
            && last.pitch == this.pitch
            && !ran_out
            && reads_the_same;
        match seamless {
            true => 0,
            false => song.crossfade().min((first - last_first) / 2),
        }
    }

    /// Calls `each` with the runs of the slice `timing` places that play on
    /// the song's frames from `from` up to `to`.
    fn slice_runs(&self, timing: &Timing, from: i64, to: i64, each: &mut impl FnMut(Run)) {
        let t = timing;
        // Its fade in, before it starts, from the audio before its start.
        if t.fade_in > 0 {
            let (a, b) = (from.max(t.first - t.fade_in), to.min(t.first));
            if a < b {
                let fade_in = Ramp::rising((t.first - t.fade_in) as f64, t.fade_in as f64);
                each(Run {
                    from: a,
                    to: b,
                    anchor: t.first,
                    at: t.position(t.first as f64),
                    step: t.step,
                    shape: Shape::default().with(fade_in),
                    back_to: None,
                });
            }
        }
        let (a, b) = (from.max(t.first), to.min(t.next));
        if a >= b {
            return;
        }
        let fade_out = (t.fade_out > 0)
            .then(|| Ramp::falling((t.next - t.fade_out) as f64, t.fade_out as f64));
        let decay = t.decay.map(|frames| Decay {
            from: t.start,
            frames,
        });
        // A run of the slice from frame `start` up to `end`, reading the
        // file from `at` on frame `anchor` at `step`, through `ramps`, of
        // those frames it plays.
        let mut run = |start: i64,
                       end: i64,
                       anchor: i64,
                       (at, step): (f64, f64),
                       ramps: &[Ramp],
                       back_to: Option<f64>| {
            let (start, end) = (start.max(a), end.min(b));
            if start >= end {
                return;
            }
            let mut shape = Shape {
                decay,
                ..Shape::default()
            };
            for ramp in ramps.iter().chain(fade_out.as_ref()) {
                if ramp.moves_over(start, end) {
                    shape = shape.with(*ramp);
                }
            }
            each(Run {
                from: start,
                to: end,
                anchor,
                at,
                step,
                shape,
                back_to,
            });
        };
        let (first, step) = (t.first, t.step);
        let forward = (t.position(first as f64), step);
        let gap = t.runs_out.then(|| self.gap(t)).flatten();
        let Some(gap) = gap else {
            // It plays until the next starts, or stops where its audio
            // runs out, fading out just before.
            if !t.runs_out {
                run(first, t.next, first, forward, &[], None);
                return;
            }
            let out = t.frame_of(t.end);
            let length = t.crossfade.min((out - t.start) / 2.0);
            let fade = Ramp::falling(out - length, length);
            run(first, first_on(out), first, forward, &[fade], None);
            return;
        };
        let (end, length) = (t.end, gap.length);
        let seam = gap.seam;
        match t.transient_loop {
            TransientLoop::Forward => {
                // Once through, then from the loop's start over and over,
                // each time round fading in over a seam just before it, as
                // the time before fades out.
                let back_to = Some(gap.start - seam);
                let fade = Ramp::falling(t.frame_of(end - seam), t.crossfade);
                run(
                    first,
                    first_on(t.frame_of(end)),
                    first,
                    forward,
                    &[fade],
                    back_to,
                );
                let lowest = t.position(a as f64);
                let highest = t.position((b - 1) as f64);
                let first_pass = (((lowest - end) / length).floor() as i64 + 1).max(1);
                let last_pass = ((highest - end + seam) / length).floor() as i64 + 1;
                for pass in first_pass..=last_pass {
                    let into = end + (pass - 1) as f64 * length - seam;
                    let out = end + pass as f64 * length;
                    let anchor = first_on(t.frame_of(into));
                    let at = t.position(anchor as f64) - pass as f64 * length;
                    let ramps = [
                        Ramp::rising(t.frame_of(into), t.crossfade),
                        Ramp::falling(t.frame_of(out - seam), t.crossfade),
                    ];
                    let until = first_on(t.frame_of(out));
                    run(anchor, until, anchor, (at, step), &ramps, back_to);
                }
            }
            TransientLoop::BackAndForth => {
                // Once through, then back to the loop's start and forward
                // again, over and over, turning on the last frame of its
                // audio, so it never reads the next slice's, or past the
                // file's end.
                let turn = end - 1.0;
                let back_to = Some(gap.start - 1.0);
                run(
                    first,
                    first_on(t.frame_of(turn)),
                    first,
                    forward,
                    &[],
                    back_to,
                );
                let lowest = t.position(a as f64);
                let highest = t.position((b - 1) as f64);
                let first_half = ((lowest - turn) / length).floor().max(0.0) as i64;
                let last_half = ((highest - turn) / length).floor() as i64;
                for half in first_half..=last_half {
                    let into = turn + half as f64 * length;
                    let anchor = first_on(t.frame_of(into));
                    let unlooped = t.position(anchor as f64);
                    let reading = match half % 2 {
                        0 => (2.0 * turn + half as f64 * length - unlooped, -step),
                        _ => (unlooped - (half + 1) as f64 * length, step),
                    };
                    let until = first_on(t.frame_of(into + length));
                    run(anchor, until, anchor, reading, &[], back_to);
                }
            }
            TransientLoop::Off => unreachable!("a gap to loop"),
        }
    }

    /// The stretch of its audio the slice `timing` places loops where its
    /// audio runs out, if its Transient Loop Mode loops and the stretch is
    /// long enough to: four seams at least.
    fn gap(&self, timing: &Timing) -> Option<Gap> {
        let t = timing;
        if t.transient_loop == TransientLoop::Off {
            return None;
        }
        let middle = (t.from + t.end) / 2.0;
        let start = middle.max(t.end - LOOP_SECONDS * f64::from(self.file_rate));
        let seam = t.crossfade * t.step;
        let length = t.end - start;
        (length >= 4.0 * seam).then_some(Gap {
            start,
            length,
            seam,
        })
    }

    /// Where in the file the first slice to start after song frame `frame`
    /// starts reading, fractional, as it fades in: `None` after the last.
    pub(super) fn next_read(&self, song: &Song<'_>, frame: i64) -> Option<f64> {
        let k = self.slice_on(song, frame);
        let timing = |k: usize| (k < self.count()).then(|| self.timing(song, k));
        let next = timing(k).filter(|t| t.first - t.fade_in > frame);
        let t = next.or_else(|| timing(k + 1))?;
        Some(t.position((t.first - t.fade_in) as f64))
    }
}

/// The stretch of a slice's audio that loops where its audio runs out.
#[derive(Debug, Clone, Copy)]
struct Gap {
    /// Where it starts in the file, and how long it is, in the file's frames.
    start: f64,
    length: f64,
    /// How many of the file's frames each seam crossfades over.
    seam: f64,
}

/// Where the slices of a clip in Slice mode start, in the clip's own time
/// and the file's frames, for each Preserve.
struct Starts<'a> {
    warp: &'a Warp,
    rate: u32,
    /// How long the file is.
    frames: u64,
    /// How long a bar of the grid is.
    bar: Beats,
    /// The stretch of the clip's own time it plays.
    own: (f64, f64),
}

impl Starts<'_> {
    /// Where slices start with `preserve`, in order, as the clip's own time
    /// and the file's frame: each transient in sight and the file's start
    /// before them, or each step of a grid around the stretch the clip
    /// plays. A clip whose audio isn't analysed yet is cut on sixteenths.
    fn of(&self, preserve: Preserve) -> Vec<(f64, f64)> {
        let length = match preserve.length(self.bar) {
            Some(length) => length,
            None if self.warp.onsets.is_none() => {
                Preserve::Sixteenth.length(self.bar).expect("a grid")
            }
            None => {
                let frames = self
                    .warp
                    .transients()
                    .map(|o| o.frame)
                    .filter(|&f| f < self.frames);
                let mut starts: Vec<u64> = frames.collect();
                if starts.first() != Some(&0) {
                    starts.insert(0, 0);
                }
                return (starts.into_iter())
                    .map(|frame| (self.warp.beat_at(frame as f64, self.rate), frame as f64))
                    .collect();
            }
        };
        let step = length.to_f64();
        let (start, end) = self.own;
        // From the step before the clip's start to the one after its end.
        let first = (start / step).floor() as i64 - 1;
        let last = (end / step).ceil() as i64 + 1;
        (first..=last)
            .map(|i| {
                let own = i as f64 * step;
                (own, self.warp.frame_at(own, self.rate))
            })
            .collect()
    }
}

/// How a run of a slice's frames fades, on top of the clip's own gain.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Shape {
    decay: Option<Decay>,
    ramps: [Option<Ramp>; 3],
}

impl Shape {
    /// The same, faded by `ramp` too.
    pub(super) fn with(mut self, ramp: Ramp) -> Self {
        if let Some(slot) = self.ramps.iter_mut().find(|r| r.is_none()) {
            *slot = Some(ramp);
        }
        self
    }

    /// Whether it lets every frame through as it is.
    pub(super) fn is_flat(&self) -> bool {
        self.decay.is_none() && self.ramps.iter().all(Option::is_none)
    }

    /// How much of song frame `frame` it lets through.
    pub(super) fn at(&self, frame: i64) -> f32 {
        let mut level = self.decay.map_or(1.0, |decay| decay.at(frame));
        for ramp in self.ramps.iter().flatten() {
            level *= ramp.at(frame);
        }
        level as f32
    }
}

/// A slice's Transient Envelope: from song frame `from`, falling by e every
/// `frames` frames.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Decay {
    from: f64,
    frames: f64,
}

impl Decay {
    fn at(&self, frame: i64) -> f64 {
        (-(frame as f64 - self.from).max(0.0) / self.frames).exp()
    }
}

/// A raised cosine over song frames, from `start` for `length`, rising from
/// silence to full or falling from full to silence: full before a rise and
/// after a fall, and silence after a fall. Rising and falling over the same
/// frames, two add up to one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Ramp {
    start: f64,
    length: f64,
    rising: bool,
}

impl Ramp {
    pub(super) fn rising(start: f64, length: f64) -> Self {
        Self {
            start,
            length,
            rising: true,
        }
    }

    pub(super) fn falling(start: f64, length: f64) -> Self {
        Self {
            start,
            length,
            rising: false,
        }
    }

    /// Whether it changes anything on the frames from `from` up to `to`.
    fn moves_over(&self, from: i64, to: i64) -> bool {
        let (from, to) = (from as f64, to as f64);
        match self.rising {
            // Silence before it, as well as the rise.
            true => from < self.start + self.length,
            false => to > self.start,
        }
    }

    fn at(&self, frame: i64) -> f64 {
        let x = (frame as f64 - self.start) / self.length;
        let up = match x {
            _ if x.is_nan() || x <= 0.0 => 0.0,
            _ if x >= 1.0 => 1.0,
            _ => 0.5 - 0.5 * (PI * x).cos(),
        };
        match self.rising {
            true => up,
            false => 1.0 - up,
        }
    }
}
