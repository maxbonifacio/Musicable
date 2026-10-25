//! Spectral: a warped clip's audio stretched in the frequency domain, so it
//! keeps its pitch at any speed, by Signalsmith Stretch
//! (`musicable-stretch`), as Live's Complex stretches it.
//!
//! A stretcher remembers what it played, so what it renders at a song
//! position depends on where it started. To keep the engine's rule that a
//! position always renders the same, a clip in Spectral plays audio
//! rendered ahead, sample for sample. Its [`Plan`] cuts its file into
//! segments of 4 to 8 seconds, at the strongest transient where there is
//! one, and each segment renders from a fresh stretcher ([`render`]) into
//! the song's time at the file's sample rate, the same every time. The app
//! renders them on the background queue and keeps them in its render
//! cache, by a key of what the render depends on ([`SegmentPlan::key`]),
//! relative to the segment's own start, so a copy of the clip at the same
//! tempo plays the same audio. Each segment starts with preroll, the
//! audio before it, so its stretcher starts settled, and crossfades from
//! the one before over the 5 ms before the cut, so the attack after it
//! lands at full level.
//!
//! Until a segment is rendered, the clip plays a live stretch of its file,
//! so an edit is heard at once, and hands over to the rendered audio where
//! the next rendered segment starts, through the same crossfade. A segment
//! plays rendered or live as playback enters it. The live stretch follows
//! the song's tempo as it plays, so when the tempo changes, which changes
//! every segment, the clip plays live, without a dropout, until the new
//! segments land.
//!
//! Where the song plays the file at its own speed wherever the clip plays,
//! and nothing transposes, the clip plays the file as Re-Pitch does, sample
//! for sample at the file's rate, with no stretcher and nothing to render.

use std::hash::Hasher;
use std::io;
use std::sync::{Arc, OnceLock};

use musicable_model::clip::{AudioClip, Clip, ClipContent};
use musicable_model::param::ClipParam;
use musicable_model::time::{Beats, TempoChange, TempoMap};
use musicable_model::warp::{Segment, SpectralSettings, Warp, WarpMode, pitch_ratio};
use musicable_stretch::{Sizes, Stretch};
use siphasher::sip::SipHasher13;

use super::{Curve, SAME_SPEED, WIDEST};
use crate::media::{AudioData, AudioReader, Source};
use crate::resample::Resampler;
use crate::{EngineConfig, ProcessContext};

/// How far apart a clip's segments are cut, in seconds of its file, and how
/// far either way a cut moves to fall on the strongest transient: so a
/// segment is 4 to 8 seconds of the file.
const SPACING_SECONDS: f64 = 6.0;
const SNAP_SECONDS: f64 = 2.0;

/// Where no transient is in reach, cuts fall on a lattice this many
/// seconds of the file apart, so that after an edit the cuts fall where
/// they fell before.
const LATTICE_SECONDS: f64 = 2.0;

/// How long two segments crossfade, just before the cut between them.
const CROSSFADE_SECONDS: f64 = 0.005;

/// What every segment's stretcher draws its random phases from.
const SEED: u32 = 1;

/// How far apart a frame position worked out at two places in the song may
/// come out where it's whole: rounded with this much slack, a copy of a
/// clip a whole number of frames on cuts and reads its file the same, and
/// renders the same segments.
const SLACK: f64 = 1e-4;

/// Changes when what a segment plan renders may change, as
/// [`musicable_stretch::VERSION`] does for the stretcher: the render cache
/// keys segments with both, so they render again.
const VERSION: u32 = 1;

/// How far beyond the clip, in grid frames, segments still render: as far
/// as the device's resampler reaches.
fn margin() -> i64 {
    Resampler::most_reach(WIDEST) as i64 + 1
}

/// Where a clip's file plays in the song: the clip's own time is the song's
/// beats plus `shift`, and its warp's segments place the file's frames in
/// that. Grid frames are the song's frames at the file's sample rate,
/// which segments render at.
#[derive(Debug, Clone)]
struct Map {
    shift: f64,
    segments: Vec<Segment>,
    rate: u32,
}

impl Map {
    /// The clip's own time on grid frame `grid`, at `tempo`.
    fn own_at(&self, tempo: &TempoMap, grid: f64) -> f64 {
        tempo.beats_at(grid, self.rate) + self.shift
    }

    /// The warp segment that plays `own` of the clip's own time.
    fn segment(&self, own: f64) -> &Segment {
        let after = self.segments[1..].partition_point(|s| s.beat.to_f64() <= own);
        &self.segments[after]
    }

    /// The file's position, fractional, on grid frame `grid`.
    fn position(&self, tempo: &TempoMap, grid: f64) -> f64 {
        let own = self.own_at(tempo, grid);
        self.segment(own).frame_at(own)
    }

    /// How many of the file's frames a grid frame plays, on grid frame
    /// `grid`.
    fn speed(&self, tempo: &TempoMap, grid: f64) -> f64 {
        let own = self.own_at(tempo, grid);
        let bpm = tempo.bpm_at(Beats::from_f64(own - self.shift));
        self.segment(own).frames_per_beat * bpm / (60.0 * f64::from(self.rate))
    }

    /// The moment of the clip's own time that the file's `frame` plays at.
    fn own_of(&self, frame: f64) -> f64 {
        let after = self.segments[1..].partition_point(|s| s.frame as f64 <= frame);
        self.segments[after].beat_at(frame)
    }

    /// The grid frame, fractional, that `own` of the clip's own time plays
    /// on, at `tempo`.
    fn grid_of(&self, tempo: &TempoMap, own: f64) -> f64 {
        let at = own - self.shift;
        let tick = Beats::from_f64(at);
        let rest = (at - tick.to_f64()) * 60.0 * f64::from(self.rate) / tempo.bpm_at(tick);
        tempo.sample_at(tick, self.rate) + rest
    }

    /// The file frame a stretcher of `sizes` must have read up to before
    /// its spectrum on grid frame `clock`, for that spectrum to be heard
    /// where its audio plays: a spectrum is of the input centred
    /// [`input_latency`](Sizes::input_latency) before the last frame read,
    /// and heard [`output_latency`](Sizes::output_latency) after it's made.
    fn frontier(&self, tempo: &TempoMap, sizes: &Sizes, clock: i64) -> i64 {
        let heard = (clock + sizes.output_latency as i64) as f64;
        (self.position(tempo, heard) + sizes.input_latency as f64 + SLACK).floor() as i64
    }

    /// Whether the file plays at its own speed from `from` to `to` in the
    /// song, at `tempo`: every warp segment and tempo over them plays a
    /// frame of the file on each grid frame.
    fn plays_as_is(&self, tempo: &TempoMap, from: Beats, to: Beats) -> bool {
        let (mut own, until) = (from.to_f64() + self.shift, to.to_f64() + self.shift);
        let rate = 60.0 * f64::from(self.rate);
        while own < until {
            let segment = self.segments[1..].partition_point(|s| s.beat.to_f64() <= own);
            let bpm = tempo.bpm_at(Beats::from_f64(own - self.shift));
            if (self.segments[segment].frames_per_beat * bpm / rate - 1.0).abs() >= SAME_SPEED {
                return false;
            }
            let next_segment = self.segments.get(segment + 1).map(|s| s.beat.to_f64());
            let next_tempo = (tempo.tempos().iter())
                .map(|c| c.at.to_f64() + self.shift)
                .find(|&at| at > own);
            own = [next_segment, next_tempo]
                .into_iter()
                .flatten()
                .fold(until, f64::min);
        }
        true
    }
}

/// One of the settings a clip's pitch is rendered with, over the clip's
/// own time: its curve, where it has one, or else its warp's value.
#[derive(Debug)]
enum Value {
    Fixed(f64),
    Curve(Curve),
}

impl Value {
    fn new(audio: &AudioClip, param: ClipParam, otherwise: f64) -> Self {
        let curve = audio
            .lane(param)
            .and_then(|lane| Curve::new(&lane.points, param.spec().scale));
        curve.map_or(Self::Fixed(otherwise), Self::Curve)
    }

    fn at(&self, own: f64) -> f64 {
        match self {
            Self::Fixed(value) => *value,
            Self::Curve(curve) => curve.value_at(own),
        }
    }

    /// Whether it's 0 all through.
    fn is_zero(&self) -> bool {
        match self {
            Self::Fixed(value) => *value == 0.0,
            Self::Curve(curve) => curve.points.iter().all(|p| p.1 == 0.0),
        }
    }
}

/// A clip's Transpose, Detune and Formants, as its curves and its warp have
/// them.
#[derive(Debug)]
struct Pitches {
    transpose: Value,
    detune: Value,
    formants: Value,
}

impl Pitches {
    fn new(audio: &AudioClip, warp: &Warp) -> Self {
        Self {
            transpose: Value::new(audio, ClipParam::Transpose, f64::from(warp.transpose)),
            detune: Value::new(audio, ClipParam::Detune, f64::from(warp.detune)),
            formants: Value::new(audio, ClipParam::Formants, warp.spectral.formants),
        }
    }

    /// What the stretcher's pitch is set to at `own` of the clip's own
    /// time.
    fn at(&self, own: f64) -> Pitch {
        let ratio = pitch_ratio(self.transpose.at(own), self.detune.at(own));
        Pitch::new(ratio, self.formants.at(own))
    }

    /// Whether nothing transposes, anywhere in the clip.
    fn none(&self) -> bool {
        self.transpose.is_zero() && self.detune.is_zero()
    }
}

/// What a stretcher's pitch is set to: how much it moves each frequency,
/// and the formants with it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pitch {
    transpose: f32,
    formants: f32,
    /// Whether the formants move from where they were before the
    /// transpose, rather than from where it puts them.
    compensate: bool,
}

impl Pitch {
    /// A transpose of `ratio` with Formants at `formants`: with no
    /// transpose, or with the formants moving all the way with the pitch,
    /// the stretcher leaves the formants to the transpose, which costs
    /// least, and otherwise it moves them `formant_ratio` from where they
    /// were.
    fn new(ratio: f64, formants: f64) -> Self {
        if ratio == 1.0 || formants == 0.0 {
            return Self {
                transpose: ratio as f32,
                formants: 1.0,
                compensate: false,
            };
        }
        Self {
            transpose: ratio as f32,
            formants: SpectralSettings::formant_ratio(formants, ratio) as f32,
            compensate: true,
        }
    }

    fn apply(&self, stretch: &mut Stretch) {
        stretch.set_transpose(f64::from(self.transpose));
        stretch.set_formants(f64::from(self.formants), self.compensate);
    }
}

/// How a clip in Spectral plays, in a song at one tempo map: the segments of
/// its file it renders, in order, and what they render from.
#[derive(Debug)]
pub struct Plan {
    /// The song's tempo as the segments were planned at: they play while
    /// the song's tempo up to the clip's end is still this.
    tempo: TempoMap,
    until: Beats,
    map: Map,
    pitches: Pitches,
    /// The file's channels the clip plays: its first two.
    channels: usize,
    sizes: Sizes,
    /// How many grid frames two segments crossfade over.
    crossfade: usize,
    /// Whether the clip plays the file as it is: then it has no segments.
    passthrough: bool,
    segments: Vec<SegmentPlan>,
}

impl Plan {
    /// How `clip` plays in Spectral in a song whose tempo map is `tempo`,
    /// from a file of `channels` channels at `rate`, `frames` long: `None`
    /// for a clip that isn't warped in Spectral, and for a file without
    /// audio. Only the segments the clip plays are planned.
    pub fn new(
        clip: &Clip,
        tempo: &TempoMap,
        channels: usize,
        rate: u32,
        frames: u64,
    ) -> Option<Self> {
        let ClipContent::Audio(audio) = &clip.content else {
            return None;
        };
        let warp = audio
            .warp
            .as_ref()
            .filter(|w| w.mode == WarpMode::Spectral)?;
        let offset = audio.offset.beats()?;
        if frames == 0 || channels == 0 || rate == 0 {
            return None;
        }
        let map = Map {
            shift: (offset - clip.start).to_f64(),
            segments: warp.segments(rate).collect(),
            rate,
        };
        let pitches = Pitches::new(audio, warp);
        let passthrough = pitches.none() && map.plays_as_is(tempo, clip.start, clip.end());
        let mut plan = Self {
            tempo: tempo.clone(),
            until: clip.end(),
            map,
            pitches,
            channels: channels.min(2),
            sizes: musicable_stretch::sizes(rate),
            crossfade: (CROSSFADE_SECONDS * f64::from(rate)).round() as usize,
            passthrough,
            segments: Vec::new(),
        };
        if !passthrough {
            let start = tempo.sample_at(clip.start, rate).floor() as i64;
            let end = tempo.sample_at(clip.end(), rate).ceil() as i64;
            plan.segments = plan.cut(warp, frames, start - margin(), end + margin());
        }
        Some(plan)
    }

    /// The segments, in order of where they start.
    pub fn segments(&self) -> &[SegmentPlan] {
        &self.segments
    }

    /// The rate the segments render at: the file's.
    pub fn sample_rate(&self) -> u32 {
        self.map.rate
    }

    /// The channels the segments render: one or two.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Whether the clip plays the file as it is, as Re-Pitch would, with
    /// nothing to render.
    pub fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    /// Whether the segments play at `tempo`: whether it places the song's
    /// beats as the plan's tempo did, up to the clip's end.
    pub fn is_fresh(&self, tempo: &TempoMap) -> bool {
        fn upto(map: &TempoMap, until: Beats) -> &[TempoChange] {
            let tempos = map.tempos();
            &tempos[..tempos.partition_point(|c| c.at <= until)]
        }
        upto(&self.tempo, self.until) == upto(tempo, self.until)
    }

    /// The segments of a file `frames` long that play between grid frames
    /// `from` and `to`.
    fn cut(&self, warp: &Warp, frames: u64, from: i64, to: i64) -> Vec<SegmentPlan> {
        let cuts = cuts(warp, frames, self.map.rate);
        let crossfade = self.crossfade as i64;
        let grid = |frame: u64| {
            let own = self.map.own_of(frame as f64);
            (self.map.grid_of(&self.tempo, own) - SLACK).ceil() as i64
        };
        // Where each segment's own audio starts, and where the last one's
        // ends: cuts that land closer together than two crossfades merge.
        let mut starts: Vec<i64> = Vec::with_capacity(cuts.len());
        for &cut in &cuts[..cuts.len() - 1] {
            let at = grid(cut);
            if starts.last().is_none_or(|&last| at - last >= 2 * crossfade) {
                starts.push(at);
            }
        }
        let end = grid(frames) + crossfade;
        while starts.len() > 1 && end - starts[starts.len() - 1] < 2 * crossfade {
            starts.pop();
        }
        starts.push(end);
        starts
            .windows(2)
            .map(|w| (w[0] - crossfade, w[1]))
            .filter(|&(start, end)| start < to && end > from)
            .map(|(start, end)| self.segment(start, (end - start) as usize))
            .collect()
    }

    /// The segment whose output starts on grid frame `start`, `frames`
    /// long: a block of output to settle the stretcher, then its own.
    fn segment(&self, start: i64, frames: usize) -> SegmentPlan {
        let (tempo, map, sizes) = (&self.tempo, &self.map, &self.sizes);
        let discard = sizes.block;
        let first = start - discard as i64;
        let count = (discard + frames).div_ceil(sizes.interval);
        let clock = |call: usize| first + (call * sizes.interval) as i64;
        let heard = |call: usize| (clock(call) + sizes.output_latency as i64) as f64;
        let frontiers: Vec<i64> = (0..=count)
            .map(|call| map.frontier(tempo, sizes, clock(call)))
            .collect();
        let calls = (0..count)
            .map(|call| Call {
                input: (frontiers[call + 1] - frontiers[call]).max(0) as u32,
                pitch: self.pitches.at(map.own_at(tempo, heard(call))),
            })
            .collect();
        let mut segment = SegmentPlan {
            key: 0,
            start,
            frames,
            read_from: frontiers[0] - (sizes.block + sizes.interval) as i64,
            seek_rate: map.speed(tempo, heard(0)) as f32,
            discard,
            calls,
        };
        segment.key = segment.hash(map.rate, self.channels);
        segment
    }
}

/// Where a clip's file is cut into segments, in its frames: at its start,
/// then 4 to 8 seconds on each time, on the strongest transient in reach,
/// the one nearest 6 seconds on among equals, or else on the lattice, and
/// at its end, `frames` on.
fn cuts(warp: &Warp, frames: u64, rate: u32) -> Vec<u64> {
    let seconds = |s: f64| (s * f64::from(rate)).round() as u64;
    let (shortest, spacing, longest) = (
        seconds(SPACING_SECONDS - SNAP_SECONDS),
        seconds(SPACING_SECONDS),
        seconds(SPACING_SECONDS + SNAP_SECONDS),
    );
    let lattice = seconds(LATTICE_SECONDS).max(1);
    let transients: Vec<(u64, f32)> = warp.transients().map(|o| (o.frame, o.strength)).collect();
    let mut cuts = vec![0];
    let mut last = 0;
    while frames - last > longest {
        let (from, to, aim) = (last + shortest, last + longest, last + spacing);
        let reach = &transients[transients.partition_point(|t| t.0 < from)..];
        let reach = &reach[..reach.partition_point(|t| t.0 <= to)];
        let off = |frame: u64| frame.abs_diff(aim);
        let strongest = reach
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1).then(off(b.0).cmp(&off(a.0))));
        let cut = match strongest {
            Some(&(frame, _)) => frame,
            None => {
                let on = (aim + lattice / 2) / lattice * lattice;
                if (from..=to).contains(&on) { on } else { aim }
            }
        };
        cuts.push(cut);
        last = cut;
    }
    cuts.push(frames);
    cuts
}

/// One segment of a clip in Spectral: the grid frames it plays, and the
/// calls to a fresh stretcher that render them.
#[derive(Debug)]
pub struct SegmentPlan {
    key: u64,
    /// Its first grid frame, and how many it has: it fades in over the
    /// first crossfade, as the segment before fades out, and out over the
    /// last.
    start: i64,
    frames: usize,
    /// Where in the file the stretcher's preroll starts: a block and an
    /// interval of the file before the first call's input.
    read_from: i64,
    /// How fast the file plays where it starts, for the stretcher's seek.
    seek_rate: f32,
    /// The frames of output before the segment's own, while the stretcher
    /// settles: a block.
    discard: usize,
    /// Each an interval of output.
    calls: Vec<Call>,
}

/// One call to a segment's stretcher: an interval of output, playing
/// `input` frames of the file on from the last call's, at `pitch`.
#[derive(Debug, Clone, Copy)]
struct Call {
    input: u32,
    pitch: Pitch,
}

impl SegmentPlan {
    /// What its audio is found again by in the render cache, with its file:
    /// everything the render depends on, but not where in the song it
    /// plays, so a copy of its clip at the same tempo has the same keys.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// Its first grid frame.
    pub fn start(&self) -> i64 {
        self.start
    }

    /// How many grid frames it has.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The grid frame after its last.
    pub fn end(&self) -> i64 {
        self.start + self.frames as i64
    }

    fn hash(&self, rate: u32, channels: usize) -> u64 {
        let mut hasher = SipHasher13::new();
        hasher.write_u32(VERSION);
        hasher.write_u32(musicable_stretch::VERSION);
        hasher.write_u32(SEED);
        hasher.write_u32(rate);
        hasher.write_u64(channels as u64);
        hasher.write_u64(self.discard as u64);
        hasher.write_u64(self.frames as u64);
        hasher.write_i64(self.read_from);
        hasher.write_u32(self.seek_rate.to_bits());
        for call in &self.calls {
            hasher.write_u32(call.input);
            hasher.write_u32(call.pitch.transpose.to_bits());
            hasher.write_u32(call.pitch.formants.to_bits());
            hasher.write_u8(u8::from(call.pitch.compensate));
        }
        hasher.finish()
    }

    /// How much of grid frame `grid` of its audio plays: a raised cosine up
    /// over its first `crossfade` frames and down over its last, so two
    /// segments crossfading add up to one.
    fn ramp(&self, grid: i64, crossfade: usize) -> f32 {
        ramp(self.start, self.end(), grid, crossfade)
    }
}

/// How much of grid frame `grid` of audio from `start` to `end` plays,
/// fading in and out over `crossfade` frames at its ends.
fn ramp(start: i64, end: i64, grid: i64, crossfade: usize) -> f32 {
    let length = crossfade as i64;
    let (into, left) = (grid - start, end - grid);
    if into < length {
        rise(into, crossfade)
    } else if left <= length {
        1.0 - rise(length - left, crossfade)
    } else {
        1.0
    }
}

/// A raised cosine from 0 to 1 over `length` frames, at frame `at` of them:
/// 0 before them, 1 after.
fn rise(at: i64, length: usize) -> f32 {
    if at <= 0 {
        return 0.0;
    }
    if at >= length as i64 {
        return 1.0;
    }
    let x = at as f64 / length as f64;
    (0.5 - 0.5 * (std::f64::consts::PI * x).cos()) as f32
}

/// Renders `segment` of `plan` from `reader`, the clip's file: its frames
/// at the file's rate, the same every time, bit for bit, on the same build.
/// It allocates, so it runs off the audio thread.
pub fn render(
    plan: &Plan,
    segment: &SegmentPlan,
    reader: &mut dyn AudioReader,
) -> io::Result<AudioData> {
    let (channels, sizes) = (plan.channels, plan.sizes);
    let keep = sizes.block + sizes.interval;
    let mut stretch = Stretch::new(channels, plan.map.rate, SEED);
    let mut input = Input::new(channels, keep);
    let preroll = input.read(reader, segment.read_from, keep)?;
    stretch.seek(&preroll[..channels], f64::from(segment.seek_rate));
    let mut at = segment.read_from + keep as i64;
    let mut output: Vec<Vec<f32>> = (0..channels)
        .map(|_| Vec::with_capacity(segment.frames + sizes.interval))
        .collect();
    let mut out = vec![vec![0.0; sizes.interval]; channels];
    let mut skip = segment.discard;
    let mut pitch = None;
    for call in &segment.calls {
        if pitch != Some(call.pitch) {
            call.pitch.apply(&mut stretch);
            pitch = Some(call.pitch);
        }
        let frames = call.input as usize;
        let tail = frames.min(keep);
        let tails = input.read(reader, at + (frames - tail) as i64, tail)?;
        let mut outs: Vec<&mut [f32]> = out.iter_mut().map(Vec::as_mut_slice).collect();
        stretch.process_tail(&tails[..channels], frames, &mut outs);
        let from = skip.min(sizes.interval);
        for (channel, out) in output.iter_mut().zip(&out) {
            channel.extend_from_slice(&out[from..]);
        }
        skip -= from;
        at += frames as i64;
    }
    for channel in &mut output {
        channel.truncate(segment.frames);
        channel.resize(segment.frames, 0.0);
    }
    Ok(AudioData::new(plan.map.rate, output))
}

/// Reads stretches of a file, a channel at a time, for a render.
struct Input {
    channels: Vec<Vec<f32>>,
    interleaved: Vec<f32>,
}

impl Input {
    fn new(channels: usize, room: usize) -> Self {
        Self {
            channels: vec![vec![0.0; room]; channels],
            interleaved: Vec::new(),
        }
    }

    /// The file's `frames` frames from frame `start` on, a slice for each
    /// channel: silent before the file starts and after it ends.
    fn read(
        &mut self,
        reader: &mut dyn AudioReader,
        start: i64,
        frames: usize,
    ) -> io::Result<[&[f32]; 2]> {
        let count = reader.channels();
        for channel in &mut self.channels {
            channel[..frames].fill(0.0);
        }
        // The frames before the file, and those in it.
        let skip = (-start).clamp(0, frames as i64) as usize;
        let at = start.max(0) as u64;
        let wanted = (frames - skip).min(reader.frames().saturating_sub(at) as usize);
        self.interleaved.resize(wanted * count, 0.0);
        let mut done = 0;
        while done < wanted {
            let read = reader.read(at + done as u64, &mut self.interleaved[done * count..])?;
            if read == 0 {
                break;
            }
            done += read;
        }
        for (frame, samples) in self.interleaved[..done * count]
            .chunks_exact(count)
            .enumerate()
        {
            for (channel, &sample) in self.channels.iter_mut().zip(samples) {
                channel[skip + frame] = sample;
            }
        }
        let slice = |c: usize| self.channels.get(c).map_or(&[][..], |c| &c[..frames]);
        Ok([slice(0), slice(1)])
    }
}

/// A segment of a clip in Spectral, rendered or still to be: the app fills
/// it once, and the clip's player plays it from then on.
#[derive(Debug, Default)]
pub struct Rendered(OnceLock<AudioData>);

impl Rendered {
    pub fn new() -> Self {
        Self::default()
    }

    /// Already filled with `audio`.
    pub fn filled(audio: AudioData) -> Self {
        let rendered = Self::new();
        rendered.fill(audio);
        rendered
    }

    /// Fills it with `audio`, unless it's filled already, and returns
    /// whether it did.
    pub fn fill(&self, audio: AudioData) -> bool {
        self.0.set(audio).is_ok()
    }

    /// The audio, once it's filled. Real-time safe.
    pub fn get(&self) -> Option<&AudioData> {
        self.0.get()
    }

    pub fn is_ready(&self) -> bool {
        self.get().is_some()
    }
}

/// What a clip in Spectral plays: its plan, and a slot for each of its
/// segments, in the same order.
#[derive(Debug, Clone)]
pub struct Renders {
    pub plan: Arc<Plan>,
    pub slots: Arc<[Arc<Rendered>]>,
}

impl Renders {
    /// `plan`, with none of its segments rendered.
    pub fn new(plan: Plan) -> Self {
        let slots = plan.segments.iter().map(|_| Arc::new(Rendered::new()));
        Self {
            slots: slots.collect(),
            plan: Arc::new(plan),
        }
    }

    /// Renders every segment that isn't yet, from `reader`, the clip's
    /// file, here and now.
    pub fn render_all(&self, reader: &mut dyn AudioReader) -> io::Result<()> {
        for (segment, slot) in self.plan.segments.iter().zip(self.slots.iter()) {
            if !slot.is_ready() {
                slot.fill(render(&self.plan, segment, reader)?);
            }
        }
        Ok(())
    }

    /// Whether every segment is rendered.
    pub fn is_ready(&self) -> bool {
        self.slots.iter().all(|slot| slot.is_ready())
    }
}

/// How a segment plays while playback is in it: decided as playback
/// enters it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Undecided,
    Rendered,
    Live,
}

/// A clip's player in Spectral: plays its segments as they're rendered, and
/// its live stretch wherever they aren't, or the song's tempo has moved on
/// from its plan's.
#[derive(Debug)]
pub(super) struct Player {
    plan: Arc<Plan>,
    slots: Arc<[Arc<Rendered>]>,
    choices: Box<[Choice]>,
    /// The segments that fade in through `swap`, as what played before
    /// fades out.
    entering: Box<[bool]>,
    /// The segments before this one were passed by the last block.
    passed: usize,
    /// The transport's jumps as of the last block, and the song frame after
    /// it.
    last: Option<(u64, i64)>,
    /// Whether the last block played the plan's segments, at its tempo.
    fresh: bool,
    live: Option<Box<Live>>,
    /// The crossfade under way from what played before a new plan took
    /// over, or before the tempo moved, and what fades out in it.
    swap: Option<Swap>,
    outgoing: [Option<Outgoing>; 4],
    /// The rendered segments of the player the clip had before that
    /// `outgoing` plays out: kept until the player is dropped, off the
    /// audio thread.
    kept: [Option<Arc<Rendered>>; 2],
    /// From the grid to the device's rate, where they differ.
    resampler: Option<Resampler>,
    /// The file's frames on each of the device's.
    step: f64,
    /// One block's grid frames, a channel after another, `room` each.
    grid: Vec<f32>,
    room: usize,
    weights: Vec<f32>,
}

/// A crossfade from what played before, from grid frame `at`: what plays
/// now rises from silence over it, but for the live stretch, which plays
/// everything while the plan is stale, and rises from how much of it
/// played before, `live`.
#[derive(Debug, Clone, Copy)]
struct Swap {
    at: i64,
    live: f32,
}

impl Swap {
    /// How much of what fades in plays on grid frame `grid`.
    fn rise(&self, grid: i64, crossfade: usize) -> f32 {
        rise(grid - self.at, crossfade)
    }

    /// How much of the live stretch plays on grid frame `grid`, where it
    /// plays everything.
    fn live(&self, grid: i64, crossfade: usize) -> f32 {
        self.live + (1.0 - self.live) * self.rise(grid, crossfade)
    }
}

/// A rendered segment fading out from grid frame `at`, as another plan or
/// the live stretch takes over: it plays its own grid frames `offset` on
/// from the song's.
#[derive(Debug, Clone, Copy)]
struct Outgoing {
    fading: Fading,
    offset: i64,
    at: i64,
}

#[derive(Debug, Clone, Copy)]
enum Fading {
    /// One of the player's own segments.
    Own(usize),
    /// One of its `kept`, which played from grid frame `start` up to `end`.
    Kept { index: usize, start: i64, end: i64 },
}

impl Player {
    pub(super) fn new(renders: Renders) -> Self {
        assert_eq!(
            renders.slots.len(),
            renders.plan.segments.len(),
            "a slot for each segment"
        );
        let count = renders.slots.len();
        Self {
            plan: renders.plan,
            slots: renders.slots,
            choices: vec![Choice::Undecided; count].into_boxed_slice(),
            entering: vec![false; count].into_boxed_slice(),
            passed: 0,
            last: None,
            fresh: true,
            live: None,
            swap: None,
            outgoing: [None; 4],
            kept: [None, None],
            resampler: None,
            step: 1.0,
            grid: Vec::new(),
            room: 0,
            weights: Vec::new(),
        }
    }

    /// Whether, at `tempo`, it plays the file as Re-Pitch does.
    pub(super) fn plays_as_is(&self, tempo: &TempoMap) -> bool {
        self.plan.passthrough && self.plan.is_fresh(tempo)
    }

    pub(super) fn prepare(&mut self, config: &EngineConfig) {
        let rate = self.plan.map.rate;
        self.step = f64::from(rate) / f64::from(config.sample_rate);
        self.resampler = (rate != config.sample_rate)
            .then(|| Resampler::with_step_widened_at_most(self.step, WIDEST));
        let reach = self.resampler.map_or(0, |r| r.reach());
        self.room = (config.max_block as f64 * self.step).ceil() as usize + 2 * reach + 2;
        self.grid = vec![0.0; 2 * self.room];
        self.weights = vec![0.0; 2 * reach];
        let sizes = self.plan.sizes;
        let ring = self.room + sizes.interval + 1;
        self.live = Some(Box::new(Live::new(self.plan.channels, rate, sizes, ring)));
    }

    /// Where in its file the clip reads first to play song frame `frame`
    /// at `tempo` and the device's rate, live: where its live stretch would
    /// start reading if it started there.
    pub(super) fn read_start(&self, tempo: &TempoMap, frame: i64) -> i64 {
        let sizes = &self.plan.sizes;
        let grid = (frame as f64 * self.step).floor() as i64;
        let frontier = self
            .plan
            .map
            .frontier(tempo, sizes, grid - sizes.block as i64);
        frontier - (sizes.block + sizes.interval) as i64
    }

    /// The grid frames the song frames from `from` up to `to` play from.
    fn grid_range(&self, from: i64, to: i64) -> (i64, i64) {
        match self.resampler {
            None => (from, to),
            Some(r) => {
                let reach = r.reach() as i64;
                let first = (from as f64 * self.step).floor() as i64 - reach + 1;
                let last = ((to - 1) as f64 * self.step).floor() as i64 + reach + 1;
                (first, last)
            }
        }
    }

    /// Renders the clip's song frames from `from` up to `to`, in the block
    /// `ctx` renders, into `out`, a channel after another, before the
    /// clip's gain and fades. Reads `source`, the clip's file, for the live
    /// stretch. Real-time safe.
    pub(super) fn play(
        &mut self,
        ctx: &ProcessContext<'_>,
        source: &Source,
        after_loop: Option<u64>,
        from: i64,
        to: i64,
        out: &mut [f32],
    ) {
        let transport = ctx.transport;
        let jumped = self.last.is_none_or(|(jumps, _)| jumps != transport.jumps);
        let previous = self.last.map(|(_, end)| end);
        let now = transport.position as i64;
        self.last = Some((transport.jumps, now + ctx.frames as i64));
        let step = self.step;
        let grid_at = |frame: i64| (frame as f64 * step).floor() as i64;
        // How far the tempo moved the playhead since the last block, as it
        // keeps its place in beats: what played plays on from where it was.
        let moved = match previous {
            Some(previous) if !jumped => grid_at(now) - grid_at(previous),
            _ => 0,
        };
        if jumped {
            self.choices.fill(Choice::Undecided);
            self.entering.fill(false);
            self.passed = 0;
            self.swap = None;
            self.outgoing = [None; 4];
        } else if moved != 0 {
            self.shift(moved);
        }
        let fresh = self.plan.is_fresh(ctx.tempo);
        if self.fresh && !fresh && !jumped {
            self.fade_out_rendered(grid_at(now), moved);
        }
        self.fresh = fresh;
        let (first, last) = self.grid_range(from, to);
        let width = (last - first) as usize;
        let Self {
            plan,
            slots,
            choices,
            entering,
            passed,
            live,
            swap,
            grid,
            ..
        } = self;
        let Some(live) = live.as_mut() else {
            return;
        };
        let grid = &mut grid[..2 * width];
        grid.fill(0.0);
        let segments = &plan.segments;
        let crossfade = plan.crossfade;
        let swap = *swap;
        if fresh {
            // The segments it passed decide again when playback comes back.
            let entered = segments.partition_point(|s| s.end() <= first);
            for choice in &mut choices[(*passed).min(entered)..entered] {
                *choice = Choice::Undecided;
            }
            *passed = entered;
            let mut span = (i64::MAX, i64::MIN);
            let mut k = entered;
            while k < segments.len() && segments[k].start < last {
                if choices[k] == Choice::Undecided {
                    choices[k] = match slots[k].is_ready() {
                        true => Choice::Rendered,
                        false => Choice::Live,
                    };
                }
                if choices[k] == Choice::Live {
                    span.0 = span.0.min(segments[k].start.max(first));
                    span.1 = span.1.max(segments[k].end().min(last));
                }
                k += 1;
            }
            if span.0 < span.1 {
                live.ensure(plan, ctx.tempo, source, after_loop, span.0, span.1);
            }
            for (k, segment) in segments.iter().enumerate().take(k).skip(entered) {
                let (from, to) = (segment.start.max(first), segment.end().min(last));
                let rising = swap.filter(|_| entering[k]);
                let ramp = |g: i64| {
                    let rises = rising.map_or(1.0, |swap| swap.rise(g, crossfade));
                    segment.ramp(g, crossfade) * rises
                };
                match (choices[k], slots[k].get()) {
                    (Choice::Rendered, Some(audio)) => {
                        add_audio(grid, width, first, audio, segment.start, from, to, ramp);
                    }
                    (Choice::Live, _) => live.add(grid, width, first, from, to, ramp),
                    _ => {}
                }
            }
        } else {
            // The song's tempo moved on from the plan's: the live stretch
            // follows it, and the segments playback is in, as the plan has
            // them, play it on to their ends once the plan's tempo is back.
            live.ensure(plan, ctx.tempo, source, after_loop, first, last);
            let level = |g: i64| swap.map_or(1.0, |swap| swap.live(g, crossfade));
            live.add(grid, width, first, first, last, level);
            let map = &plan.map;
            let at = map.grid_of(&plan.tempo, map.own_at(ctx.tempo, first as f64));
            let until = at.ceil() as i64 + (width + crossfade) as i64;
            let entered = segments.partition_point(|s| s.end() <= at.floor() as i64);
            choices.fill(Choice::Undecided);
            for (choice, _) in (choices[entered..].iter_mut())
                .zip(&segments[entered..])
                .take_while(|(_, s)| s.start < until)
            {
                *choice = Choice::Live;
            }
            *passed = entered;
        }
        self.cross_out(first, last, width);
        self.write_out(from, to, first, width, out);
    }

    /// Moves everything playing `by` grid frames later in the song, as the
    /// tempo moves the playhead, so it plays on from where it was.
    fn shift(&mut self, by: i64) {
        if let Some(live) = self.live.as_mut() {
            live.shift(by);
        }
        for outgoing in self.outgoing.iter_mut().flatten() {
            outgoing.offset -= by;
            outgoing.at += by;
        }
        if let Some(swap) = self.swap.as_mut() {
            swap.at += by;
        }
    }

    /// The tempo moved under the clip as it played the plan's segments,
    /// moving the playhead `moved` grid frames on, to grid frame `at`: the
    /// segments it played rendered there crossfade out from it, as the live
    /// stretch, which plays everything now, rises from how much of it
    /// played.
    fn fade_out_rendered(&mut self, at: i64, moved: i64) {
        let was = at - moved;
        let crossfade = self.plan.crossfade;
        let segments = &self.plan.segments;
        let entered = segments.partition_point(|s| s.end() <= was);
        let mut live = 0.0;
        let mut free = self.outgoing.iter_mut().filter(|o| o.is_none());
        for (k, segment) in segments.iter().enumerate().skip(entered) {
            if segment.start > was {
                break;
            }
            match self.choices[k] {
                Choice::Rendered => {
                    if let Some(free) = free.next() {
                        *free = Some(Outgoing {
                            fading: Fading::Own(k),
                            offset: -moved,
                            at,
                        });
                    }
                }
                Choice::Live => live += segment.ramp(was, crossfade),
                Choice::Undecided => {}
            }
        }
        self.entering.fill(false);
        self.swap = Some(Swap {
            at,
            live: live.min(1.0),
        });
    }

    /// Adds what fades out to the grid frames from `first` up to `last`,
    /// `width` of them, and ends the crossfades that are over.
    fn cross_out(&mut self, first: i64, last: i64, width: usize) {
        let crossfade = self.plan.crossfade;
        if self
            .swap
            .is_some_and(|swap| first >= swap.at + crossfade as i64)
        {
            self.swap = None;
            self.entering.fill(false);
        }
        let grid = &mut self.grid[..2 * width];
        for entry in &mut self.outgoing {
            let Some(outgoing) = *entry else {
                continue;
            };
            let until = outgoing.at + crossfade as i64;
            if first >= until {
                *entry = None;
                continue;
            }
            let (slot, start, end) = match outgoing.fading {
                Fading::Own(segment) => {
                    let plan = &self.plan.segments[segment];
                    (Some(&self.slots[segment]), plan.start, plan.end())
                }
                Fading::Kept { index, start, end } => (self.kept[index].as_ref(), start, end),
            };
            let Some(audio) = slot.and_then(|slot| slot.get()) else {
                continue;
            };
            let offset = outgoing.offset;
            let (from, to) = (
                (start - offset).max(first),
                (end - offset).min(last).min(until),
            );
            let fade = |g: i64| {
                ramp(start, end, g + offset, crossfade) * (1.0 - rise(g - outgoing.at, crossfade))
            };
            add_audio(grid, width, first, audio, start - offset, from, to, fade);
        }
    }

    /// Writes the song frames from `from` up to `to` into `out`, a channel
    /// after another, from the grid frames from `first`, `width` of them:
    /// as they are, or resampled to the device's rate.
    fn write_out(&mut self, from: i64, to: i64, first: i64, width: usize, out: &mut [f32]) {
        let frames = (to - from) as usize;
        let grid = &self.grid[..2 * width];
        match self.resampler {
            None => {
                out[..frames].copy_from_slice(&grid[..frames]);
                out[frames..2 * frames].copy_from_slice(&grid[width..width + frames]);
            }
            Some(resampler) => {
                let weights = &mut self.weights[..];
                for (k, n) in (from..to).enumerate() {
                    let x = n as f64 * self.step;
                    let base = (resampler.weights(x, weights) - first) as usize;
                    for c in 0..2 {
                        let input = &grid[c * width + base..][..weights.len()];
                        out[c * frames + k] =
                            input.iter().zip(weights.iter()).map(|(x, w)| x * w).sum();
                    }
                }
            }
        }
    }

    /// Takes over from `previous`, the player the clip had in the graph
    /// before: its live stretch, how it was playing each segment the two
    /// plans share, and a crossfade from the segments it was playing that
    /// this plan hasn't. Real-time safe.
    pub(super) fn adopt(&mut self, previous: &mut Player) {
        let same = self.plan.channels == previous.plan.channels
            && self.plan.map.rate == previous.plan.map.rate
            && self.step == previous.step;
        if !same {
            return;
        }
        std::mem::swap(&mut self.live, &mut previous.live);
        self.last = previous.last;
        self.fresh = previous.fresh;
        let Some((_, end)) = previous.last else {
            return;
        };
        let at = (end as f64 * self.step).floor() as i64;
        let crossfade = self.plan.crossfade;
        let (new, old) = (&self.plan.segments, &previous.plan.segments);
        // How the old player played each of its segments: all live, where
        // its plan was stale.
        let played = |j: usize| match previous.fresh {
            true => previous.choices[j],
            false => Choice::Live,
        };
        // A segment of the old plan this one has too, and plays as it was
        // played: live, or rendered, which it is in both.
        let shared = |j: usize| {
            let segment = &old[j];
            let k = new.partition_point(|s| s.start < segment.start);
            let same = new
                .get(k)
                .is_some_and(|s| s.start == segment.start && s.key == segment.key);
            let plays =
                played(j) != Choice::Rendered || self.slots.get(k).is_some_and(|s| s.is_ready());
            (same && plays).then_some((k, played(j)))
        };
        for j in 0..old.len() {
            if let Some((k, played)) = shared(j) {
                self.choices[k] = played;
            }
        }
        // The segments playing at the swap.
        let playing = |segments: &[SegmentPlan]| {
            let from = segments.partition_point(|s| s.end() <= at);
            from..segments.partition_point(|s| s.start <= at).max(from)
        };
        let mut live = if previous.fresh { 0.0 } else { 1.0 };
        let mut kept = 0;
        for j in playing(old) {
            match played(j) {
                Choice::Live if previous.fresh => live += old[j].ramp(at, crossfade),
                Choice::Rendered if shared(j).is_none() && kept < self.kept.len() => {
                    self.kept[kept] = Some(previous.slots[j].clone());
                    self.outgoing[kept] = Some(Outgoing {
                        fading: Fading::Kept {
                            index: kept,
                            start: old[j].start,
                            end: old[j].end(),
                        },
                        offset: 0,
                        at,
                    });
                    kept += 1;
                }
                _ => {}
            }
        }
        // Where the live stretch was playing, it plays on to the ends of
        // the segments it's in, so the handover falls on a cut; what else
        // takes over fades in.
        let mut entering = false;
        for k in playing(new) {
            if self.choices[k] == Choice::Undecided {
                if live > 0.0 {
                    self.choices[k] = Choice::Live;
                } else {
                    self.entering[k] = true;
                    entering = true;
                }
            }
        }
        if entering || kept > 0 {
            self.swap = Some(Swap {
                at,
                live: f32::min(live, 1.0),
            });
        }
    }
}

/// Adds `audio`'s frames, which start on grid frame `start`, from grid frame
/// `from` up to `to`, each times `ramp` of it, to `grid`, whose channels are
/// `width` frames from grid frame `first`.
#[allow(clippy::too_many_arguments)]
fn add_audio(
    grid: &mut [f32],
    width: usize,
    first: i64,
    audio: &AudioData,
    start: i64,
    from: i64,
    to: i64,
    ramp: impl Fn(i64) -> f32,
) {
    let frames = audio.frames() as i64;
    let to = to.min(start + frames);
    let last = audio.channels() - 1;
    for c in 0..2 {
        let samples = audio.channel(c.min(last));
        let out = &mut grid[c * width..][..width];
        for g in from..to {
            out[(g - first) as usize] += samples[(g - start) as usize] * ramp(g);
        }
    }
}

/// A clip's file stretched as it plays, while its segments render and while
/// the song's tempo has moved on from its plan's: one stretcher, running up
/// to an interval ahead of playback, and what it rendered last. As the
/// tempo moves the playhead, what it rendered moves with it, and it plays
/// on without a break, reading on to where the new tempo has the file.
#[derive(Debug)]
struct Live {
    stretch: Stretch,
    sizes: Sizes,
    channels: usize,
    running: bool,
    /// The grid frame whose spectrum it makes next, and the file frame its
    /// input has reached.
    clock: i64,
    frontier: i64,
    pitch: Option<Pitch>,
    /// What it rendered last, up to the clock, a channel after another,
    /// `room` frames each: `held` of them, the last at `written - 1`.
    ring: Vec<f32>,
    room: usize,
    written: usize,
    held: usize,
    /// The end of one call's input, and one call's output, a channel after
    /// another.
    input: Vec<f32>,
    output: Vec<f32>,
}

impl Live {
    /// A live stretch of `channels` channels of a file at `rate`, keeping
    /// the last `room` frames it renders. Allocates.
    fn new(channels: usize, rate: u32, sizes: Sizes, room: usize) -> Self {
        Self {
            stretch: Stretch::new(channels, rate, SEED),
            sizes,
            channels,
            running: false,
            clock: 0,
            frontier: 0,
            pitch: None,
            ring: vec![0.0; channels * room],
            room,
            written: 0,
            held: 0,
            input: vec![0.0; channels * (sizes.block + sizes.interval)],
            output: vec![0.0; channels * sizes.interval],
        }
    }

    /// Renders on until it has the grid frames from `from` up to `to`, at
    /// `tempo`, starting again where it can't get there from where it is.
    fn ensure(
        &mut self,
        plan: &Plan,
        tempo: &TempoMap,
        source: &Source,
        after_loop: Option<u64>,
        from: i64,
        to: i64,
    ) {
        let (map, sizes) = (&plan.map, self.sizes);
        // Starting a block early, so its stretcher has settled.
        let warm = sizes.block as i64;
        let held = self.clock - self.held as i64;
        if !self.running || from < held || from > self.clock + warm {
            self.restart(map, tempo, source, after_loop, from - warm);
        }
        while self.clock < to {
            self.call(plan, tempo, source, after_loop);
        }
    }

    /// Moves what it rendered `by` grid frames later in the song, as the
    /// tempo moves the playhead: it plays on from where it was, without a
    /// break, and its next calls read on to where the new tempo has the
    /// file.
    fn shift(&mut self, by: i64) {
        self.clock += by;
    }

    /// Starts afresh on grid frame `at`, from the file there.
    fn restart(
        &mut self,
        map: &Map,
        tempo: &TempoMap,
        source: &Source,
        after_loop: Option<u64>,
        at: i64,
    ) {
        let sizes = self.sizes;
        let keep = sizes.block + sizes.interval;
        self.stretch.reset();
        self.pitch = None;
        let frontier = map.frontier(tempo, &sizes, at);
        self.read(source, after_loop, frontier - keep as i64, keep);
        let rate = map.speed(tempo, (at + sizes.output_latency as i64) as f64);
        let (left, right) = self.input[..self.channels * keep].split_at(keep);
        let preroll = [left, right];
        self.stretch.seek(&preroll[..self.channels], rate);
        self.clock = at;
        self.frontier = frontier;
        self.held = 0;
        self.running = true;
    }

    /// Renders an interval more, reading on in the file to where `tempo`
    /// says it should have read for the spectrum after.
    fn call(&mut self, plan: &Plan, tempo: &TempoMap, source: &Source, after_loop: Option<u64>) {
        let (map, sizes) = (&plan.map, self.sizes);
        let (interval, keep) = (sizes.interval, sizes.block + sizes.interval);
        let heard = (self.clock + sizes.output_latency as i64) as f64;
        let pitch = plan.pitches.at(map.own_at(tempo, heard));
        if self.pitch != Some(pitch) {
            pitch.apply(&mut self.stretch);
            self.pitch = Some(pitch);
        }
        let target = map.frontier(tempo, &sizes, self.clock + interval as i64);
        let frames = (target - self.frontier).max(0) as usize;
        let tail = frames.min(keep);
        self.read(
            source,
            after_loop,
            self.frontier + (frames - tail) as i64,
            tail,
        );
        let channels = self.channels;
        let (left, right) = self.input[..channels * tail].split_at(tail);
        let tails = [left, right];
        let (out_left, out_right) = self.output.split_at_mut(interval);
        let mut outs = [out_left, out_right];
        self.stretch
            .process_tail(&tails[..channels], frames, &mut outs[..channels]);
        for c in 0..channels {
            let ring = &mut self.ring[c * self.room..][..self.room];
            let out = &self.output[c * interval..][..interval];
            let fits = (self.room - self.written).min(interval);
            ring[self.written..self.written + fits].copy_from_slice(&out[..fits]);
            ring[..interval - fits].copy_from_slice(&out[fits..]);
        }
        self.written = (self.written + interval) % self.room;
        self.held = (self.held + interval).min(self.room);
        self.clock += interval as i64;
        self.frontier += frames as i64;
    }

    /// Reads `frames` frames of the file from frame `start` into its
    /// input, a channel after another, telling a stream first if it doesn't
    /// have them in memory.
    fn read(&mut self, source: &Source, after_loop: Option<u64>, start: i64, frames: usize) {
        if let Source::Stream(stream) = source {
            let at = start.max(0) as u64;
            if !stream.is_loaded(at, frames as u64) {
                stream.hint(at, after_loop);
            }
        }
        super::read(
            source,
            start,
            &mut self.input[..self.channels * frames],
            self.channels,
        );
    }

    /// Adds its grid frames from `from` up to `to` that it has, each times
    /// `ramp` of it, to `grid`, whose channels are `width` frames from grid
    /// frame `first`.
    fn add(
        &self,
        grid: &mut [f32],
        width: usize,
        first: i64,
        from: i64,
        to: i64,
        ramp: impl Fn(i64) -> f32,
    ) {
        let from = from.max(self.clock - self.held as i64);
        let to = to.min(self.clock);
        let last = self.channels - 1;
        for c in 0..2 {
            let ring = &self.ring[c.min(last) * self.room..][..self.room];
            let out = &mut grid[c * width..][..width];
            for g in from..to {
                let back = (self.clock - g) as usize;
                let at = (self.written + self.room - back) % self.room;
                out[(g - first) as usize] += ring[at] * ramp(g);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use musicable_model::automation::Point;
    use musicable_model::clip::{AudioClip, ClipId, Offset};
    use musicable_model::time::TimeSignature;
    use musicable_model::warp::{Onset, Onsets, Origin};

    use super::*;
    use crate::clips::AudioClips;
    use crate::media::DataReader;
    use crate::{AudioBuffer, Inputs, Node, TransportState, rt};

    const RATE: u32 = 48_000;

    fn song(bpm: f64) -> TempoMap {
        TempoMap::new(bpm, TimeSignature::FOUR_FOUR)
    }

    /// A stereo file at `rate`, `seconds` long, of `left` and `right`, each
    /// a function of the time in seconds.
    fn file(
        rate: u32,
        seconds: f64,
        left: impl Fn(f64) -> f64,
        right: impl Fn(f64) -> f64,
    ) -> Arc<AudioData> {
        let frames = (seconds * f64::from(rate)) as usize;
        let time = |i: usize| i as f64 / f64::from(rate);
        let left = (0..frames).map(|i| left(time(i)) as f32).collect();
        let right = (0..frames).map(|i| right(time(i)) as f32).collect();
        Arc::new(AudioData::new(rate, vec![left, right]))
    }

    fn sine(frequency: f64) -> impl Fn(f64) -> f64 {
        move |t| 0.5 * (TAU * frequency * t).sin()
    }

    /// A chord on the left and a fifth on the right.
    fn chord(rate: u32, seconds: f64) -> Arc<AudioData> {
        let tone = |f: f64, t: f64| (TAU * f * t).sin();
        file(
            rate,
            seconds,
            move |t| 0.2 * (tone(220.0, t) + tone(277.2, t) + tone(329.6, t)),
            move |t| 0.3 * (tone(220.0, t) + tone(330.0, t)),
        )
    }

    /// A clip from beat `start`, `length` beats long, of a file warped in
    /// Spectral at `tempo` from its start, which plays at the clip's start.
    fn stretched(id: u32, start: f64, length: f64, tempo: f64) -> Clip {
        let mut audio = AudioClip::new("mix.wav");
        let mut warp = Warp::new(0, Beats::ZERO, tempo);
        warp.mode = WarpMode::Spectral;
        audio.warp = Some(warp);
        audio.offset = Offset::Beats(Beats::ZERO);
        Clip {
            id: ClipId(id),
            name: String::new(),
            start: Beats::from_f64(start),
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

    fn plan(clip: &Clip, data: &AudioData, tempo: &TempoMap) -> Plan {
        let (channels, rate, frames) = (data.channels(), data.sample_rate(), data.frames());
        Plan::new(clip, tempo, channels, rate, frames).expect("a clip in Spectral")
    }

    /// `clip`'s renders at `tempo`, with every segment rendered.
    fn rendered(clip: &Clip, data: &Arc<AudioData>, tempo: &TempoMap) -> Renders {
        let renders = Renders::new(plan(clip, data, tempo));
        renders.render_all(&mut DataReader(data.clone())).unwrap();
        renders
    }

    /// A player of `clip`, playing `renders`.
    fn player(
        clip: &Clip,
        data: &Arc<AudioData>,
        renders: &Renders,
        tempo: &TempoMap,
    ) -> AudioClips {
        let mut clips = AudioClips::new();
        let source = Some(Source::Whole(data.clone()));
        clips.add_rendered(clip, source, Some(renders.clone()), tempo);
        clips
    }

    /// A song playing clips, as the engine plays them.
    struct Rig {
        clips: AudioClips,
        tempo: TempoMap,
        config: EngineConfig,
        position: u64,
        jumps: u64,
        buffer: AudioBuffer,
    }

    impl Rig {
        fn new(clips: AudioClips, tempo: &TempoMap, rate: u32, block: usize) -> Self {
            let config = EngineConfig {
                sample_rate: rate,
                max_block: block,
            };
            let mut clips = clips;
            clips.prepare(&config);
            Self {
                clips,
                tempo: tempo.clone(),
                config,
                position: 0,
                jumps: 0,
                buffer: AudioBuffer::new(2, block),
            }
        }

        /// Plays `frames` frames on, in blocks as long as they go, and
        /// returns the two channels.
        fn play(&mut self, frames: usize) -> [Vec<f32>; 2] {
            let (mut left, mut right) = (Vec::new(), Vec::new());
            while left.len() < frames {
                let n = self.config.max_block.min(frames - left.len());
                let ctx = ProcessContext {
                    frames: n,
                    sample_rate: self.config.sample_rate,
                    time: 0,
                    transport: TransportState {
                        playing: true,
                        position: self.position,
                        jumps: self.jumps,
                        ..TransportState::default()
                    },
                    tempo: &self.tempo,
                    events: &[],
                    input: crate::AudioRef::empty(n),
                    midi: &[],
                };
                let (clips, buffer) = (&mut self.clips, &mut self.buffer);
                rt::scope(|| clips.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n)));
                left.extend_from_slice(self.buffer.view(n).channel(0));
                right.extend_from_slice(self.buffer.view(n).channel(1));
                self.position += n as u64;
            }
            [left, right]
        }

        fn locate(&mut self, position: u64) {
            self.position = position;
            self.jumps += 1;
        }

        /// Changes the song's tempo to `tempo`, the playhead keeping its
        /// place in beats, as the engine's transport does.
        fn set_tempo(&mut self, tempo: TempoMap) {
            let rate = self.config.sample_rate;
            let at = Beats::from_f64(self.tempo.beats_at(self.position as f64, rate));
            self.position = tempo.frame_at(at, rate) as u64;
            self.tempo = tempo;
        }

        /// Swaps in a new graph's `clips`, as the engine does between
        /// blocks.
        fn swap(&mut self, clips: AudioClips) {
            let mut clips = clips;
            clips.prepare(&self.config);
            rt::scope(|| clips.adopt(&mut self.clips));
            self.clips = clips;
        }
    }

    /// How often `signal` crosses zero upwards, in Hz at `rate`.
    fn frequency(signal: &[f32], rate: u32) -> f64 {
        let ups = signal.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0);
        ups.count() as f64 * f64::from(rate) / signal.len() as f64
    }

    fn level(signal: &[f32]) -> f32 {
        let power: f32 = signal.iter().map(|s| s * s).sum();
        (power / signal.len() as f32).sqrt()
    }

    /// How strong `signal` is at `frequency`, in Hz at `rate`, through a
    /// Hann window.
    fn strength(signal: &[f32], frequency: f64, rate: u32) -> f64 {
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &sample) in signal.iter().enumerate() {
            let window = 0.5 - 0.5 * (TAU * i as f64 / n).cos();
            let phase = TAU * frequency * i as f64 / f64::from(rate);
            re += f64::from(sample) * window * phase.cos();
            im += f64::from(sample) * window * phase.sin();
        }
        re.hypot(im)
    }

    /// The largest step from one sample to the next.
    fn steepest(signal: &[f32]) -> f32 {
        signal
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn files_are_cut_at_the_strongest_transient_in_reach_or_on_the_lattice() {
        let second = u64::from(RATE);
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        let onset = |seconds: f64, strength: f32| Onset {
            frame: (seconds * f64::from(RATE)) as u64,
            strength,
            percussiveness: 1.0,
            origin: Origin::Found,
        };
        warp.onsets = Some(Onsets::new([
            onset(3.0, 1.0),
            onset(5.0, 0.5),
            onset(6.5, 0.9),
            onset(7.9, 0.9),
        ]));
        // Out of reach before 4 s, then the strongest, the nearer to 6 s
        // of two, then the lattice, every 6 s, then the end.
        let cuts = cuts(&warp, 20 * second, RATE);
        assert_eq!(cuts, [0, 6 * second + second / 2, 12 * second, 20 * second]);
        // A file that fits in a segment isn't cut.
        assert_eq!(super::cuts(&warp, 8 * second, RATE), [0, 8 * second]);
    }

    #[test]
    fn a_file_at_its_own_speed_untransposed_plays_sample_for_sample() {
        let data = chord(RATE, 4.0);
        let tempo = song(120.0);
        let clip = stretched(0, 1.0, 6.0, 120.0);
        let plan = plan(&clip, &data, &tempo);
        assert!(plan.is_passthrough());
        assert!(plan.segments().is_empty());
        let mut rig = Rig::new(
            player(&clip, &data, &Renders::new(plan), &tempo),
            &tempo,
            RATE,
            512,
        );
        let [left, right] = rig.play(4 * 24_000);
        let beat = 24_000;
        assert!(left[..beat].iter().all(|&s| s == 0.0));
        assert_eq!(left[beat..], data.channel(0)[..3 * beat]);
        assert_eq!(right[beat..], data.channel(1)[..3 * beat]);

        // Transposed, it stretches.
        let mut transposed = clip.clone();
        warp_of(&mut transposed).transpose = 2;
        assert!(!super::tests::plan(&transposed, &data, &tempo).is_passthrough());
        // And at another tempo.
        assert!(!super::tests::plan(&clip, &data, &song(100.0)).is_passthrough());
    }

    #[test]
    fn segments_render_the_same_every_time() {
        let data = chord(RATE, 10.0);
        let tempo = song(120.0);
        let clip = stretched(0, 0.0, 16.0, 100.0);
        let plan = plan(&clip, &data, &tempo);
        // 10 s of file at 1.2 times its speed: two segments.
        assert_eq!(plan.segments().len(), 2);
        let render = |k: usize| render(&plan, &plan.segments()[k], &mut DataReader(data.clone()));
        for (k, segment) in plan.segments().iter().enumerate() {
            let (first, again) = (render(k).unwrap(), render(k).unwrap());
            assert_eq!(first, again, "segment {k}");
            assert_eq!(first.frames(), segment.frames() as u64);
            assert_eq!(first.channels(), 2);
        }
        let [a, b] = [&plan.segments()[0], &plan.segments()[1]];
        assert_eq!(a.end() - b.start(), plan.crossfade as i64, "they crossfade");
        assert_ne!(a.key(), b.key());
    }

    /// Where the energy of a burst in `signal` peaks, in frames.
    fn peak(signal: &[f32]) -> usize {
        let window = 96;
        (0..signal.len() - window)
            .max_by(|&a, &b| {
                level(&signal[a..a + window]).total_cmp(&level(&signal[b..b + window]))
            })
            .unwrap()
            + window / 2
    }

    #[test]
    fn a_stretched_file_plays_where_its_warp_says_at_its_own_pitch() {
        // A burst at 2 s and a 440 Hz tone after it, warped at 100 BPM.
        let burst = |t: f64| match t {
            _ if (2.0..2.005).contains(&t) => (TAU * 3000.0 * t).sin(),
            _ if t >= 3.0 => 0.5 * (TAU * 440.0 * t).sin(),
            _ => 0.0,
        };
        let data = file(RATE, 8.0, burst, burst);
        for bpm in [80.0, 125.0] {
            let tempo = song(bpm);
            let clip = stretched(0, 0.0, 16.0, 100.0);
            let renders = rendered(&clip, &data, &tempo);
            let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 512);
            let [left, right] = rig.play(RATE as usize * 6);
            // The burst lands where the song's tempo puts the middle of it,
            // the file's 2.0025 s, to within a couple of milliseconds.
            let expected = 2.0025 * 100.0 / bpm * f64::from(RATE);
            let landed = peak(&left[..(3.0 * 100.0 / bpm * f64::from(RATE)) as usize]);
            let off = (landed as f64 - expected).abs();
            assert!(
                off < 0.002 * f64::from(RATE),
                "{bpm} BPM: {landed} for {expected}"
            );
            // The tone keeps its pitch.
            let tone = (3.5 * 100.0 / bpm * f64::from(RATE)) as usize;
            let heard = frequency(&left[tone..tone + RATE as usize / 2], RATE);
            assert!((heard - 440.0).abs() < 3.0, "{bpm} BPM: {heard} Hz");
            // Both channels alike, but for the stretcher's rounding: it
            // keeps their phases together, and works them out in turn.
            assert!(left.iter().zip(&right).all(|(l, r)| (l - r).abs() < 1e-3));
        }
    }

    #[test]
    fn transposing_moves_the_pitch_and_not_the_time() {
        let data = file(RATE, 6.0, sine(440.0), sine(440.0));
        let tempo = song(120.0);
        for (semitones, formants) in [(5, 100.0), (-5, 100.0), (5, 0.0), (-5, 40.0)] {
            let mut clip = stretched(0, 0.0, 8.0, 100.0);
            warp_of(&mut clip).transpose = semitones;
            warp_of(&mut clip).spectral.formants = formants;
            let renders = rendered(&clip, &data, &tempo);
            let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 512);
            let [left, _] = rig.play(RATE as usize * 3);
            let heard = frequency(&left[RATE as usize..2 * RATE as usize], RATE);
            let expected = 440.0 * pitch_ratio(f64::from(semitones), 0.0);
            assert!(
                (heard - expected).abs() < 4.0,
                "{semitones}: {heard} Hz for {expected}"
            );
        }
    }

    #[test]
    fn formants_stay_in_place_as_it_transposes_or_move_with_the_pitch() {
        // A voice on 150 Hz, its harmonics shaped by a formant at 1 kHz.
        let voice = |t: f64| {
            let harmonic = |k: u32| {
                let f = 150.0 * f64::from(k);
                let shape = 1.0 / (1.0 + ((f - 1000.0) / 150.0).powi(2));
                0.05 * shape * (TAU * f * t).sin()
            };
            (1..=26).map(harmonic).sum()
        };
        let data = file(RATE, 4.0, voice, voice);
        let tempo = song(120.0);
        // Up five semitones, onto 200 Hz: its fifth harmonic lands on the
        // formant where it stays, and its seventh on the formant moved up
        // with the pitch.
        let ratio = pitch_ratio(5.0, 0.0);
        let (stays, moves) = (5.0 * 150.0 * ratio, 7.0 * 150.0 * ratio);
        for (formants, louder, softer) in [(100.0, stays, moves), (0.0, moves, stays)] {
            let mut clip = stretched(0, 0.0, 6.0, 120.0);
            warp_of(&mut clip).transpose = 5;
            warp_of(&mut clip).spectral.formants = formants;
            let renders = rendered(&clip, &data, &tempo);
            let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 512);
            let [left, _] = rig.play(RATE as usize * 2);
            let middle = &left[RATE as usize / 2..3 * RATE as usize / 2];
            let (louder, softer) = (
                strength(middle, louder, RATE),
                strength(middle, softer, RATE),
            );
            assert!(
                louder > 2.0 * softer,
                "{formants}%: {louder} against {softer}"
            );
        }
    }

    #[test]
    fn a_transpose_curve_moves_the_pitch_as_it_plays() {
        let data = file(RATE, 6.0, sine(440.0), sine(440.0));
        let tempo = song(120.0);
        let mut clip = stretched(0, 0.0, 8.0, 120.0);
        let point = |beat: i64, value: f64| Point::new(Beats::new(beat), value);
        // Up an octave from beat 4: in 4 beats at 120 BPM, 2 seconds.
        let curve = vec![point(0, 0.0), point(4, 0.0), point(4, 12.0)];
        audio_of(&mut clip).set_lane(ClipParam::Transpose, curve);
        let renders = rendered(&clip, &data, &tempo);
        let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 256);
        let [left, _] = rig.play(RATE as usize * 4);
        let second = RATE as usize;
        assert!((frequency(&left[second / 2..3 * second / 2], RATE) - 440.0).abs() < 3.0);
        assert!((frequency(&left[5 * second / 2..7 * second / 2], RATE) - 880.0).abs() < 4.0);
    }

    #[test]
    fn rendered_segments_play_the_same_however_the_blocks_fall() {
        let tempo = song(120.0);
        for rate in [RATE, 44_100] {
            let data = chord(rate, 12.0);
            let mut clip = stretched(0, 0.0, 24.0, 100.0);
            audio_of(&mut clip).gain = 0.5;
            let renders = rendered(&clip, &data, &tempo);
            assert!(renders.plan.segments().len() >= 2);
            let play = |block: usize, from: u64, frames: usize| {
                let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, block);
                rig.locate(from);
                rig.play(frames)
            };
            let whole = play(512, 0, 12 * RATE as usize);
            assert!(level(&whole[0]) > 0.05);
            assert_eq!(play(64, 0, 12 * RATE as usize), whole, "{rate} Hz");
            assert_eq!(play(4096, 0, 12 * RATE as usize), whole, "{rate} Hz");
            // From anywhere in it.
            let from = 5 * RATE as usize + 123;
            let part = play(1000, from as u64, RATE as usize);
            assert_eq!(part[0], whole[0][from..from + RATE as usize], "{rate} Hz");
        }
    }

    #[test]
    fn segments_crossfade_to_one() {
        let data = chord(RATE, 12.0);
        let tempo = song(120.0);
        let clip = stretched(0, 0.0, 20.0, 100.0);
        let renders = Renders::new(plan(&clip, &data, &tempo));
        // Segments of ones, so what plays is how much of each plays.
        for (segment, slot) in renders.plan.segments().iter().zip(renders.slots.iter()) {
            slot.fill(AudioData::new(RATE, vec![vec![1.0; segment.frames()]; 2]));
        }
        let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 333);
        let [left, _] = rig.play(10 * RATE as usize);
        for (frame, &sample) in left.iter().enumerate() {
            assert!((sample - 1.0).abs() < 1e-6, "frame {frame}: {sample}");
        }
    }

    #[test]
    fn until_its_segments_render_it_plays_live_and_hands_over_at_a_cut() {
        let data = chord(RATE, 12.0);
        let tempo = song(120.0);
        let clip = stretched(0, 0.0, 24.0, 100.0);
        let done = rendered(&clip, &data, &tempo);
        let reference = Rig::new(player(&clip, &data, &done, &tempo), &tempo, RATE, 512)
            .play(10 * RATE as usize);
        let pending = Renders::new(plan(&clip, &data, &tempo));
        let mut rig = Rig::new(player(&clip, &data, &pending, &tempo), &tempo, RATE, 512);
        // Live from the start: the chord, at its own pitch.
        let [early, _] = rig.play(RATE as usize);
        assert!(level(&early[RATE as usize / 4..]) > 0.1);
        assert!(steepest(&early) < 0.1, "no clicks");
        // The segments land halfway through the first.
        for (slot, segment) in pending.slots.iter().zip(done.slots.iter()) {
            slot.fill(segment.get().unwrap().clone());
        }
        let [late, _] = rig.play(9 * RATE as usize);
        let heard = [early, late].concat();
        let cut = done.plan.segments()[0].end() as usize;
        let crossfade = done.plan.crossfade;
        // Live up to the first cut, crossfading into the rendered audio
        // just before it.
        let before = cut - 1000..cut - crossfade;
        assert_ne!(heard[before.clone()], reference[0][before]);
        assert_eq!(heard[cut..], reference[0][cut..10 * RATE as usize]);
        assert!(
            steepest(&heard[cut - 2 * crossfade..cut + crossfade]) < 0.1,
            "no click at the cut"
        );
    }

    #[test]
    fn copies_at_the_same_tempo_render_the_same_segments() {
        let data = chord(RATE, 20.0);
        let tempo = song(120.0);
        let keys = |start: f64| {
            let clip = stretched(1, start, 32.0, 100.0);
            let plan = plan(&clip, &data, &tempo);
            let keys: Vec<u64> = plan.segments().iter().map(SegmentPlan::key).collect();
            let starts: Vec<i64> = plan.segments().iter().map(SegmentPlan::start).collect();
            (keys, starts)
        };
        let (here, at) = keys(0.0);
        // A bar later, a whole number of frames on.
        let (later, moved) = keys(4.0);
        assert_eq!(here, later);
        assert!(at.iter().zip(&moved).all(|(a, b)| b - a == 4 * 24_000));
        // At another tempo, they render again.
        let clip = stretched(1, 0.0, 32.0, 100.0);
        let other = plan(&clip, &data, &song(121.0));
        assert!(other.segments().iter().all(|s| !here.contains(&s.key())));
    }

    #[test]
    fn a_new_graph_plays_on_where_the_old_one_was() {
        let data = chord(RATE, 12.0);
        let tempo = song(120.0);
        let clip = stretched(0, 0.0, 24.0, 100.0);
        let renders = rendered(&clip, &data, &tempo);
        let new = || player(&clip, &data, &renders, &tempo);
        let reference = Rig::new(new(), &tempo, RATE, 512).play(6 * RATE as usize);
        // The same plan in a new graph: nothing changes.
        let mut rig = Rig::new(new(), &tempo, RATE, 512);
        let [first, _] = rig.play(2 * RATE as usize);
        rig.swap(new());
        let [rest, _] = rig.play(4 * RATE as usize);
        assert_eq!([first, rest].concat(), reference[0]);

        // Transposed, with nothing rendered yet: the rendered audio fades
        // out into the live stretch.
        let mut transposed = clip.clone();
        warp_of(&mut transposed).transpose = 3;
        let pending = Renders::new(plan(&transposed, &data, &tempo));
        let swapped = || player(&transposed, &data, &pending, &tempo);
        let mut rig = Rig::new(new(), &tempo, RATE, 512);
        let swap = 2 * RATE as usize;
        rig.play(swap);
        rig.swap(swapped());
        let [after, _] = rig.play(RATE as usize);
        // What the transposed clip plays live from there, on its own.
        let mut live = Rig::new(swapped(), &tempo, RATE, 512);
        live.locate(swap as u64);
        let [alone, _] = live.play(RATE as usize);
        let crossfade = renders.plan.crossfade;
        for i in 0..2 * crossfade {
            let rising = rise(i as i64, crossfade);
            let expected = reference[0][swap + i] * (1.0 - rising) + alone[i] * rising;
            assert!(
                (after[i] - expected).abs() < 1e-5,
                "frame {i}: {} for {expected}",
                after[i]
            );
        }
        assert_eq!(after[2 * crossfade..], alone[2 * crossfade..]);
    }

    #[test]
    fn a_tempo_change_plays_on_live_without_a_break() {
        let data = chord(RATE, 20.0);
        let tempo = song(120.0);
        let clip = stretched(0, 0.0, 40.0, 100.0);
        let renders = rendered(&clip, &data, &tempo);
        let mut rig = Rig::new(player(&clip, &data, &renders, &tempo), &tempo, RATE, 441);
        let [before, _] = rig.play(2 * RATE as usize);
        // Slower, then a little faster, while it plays.
        rig.set_tempo(song(110.0));
        let [slower, _] = rig.play(RATE as usize);
        rig.set_tempo(song(114.0));
        let [faster, _] = rig.play(RATE as usize);
        let heard = [&before[..], &slower, &faster].concat();
        // The chord's own level dips to about 0.09 over 10 ms: a dropout is
        // silence.
        for window in heard[RATE as usize..].chunks(480) {
            assert!(level(window) > 0.05, "no dropout");
        }
        assert!(steepest(&heard[RATE as usize..]) < 0.1, "no clicks");
        // The new graph's plan, once rendered, takes over at a cut.
        let tempo = rig.tempo.clone();
        let renders = rendered(&clip, &data, &tempo);
        rig.swap(player(&clip, &data, &renders, &tempo));
        let [after, _] = rig.play(8 * RATE as usize);
        for window in after.chunks(480) {
            assert!(level(window) > 0.05, "no dropout");
        }
        assert!(steepest(&after) < 0.1, "no clicks");
    }
}
