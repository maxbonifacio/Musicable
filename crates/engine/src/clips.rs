//! Clip players: the nodes at the head of audio and MIDI tracks' channel
//! strips, playing the tracks' clips from the song position like everything
//! else, so a position always renders the same, however the blocks fall and
//! however often the graph is rebuilt.

use std::collections::BTreeMap;
use std::f64::consts::FRAC_PI_2;

use musicable_model::automation::Point;
use musicable_model::clip::{self, Clip, ClipContent, ClipId, Controller, Dimension, Offset};
use musicable_model::param::{ClipParam, Scale};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::warp::Segment;

use crate::media::{CHUNK, Source};
use crate::resample::Resampler;
use crate::{AudioMut, EngineConfig, Inputs, Node, NoteEvent, NoteId, Notes, ProcessContext};

/// How many times a warped clip's resampler widens its sinc at most, for a
/// file that plays faster than the device's rate: up to three octaves up,
/// its cutoff falls with the speed, and beyond, what's above the cutoff
/// folds back.
const WIDEST: f64 = 8.0;

/// How close to the device's rate a warped clip's file has to play to play
/// sample for sample: within a frame over a billion of them, six hours at
/// 48 kHz.
const SAME_SPEED: f64 = 1e-9;

/// Plays a track's audio clips, added to what comes into it, such as the
/// track's test tone.
///
/// An unwarped clip plays its file at the file's own speed, from the frame
/// it starts at in the file, for as long as the clip lasts. A warped clip
/// follows its warp (see [`warp`](musicable_model::warp)): the song's beats
/// move its own time, and its markers say which frame of the file plays at
/// each beat of that, so the file speeds up and slows down with the tempo.
/// In Re-Pitch, the file is resampled at the speed it plays at, so its pitch
/// follows, as on a turntable. A file that plays at the device's sample
/// rate plays sample for sample, and anything else is resampled (see
/// `resample.rs`). Mono files play in both channels, and files with more
/// than two play their first two.
///
/// A clip's gain follows its Gain curve, in its own time, where it has
/// one. Fades are equal-power, so where one clip fades out over the same
/// frames another fades in, they crossfade at constant power. A clip
/// without a source, such as one whose file is missing, is silent.
///
/// Clips that stream tell their [`Stream`](crate::media::Stream) where
/// they'll read next every block, playing or not, and where they'll read
/// after the loop jumps back, and again before each read of a block too
/// long for the stream to hold at once, as an offline render's can be.
#[derive(Debug, Default)]
pub struct AudioClips {
    /// In order of start, then id.
    clips: Vec<Placed>,
    /// The latest end of each clip and the clips before it, for finding the
    /// clips a block overlaps.
    reach: Vec<Beats>,
    /// The clips that stream.
    streams: Vec<usize>,
    /// The frames of the file one read takes in, at most.
    room: usize,
    /// One read's frames, a channel after another: `room` for each of two.
    input: Vec<f32>,
    /// The resampler's weights for one frame.
    weights: Vec<f32>,
    /// The gain on each frame of one read, where it moves.
    gains: Vec<f32>,
}

#[derive(Debug)]
struct Placed {
    id: ClipId,
    start: Beats,
    end: Beats,
    /// Where the fade in ends and where the fade out starts.
    fade_in: Beats,
    fade_out: Beats,
    gain: f32,
    /// The clip's gain where it has a curve for it.
    gain_curve: Option<Curve>,
    time: Time,
    source: Option<Source>,
    /// For an unwarped file that isn't at the device's rate, once prepared.
    resampler: Option<Resampler>,
    /// Where the clip reads after the loop jumps back, as hinted for this
    /// block.
    after_loop: Option<u64>,
}

/// How a clip's file follows the song.
#[derive(Debug)]
enum Time {
    /// At its own speed, from frame `offset` at the clip's start.
    Unwarped { offset: u64 },
    /// Warped: the clip's own time is the song's beats plus `shift`, and
    /// each segment plays at its own speed, from its marker. A clip
    /// without a source has none.
    Warped {
        shift: Beats,
        segments: Vec<Segment>,
    },
}

/// A run of song frames over which a clip reads its file at one speed: all
/// of an unwarped clip, or where one of a warped clip's segments plays at
/// one of the song's tempos.
#[derive(Debug, Clone, Copy)]
struct Piece {
    /// The song frames it covers, from `from` up to `to`. Where it starts
    /// places its frames, however the blocks fall.
    from: i64,
    to: i64,
    /// The file's position on `from`, fractional, and how far it moves on
    /// each song frame.
    at: f64,
    step: f64,
    /// The clip's own time on `from`, and how far it moves on each song
    /// frame, for its curves.
    own: f64,
    own_step: f64,
}

impl Piece {
    /// The file's position on song frame `frame`.
    fn position(&self, frame: i64) -> f64 {
        self.at + (frame - self.from) as f64 * self.step
    }

    /// The clip's own time on song frame `frame`.
    fn own_time(&self, frame: i64) -> f64 {
        self.own + (frame - self.from) as f64 * self.own_step
    }
}

/// How a clip reads its file over a piece.
#[derive(Debug, Clone, Copy)]
enum Reading {
    /// Sample for sample: song frame `n` plays the file's frame `n + by`.
    Copy {
        by: i64,
    },
    Resample(Resampler),
}

impl Placed {
    /// Calls `each` with the pieces that cover the song's frames from
    /// `from` up to `to`, in order, with `tempo` placing the beats at
    /// `rate`. They can start before `from` and end after `to`.
    fn pieces(&self, tempo: &TempoMap, rate: u32, from: i64, to: i64, mut each: impl FnMut(Piece)) {
        let (shift, segments) = match &self.time {
            Time::Unwarped { offset } => {
                // Its own time is the file's seconds.
                let file_rate = f64::from(self.source.as_ref().map_or(rate, Source::sample_rate));
                let step = self.resampler.map_or(1.0, |r| r.step());
                let at = *offset as f64;
                each(Piece {
                    from: tempo.frame_at(self.start, rate),
                    to: i64::MAX,
                    at,
                    step,
                    own: at / file_rate,
                    own_step: step / file_rate,
                });
                return;
            }
            Time::Warped { shift, segments } if !segments.is_empty() => (*shift, segments),
            Time::Warped { .. } => return,
        };
        // The first song frame on or after `at`: frames from there on
        // count as after it, as `TempoMap::beats_at` counts them.
        let after = |at: Beats| tempo.sample_at(at, rate).ceil() as i64;
        let tempos = tempo.tempos();
        let mut t = tempos
            .partition_point(|c| after(c.at) <= from)
            .saturating_sub(1);
        let mut s = segments[1..].partition_point(|g| after(g.beat - shift) <= from);
        let mut frame = from;
        while frame < to {
            let (change, segment) = (tempos[t], segments[s]);
            let tempo_to = tempos.get(t + 1).map_or(i64::MAX, |c| after(c.at));
            let segment_to = segments
                .get(s + 1)
                .map_or(i64::MAX, |g| after(g.beat - shift));
            // The first segment plays the file before its marker too.
            let segment_from = if s == 0 {
                i64::MIN
            } else {
                after(segment.beat - shift)
            };
            let piece_from = after(change.at).max(segment_from);
            let piece_to = tempo_to.min(segment_to);
            if piece_from < piece_to {
                let beats_per_frame = change.bpm / (60.0 * f64::from(rate));
                let since = piece_from as f64 - tempo.sample_at(change.at, rate);
                let own = change.at + shift;
                let into = (own - segment.beat).to_f64() + since * beats_per_frame;
                each(Piece {
                    from: piece_from,
                    to: piece_to,
                    at: segment.frame as f64 + into * segment.frames_per_beat,
                    step: beats_per_frame * segment.frames_per_beat,
                    own: own.to_f64() + since * beats_per_frame,
                    own_step: beats_per_frame,
                });
            }
            frame = piece_to;
            if piece_to == tempo_to {
                t += 1;
            }
            if piece_to == segment_to {
                s += 1;
            }
        }
    }

    /// How the clip reads its file over `piece`.
    fn reading(&self, piece: &Piece) -> Reading {
        let copy = Reading::Copy {
            by: piece.at.round() as i64 - piece.from,
        };
        match (&self.time, self.resampler) {
            (Time::Unwarped { .. }, None) => copy,
            (Time::Unwarped { .. }, Some(resampler)) => Reading::Resample(resampler),
            (Time::Warped { .. }, _) if (piece.step - 1.0).abs() < SAME_SPEED => copy,
            (Time::Warped { .. }, _) => {
                Reading::Resample(Resampler::with_step_widened_at_most(piece.step, WIDEST))
            }
        }
    }

    /// Where the clip's first read for song frame `frame` starts in its
    /// file.
    fn read_start(&self, tempo: &TempoMap, rate: u32, frame: i64) -> i64 {
        let mut start = 0;
        self.pieces(tempo, rate, frame, frame + 1, |piece| {
            start = match self.reading(&piece) {
                Reading::Copy { by } => frame + by,
                Reading::Resample(r) => piece.position(frame).floor() as i64 - r.reach() as i64 + 1,
            };
        });
        start
    }

    /// The clip's gain on song frame `frame` of `piece`, before its fades.
    fn gain_at(&self, piece: &Piece, frame: i64) -> f32 {
        match &self.gain_curve {
            Some(curve) => curve.value_at(piece.own_time(frame)) as f32,
            None => self.gain,
        }
    }
}

/// One of a clip's curves, in its own time.
#[derive(Debug)]
struct Curve {
    /// At least one, in order: where each falls in the clip's own time,
    /// and its value.
    points: Vec<(f64, f64)>,
    scale: Scale,
}

impl Curve {
    fn new(points: &[Point], scale: Scale) -> Option<Self> {
        (!points.is_empty()).then(|| Self {
            points: points.iter().map(|p| (p.at.to_f64(), p.value)).collect(),
            scale,
        })
    }

    /// The value at `at` in the clip's own time, moving evenly in the scale
    /// between points, as the model's
    /// [`ClipLane::value_at`](musicable_model::clip::ClipLane::value_at)
    /// works it out: at a step, the value after it.
    fn value_at(&self, at: f64) -> f64 {
        let after = self.points.partition_point(|p| p.0 <= at);
        match (after.checked_sub(1), self.points.get(after)) {
            (Some(before), Some(&(to, next))) => {
                let (from, value) = self.points[before];
                self.scale.mix(value, next, (at - from) / (to - from))
            }
            (Some(before), None) => self.points[before].1,
            (None, Some(&(_, next))) => next,
            (None, None) => unreachable!("a curve has a point"),
        }
    }
}

impl AudioClips {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `clip`, playing `source`. MIDI clips are ignored.
    pub fn add(&mut self, clip: &Clip, source: Option<Source>) {
        let ClipContent::Audio(audio) = &clip.content else {
            return;
        };
        let time = match (&audio.warp, audio.offset) {
            (Some(warp), Offset::Beats(offset)) => Time::Warped {
                shift: offset - clip.start,
                segments: source
                    .as_ref()
                    .map(|s| warp.segments(s.sample_rate()).collect())
                    .unwrap_or_default(),
            },
            (_, offset) => Time::Unwarped {
                offset: offset.frames().unwrap_or(0),
            },
        };
        let gain = ClipParam::Gain;
        let gain_curve = audio
            .lane(gain)
            .and_then(|lane| Curve::new(&lane.points, gain.spec().scale));
        self.clips.push(Placed {
            id: clip.id,
            start: clip.start,
            end: clip.end(),
            fade_in: clip.start + audio.fade_in,
            fade_out: clip.end() - audio.fade_out,
            gain: audio.gain,
            gain_curve,
            time,
            source,
            resampler: None,
            after_loop: None,
        });
    }

    pub fn with_clip(mut self, clip: &Clip, source: Option<Source>) -> Self {
        self.add(clip, source);
        self
    }

    /// Tells each stream where its clip reads next, from where the block
    /// starts.
    fn hint(&mut self, ctx: &ProcessContext<'_>) {
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as i64;
        let looping = ctx.transport.looping.map(|region| {
            let start = tempo.frame_at(region.start, rate);
            (start, tempo.frame_at(region.end, rate))
        });
        for &i in &self.streams {
            let clip = &mut self.clips[i];
            let Some(Source::Stream(stream)) = &clip.source else {
                continue;
            };
            let (start, end) = (
                tempo.frame_at(clip.start, rate),
                tempo.frame_at(clip.end, rate),
            );
            let read_at = |frame: i64| clip.read_start(tempo, rate, frame).max(0) as u64;
            let next = match first {
                f if start <= f && f < end => read_at(f),
                _ => read_at(start),
            };
            let after_loop = looping
                .filter(|&(from, to)| from < to && from < end && start < to)
                .map(|(from, _)| read_at(from.max(start)));
            stream.hint(next, after_loop);
            clip.after_loop = after_loop;
        }
    }

    /// Adds clip `i`'s frames in this block to `output`.
    fn play(&mut self, i: usize, ctx: &ProcessContext<'_>, output: &mut AudioMut<'_>) {
        let clip = &self.clips[i];
        let Some(source) = &clip.source else {
            return;
        };
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as i64;
        let (start, end) = (
            tempo.frame_at(clip.start, rate),
            tempo.frame_at(clip.end, rate),
        );
        let from = first.max(start);
        let to = (first + ctx.frames as i64).min(end);
        if from >= to {
            return;
        }
        let fades = Fades {
            fade_in: (start, tempo.frame_at(clip.fade_in, rate)),
            fade_out: (tempo.frame_at(clip.fade_out, rate), end),
        };
        let mut reader = Reader {
            source,
            channels: source.channels().min(2),
            after_loop: clip.after_loop,
            room: self.room,
            input: &mut self.input,
        };
        let (room, channels) = (reader.room, reader.channels);
        let (weights, gains) = (&mut self.weights, &mut self.gains);
        clip.pieces(tempo, rate, from, to, |piece| {
            let (from, to) = (from.max(piece.from), to.min(piece.to));
            if from >= to {
                return;
            }
            // The clip's gain on each frame, with its fades.
            let gain = |frame: i64| clip.gain_at(&piece, frame) * fades.at(frame);
            let steady = (clip.gain_curve.is_none() && fades.flat(from, to)).then_some(clip.gain);
            match clip.reading(&piece) {
                Reading::Copy { by } => {
                    let mut frame = from;
                    while frame < to {
                        let frames = ((to - frame) as usize).min(room);
                        let input = reader.read(frame + by, frames);
                        let gains = &mut gains[..frames];
                        if steady.is_none() {
                            for (k, g) in gains.iter_mut().enumerate() {
                                *g = gain(frame + k as i64);
                            }
                        }
                        let at = (frame - first) as usize;
                        for c in 0..output.channels() {
                            let input = &input[c.min(channels - 1) * frames..][..frames];
                            let out = &mut output.channel_mut(c)[at..at + frames];
                            match steady {
                                Some(g) => {
                                    out.iter_mut().zip(input).for_each(|(o, &x)| *o += x * g)
                                }
                                None => {
                                    let samples = out.iter_mut().zip(input).zip(gains.iter());
                                    samples.for_each(|((o, &x), &g)| *o += x * g);
                                }
                            }
                        }
                        frame += frames as i64;
                    }
                }
                Reading::Resample(resampler) => {
                    let reach = resampler.reach() as i64;
                    let weights = &mut weights[..2 * reach as usize];
                    // The frames of the file that `count` frames from `frame`
                    // read, from the first.
                    let span = |frame: i64, count: i64, first_in: i64| {
                        (piece.position(frame + count - 1).floor() as i64 + reach - first_in + 1)
                            as usize
                    };
                    let mut frame = from;
                    while frame < to {
                        let first_in = piece.position(frame).floor() as i64 - reach + 1;
                        // As many frames as the input has room for.
                        let fit = (room as f64 - 2.0 * reach as f64 - 2.0) / piece.step;
                        let mut count = (fit.floor() as i64 + 1).clamp(1, to - frame);
                        while count > 1 && span(frame, count, first_in) > room {
                            count -= 1;
                        }
                        let span = span(frame, count, first_in);
                        let input = reader.read(first_in, span);
                        for n in frame..frame + count {
                            let x = piece.position(n);
                            let base = (resampler.weights(x, weights) - first_in) as usize;
                            let gain = steady.unwrap_or_else(|| gain(n));
                            let mut sums = [0.0f32; 2];
                            for (c, sum) in sums.iter_mut().enumerate().take(channels) {
                                let input = &input[c * span + base..][..weights.len()];
                                *sum = input.iter().zip(weights.iter()).map(|(x, w)| x * w).sum();
                            }
                            let at = (n - first) as usize;
                            for c in 0..output.channels() {
                                output.channel_mut(c)[at] += sums[c.min(channels - 1)] * gain;
                            }
                        }
                        frame += count;
                    }
                }
            }
        });
    }
}

/// Reads a clip's file a run of frames at a time.
struct Reader<'a> {
    source: &'a Source,
    /// The channels it reads: one or two.
    channels: usize,
    after_loop: Option<u64>,
    room: usize,
    input: &'a mut [f32],
}

impl Reader<'_> {
    /// The file's `frames` frames from `start`, a channel after another: at
    /// most `room`. A stream is told first that the clip reads there, so
    /// one that waits for the disk loads them, however far into a long
    /// block they are.
    fn read(&mut self, start: i64, frames: usize) -> &[f32] {
        debug_assert!(frames <= self.room, "a read fits");
        if let Source::Stream(stream) = self.source {
            stream.hint(start.max(0) as u64, self.after_loop);
        }
        let input = &mut self.input[..self.channels * frames];
        read(self.source, start, input, self.channels);
        input
    }
}

/// Reads `channels` channels of `source` from file frame `start` into
/// `out`, a channel after another, as many frames as fit. Frames before the
/// file starts or after it ends are silent.
pub(crate) fn read(source: &Source, start: i64, out: &mut [f32], channels: usize) {
    let frames = out.len() / channels;
    // Frames before the file.
    let skip = (-start).clamp(0, frames as i64) as usize;
    let at = start.max(0) as u64;
    let (left, right) = out.split_at_mut(frames);
    let mut parts: [&mut [f32]; 2] = [left, right];
    let parts = &mut parts[..channels];
    for part in parts.iter_mut() {
        part[..skip].fill(0.0);
    }
    match source {
        Source::Whole(data) => {
            for (c, part) in parts.iter_mut().enumerate() {
                let samples = data.channel(c);
                let available = &samples[(at as usize).min(samples.len())..];
                let wanted = &mut part[skip..];
                let n = available.len().min(wanted.len());
                wanted[..n].copy_from_slice(&available[..n]);
                wanted[n..].fill(0.0);
            }
        }
        Source::Stream(stream) => match parts {
            [left] => {
                stream.read(at, &mut [&mut left[skip..]]);
            }
            [left, right] => {
                stream.read(at, &mut [&mut left[skip..], &mut right[skip..]]);
            }
            _ => unreachable!("one or two channels"),
        },
    }
}

/// A clip's fades, in song frames.
#[derive(Debug, Clone, Copy)]
struct Fades {
    /// The fade in's first frame, and the frame after its last.
    fade_in: (i64, i64),
    /// The same for the fade out.
    fade_out: (i64, i64),
}

impl Fades {
    /// How far the fades let the clip through on `frame`: a quarter sine
    /// from silence up to full over the fade in, and down over the fade
    /// out.
    fn at(&self, frame: i64) -> f32 {
        let mut level = 1.0;
        let (start, end) = self.fade_in;
        if frame < end && start < end {
            level *= (FRAC_PI_2 * (frame - start) as f64 / (end - start) as f64).sin();
        }
        let (start, end) = self.fade_out;
        if frame >= start && start < end {
            level *= (FRAC_PI_2 * (end - frame) as f64 / (end - start) as f64).sin();
        }
        level as f32
    }

    /// Whether no fade touches the frames from `from` to `to`.
    fn flat(&self, from: i64, to: i64) -> bool {
        let fading_in = from < self.fade_in.1 && self.fade_in.0 < self.fade_in.1;
        let fading_out = to > self.fade_out.0 && self.fade_out.0 < self.fade_out.1;
        !fading_in && !fading_out
    }
}

impl Node for AudioClips {
    fn prepare(&mut self, config: &EngineConfig) {
        self.clips.sort_by_key(|c| (c.start, c.id));
        self.reach = self
            .clips
            .iter()
            .scan(None, |latest: &mut Option<Beats>, c| {
                let end = latest.map_or(c.end, |l| l.max(c.end));
                *latest = Some(end);
                Some(end)
            })
            .collect();
        self.streams = (0..self.clips.len())
            .filter(|&i| matches!(self.clips[i].source, Some(Source::Stream(_))))
            .collect();
        let mut reach = 0;
        for clip in &mut self.clips {
            let rate = clip.source.as_ref().map(Source::sample_rate);
            clip.resampler = match &clip.time {
                Time::Unwarped { .. } => rate
                    .filter(|&rate| rate != config.sample_rate)
                    .map(|rate| Resampler::new(rate, config.sample_rate)),
                Time::Warped { .. } => None,
            };
            reach = reach.max(match (&clip.time, &clip.resampler) {
                (_, Some(r)) => r.reach(),
                (Time::Warped { segments, .. }, None) if !segments.is_empty() => {
                    // Its resamplers are made as it plays: the kernel they
                    // share is made now, off the audio thread.
                    let _ = Resampler::with_step(1.0);
                    Resampler::most_reach(WIDEST)
                }
                _ => 0,
            });
        }
        // Room for a block's reads at up to twice the device's rate, but no
        // more than a stream holds at once.
        let room = (2 * config.max_block + 2 * reach + 2).min(CHUNK as usize);
        self.room = room.max(2 * reach + 3);
        self.input = vec![0.0; 2 * self.room];
        self.weights = vec![0.0; 2 * reach];
        self.gains = vec![0.0; self.room];
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        if self.clips.is_empty() {
            return;
        }
        self.hint(ctx);
        if !ctx.transport.playing {
            return;
        }
        // Every clip that can have a frame in the block, with a frame to
        // spare on each side, then the ones that do.
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as f64;
        let from = Beats::from_f64(tempo.beats_at(first - 1.0, rate));
        let to = Beats::from_f64(tempo.beats_at(first + ctx.frames as f64, rate));
        let after = self.reach.partition_point(|&end| end <= from);
        let before = self
            .clips
            .partition_point(|c| c.start <= to + Beats::from_ticks(1));
        for i in after..before {
            self.play(i, ctx, output);
        }
    }
}

/// The most notes a MIDI clip player keeps track of at once. It doesn't
/// start notes beyond them.
const MAX_SOUNDING: usize = 1024;

/// Plays a track's MIDI clips: sends their notes, with their expression,
/// and their controllers' moves to the track's instrument
/// ([`Graph::send_notes`](crate::Graph::send_notes)). Audio coming into it
/// passes through.
///
/// A note plays from its start until it ends or its clip does. Notes that
/// started before the playhead reached them don't play, so a locate or a
/// loop that lands in the middle of a note leaves it out. When the playhead
/// jumps or the transport stops, the notes sounding are let go, and when a
/// recompiled player takes over, so are the ones it doesn't have sounding
/// as they are: a note deleted, moved, transposed, or shortened to end
/// before the playhead stops, and a note made longer plays on.
///
/// Each note's id is its clip's and its own ([`note_id`]), so a note keeps
/// its id however the notes around it are edited.
///
/// A controller moves as its clip's curve does, from wherever playback
/// starts or jumps to: the player chases it, sending the value its last
/// move before there left it at, from any clip, as playing the song from
/// its start would. Controllers keep their values when the transport stops
/// or a clip ends, as MIDI's do, but a switch held on, such as the sustain
/// pedal, is let go then, so no notes are left ringing. When a recompiled
/// player takes over, it sends whatever its curves hold differently from
/// what the last one sent.
#[derive(Debug)]
pub struct MidiClips {
    /// What the clips play, in order of when.
    events: Vec<Scheduled>,
    /// The notes that play, by id.
    spans: Vec<Span>,
    /// Each controller's moves, in order of when, for chasing.
    curves: BTreeMap<Controller, Vec<Move>>,
    /// The notes sounding now. Capacity for [`MAX_SOUNDING`].
    sounding: Vec<Sounding>,
    /// What each controller was last sent, by number.
    sent: [Option<f32>; CONTROLLERS],
    /// A recompiled player took over: it sends what changed at the next
    /// block it plays.
    rechase: bool,
    /// The next event to play, as of the last block.
    cursor: Option<usize>,
    /// The transport's jumps and whether it was playing, as of the last
    /// block.
    last: Option<(u64, bool)>,
    /// Where the last block ended, in beats.
    played_to: Beats,
}

/// How many controllers there are.
const CONTROLLERS: usize = Controller::MAX as usize + 1;

#[derive(Debug, Clone, Copy)]
struct Scheduled {
    at: Beats,
    /// Where it comes among events at the same time.
    rank: Rank,
    play: Play,
}

#[derive(Debug, Clone, Copy)]
enum Play {
    Note {
        event: NoteEvent,
        /// For a note's start: its release velocity, in case it's cut
        /// short.
        release: f32,
    },
    Controller {
        controller: Controller,
        value: f32,
    },
}

/// Where an event comes among others at the same time: switches let go
/// where a clip ends before the next clip's controllers move, controllers
/// move before notes, notes end before others start, and a note's
/// expression follows its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    LetGo,
    Move,
    Off,
    On,
    Expression,
}

/// A controller's move, for chasing.
#[derive(Debug, Clone, Copy)]
struct Move {
    at: Beats,
    rank: Rank,
    value: f32,
}

/// Where a note plays, and on which key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Span {
    note: NoteId,
    on: Beats,
    key: u8,
    off: Beats,
}

#[derive(Debug, Clone, Copy)]
struct Sounding {
    note: NoteId,
    /// When it started, and on which key.
    on: Beats,
    key: u8,
    release: f32,
    /// A recompiled player no longer has it sounding: it stops at the start
    /// of the next block.
    cut: bool,
}

impl Default for MidiClips {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiClips {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            spans: Vec::new(),
            curves: BTreeMap::new(),
            sounding: Vec::with_capacity(MAX_SOUNDING),
            sent: [None; CONTROLLERS],
            rechase: false,
            cursor: None,
            last: None,
            played_to: Beats::ZERO,
        }
    }

    /// Adds `clip`'s notes and controllers. Audio clips are ignored.
    pub fn add(&mut self, clip: &Clip) {
        let ClipContent::Midi(midi) = &clip.content else {
            return;
        };
        let window = midi.offset..midi.offset + clip.length;
        for note in &midi.notes {
            // Notes are always longer than nothing, but one that weren't
            // would stop before it started, and sound on.
            if !window.contains(&note.start) || note.length <= Beats::ZERO {
                continue;
            }
            let id = note_id(clip.id, note.id);
            let on = clip.start + (note.start - midi.offset);
            let off = (on + note.length).min(clip.end());
            let schedule = |at, event: NoteEvent| Scheduled {
                at,
                rank: rank(&event),
                play: Play::Note {
                    event,
                    release: note.release_velocity,
                },
            };
            self.events.push(schedule(
                on,
                NoteEvent::On {
                    note: id,
                    key: note.key,
                    velocity: note.velocity,
                },
            ));
            for dimension in Dimension::ALL {
                for point in note.expression.curve(dimension) {
                    let at = on + point.at;
                    if at < off {
                        let value = point.value;
                        let change = NoteEvent::Expression {
                            note: id,
                            dimension,
                            value,
                        };
                        self.events.push(schedule(at, change));
                    }
                }
            }
            let velocity = note.release_velocity;
            self.events
                .push(schedule(off, NoteEvent::Off { note: id, velocity }));
            let key = note.key;
            self.spans.push(Span {
                note: id,
                on,
                key,
                off,
            });
        }
        for (&controller, points) in &midi.controllers {
            let mut moves = Vec::new();
            // What the curve holds where the clip starts, then its moves
            // while it plays.
            let first = points.partition_point(|p| p.at <= midi.offset);
            if let Some(held) = first.checked_sub(1) {
                moves.push((clip.start, Rank::Move, points[held].value));
            }
            for point in points[first..].iter().take_while(|p| p.at < window.end) {
                let at = clip.start + (point.at - midi.offset);
                moves.push((at, Rank::Move, point.value));
            }
            let held = moves.last().map(|&(_, _, value)| value);
            if controller.is_switch() && held.is_some_and(Controller::is_on) {
                moves.push((clip.end(), Rank::LetGo, 0.0));
            }
            let curve = self.curves.entry(controller).or_default();
            for (at, rank, value) in moves {
                let play = Play::Controller { controller, value };
                self.events.push(Scheduled { at, rank, play });
                curve.push(Move { at, rank, value });
            }
        }
    }

    pub fn with_clip(mut self, clip: &Clip) -> Self {
        self.add(clip);
        self
    }

    fn release_all(&mut self, notes: &mut Notes<'_>) {
        for sounding in self.sounding.drain(..) {
            let off = NoteEvent::Off {
                note: sounding.note,
                velocity: sounding.release,
            };
            notes.send(0, off);
        }
    }

    /// Sends `controller` `value` at `offset`, and remembers it.
    fn send_controller(
        &mut self,
        notes: &mut Notes<'_>,
        offset: usize,
        controller: Controller,
        value: f32,
    ) {
        if notes.send_controller(offset, controller, value) {
            self.sent[usize::from(controller.number())] = Some(value);
        }
    }

    /// Lets go of the switches it holds on, as the transport stops.
    fn let_go(&mut self, notes: &mut Notes<'_>) {
        for controller in Controller::all().filter(|c| c.is_switch()) {
            if self.sent[usize::from(controller.number())].is_some_and(Controller::is_on) {
                self.send_controller(notes, 0, controller, 0.0);
            }
        }
    }

    /// Sends what each controller's curve holds as the block starts, as
    /// playback starts or jumps there, or with `changed`, only where that
    /// differs from what was sent, as a recompiled player takes over. A
    /// switch held on that its curve doesn't hold on there is let go.
    fn chase(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>, changed: bool) {
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as i64;
        for controller in Controller::all() {
            let held = self.curves.get(&controller).and_then(|moves| {
                let before = moves.partition_point(|m| tempo.frame_at(m.at, rate) < first);
                before.checked_sub(1).map(|last| moves[last].value)
            });
            let sent = self.sent[usize::from(controller.number())];
            let value = match held {
                Some(value) if !changed || sent != Some(value) => value,
                None if controller.is_switch() && sent.is_some_and(Controller::is_on) => 0.0,
                _ => continue,
            };
            self.send_controller(notes, 0, controller, value);
        }
    }
}

/// The id a note from a clip plays with: its clip's id in the top 32 bits,
/// and its own in the bottom 32, so whatever gets the note can tell which
/// one it is.
pub fn note_id(clip: ClipId, note: clip::NoteId) -> NoteId {
    NoteId((u64::from(clip.0) << 32) | u64::from(note.0))
}

fn rank(event: &NoteEvent) -> Rank {
    match event {
        NoteEvent::Off { .. } => Rank::Off,
        NoteEvent::On { .. } => Rank::On,
        NoteEvent::Expression { .. } => Rank::Expression,
    }
}

impl Node for MidiClips {
    fn prepare(&mut self, _config: &EngineConfig) {
        // Stable, so events at the same time keep the clips' order.
        self.events.sort_by_key(|e| (e.at, e.rank));
        for moves in self.curves.values_mut() {
            moves.sort_by_key(|m| (m.at, m.rank));
        }
        self.spans.sort_unstable();
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<MidiClips>() else {
            return;
        };
        std::mem::swap(&mut self.sounding, &mut previous.sounding);
        self.sent = previous.sent;
        self.rechase = true;
        self.last = previous.last;
        self.played_to = previous.played_to;
        for sounding in &mut self.sounding {
            let span = self.spans.binary_search_by_key(&sounding.note, |s| s.note);
            let still = span.is_ok_and(|i| {
                let span = self.spans[i];
                span.on == sounding.on && span.key == sounding.key && span.off > self.played_to
            });
            sounding.cut |= !still;
        }
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        self.sounding.retain(|sounding| {
            if sounding.cut {
                let off = NoteEvent::Off {
                    note: sounding.note,
                    velocity: sounding.release,
                };
                notes.send(0, off);
            }
            !sounding.cut
        });
        let transport = ctx.transport;
        let jumped = self.last != Some((transport.jumps, true));
        if jumped || !transport.playing {
            self.release_all(notes);
        }
        self.last = Some((transport.jumps, transport.playing));
        if !transport.playing {
            self.let_go(notes);
            return;
        }
        if jumped || self.rechase {
            self.chase(ctx, notes, !jumped);
            self.rechase = false;
        }

        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let frame = |at: Beats| tempo.frame_at(at, rate);
        let first = transport.position as i64;
        let end = first + ctx.frames as i64;
        // Carry on from the last block if that's where its next event is,
        // and look for it otherwise: after a jump, a new tempo or a swap.
        let events = &self.events;
        let carries_on = |i: usize| {
            (i == 0 || frame(events[i - 1].at) < first)
                && events.get(i).is_none_or(|e| frame(e.at) >= first)
        };
        let mut i = match self.cursor {
            Some(i) if carries_on(i) => i,
            _ => events.partition_point(|e| frame(e.at) < first),
        };
        while let Some(&scheduled) = self.events.get(i) {
            let at = frame(scheduled.at);
            if at >= end {
                break;
            }
            let offset = (at.max(first) - first) as usize;
            i += 1;
            let (event, release) = match scheduled.play {
                Play::Note { event, release } => (event, release),
                Play::Controller { controller, value } => {
                    self.send_controller(notes, offset, controller, value);
                    continue;
                }
            };
            let note = event.note();
            let sounding = self.sounding.iter().position(|s| s.note == note);
            match event {
                NoteEvent::On { key, .. } => {
                    if sounding.is_none()
                        && self.sounding.len() < MAX_SOUNDING
                        && notes.send(offset, event)
                    {
                        self.sounding.push(Sounding {
                            note,
                            on: scheduled.at,
                            key,
                            release,
                            cut: false,
                        });
                    }
                }
                NoteEvent::Expression { .. } => {
                    if sounding.is_some() {
                        notes.send(offset, event);
                    }
                }
                NoteEvent::Off { .. } => {
                    if let Some(s) = sounding {
                        notes.send(offset, event);
                        self.sounding.swap_remove(s);
                    }
                }
            }
        }
        self.cursor = Some(i);
        self.played_to = Beats::from_f64(tempo.beats_at(end as f64, rate));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use musicable_model::clip::{AudioClip, MidiClip, Note, Point};
    use musicable_model::time::{TempoMap, TimeSignature};
    use musicable_model::warp::{Warp, WarpMarker};

    use super::*;
    use crate::media::AudioData;
    use crate::{
        AudioBuffer, Block, Graph, NodeEventKind, NodeId, NodeKey, PreparedGraph,
        ReferenceRenderer, Renderer, TransportState, rt,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    /// Frames in a beat at 120 BPM and 48 kHz.
    const BEAT: u64 = 24_000;

    fn beats(beats: f64) -> Beats {
        Beats::from_f64(beats)
    }

    /// Records the notes and controllers it gets, at the song position
    /// they land on.
    struct Recorder {
        notes: Vec<(u64, NoteEvent)>,
        controllers: Vec<(u64, Controller, f32)>,
    }

    impl Node for Recorder {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
            for event in ctx.events {
                let at = ctx.transport.position + event.offset as u64;
                match event.kind {
                    NodeEventKind::Note(note) => self.notes.push((at, note)),
                    NodeEventKind::Controller { controller, value } => {
                        self.controllers.push((at, controller, value));
                    }
                    _ => {}
                }
            }
        }
    }

    /// A MIDI clip player feeding a [`Recorder`], and the transport they
    /// play with.
    struct Rig {
        graph: PreparedGraph,
        recorder: NodeId,
        transport: TransportState,
        tempo: TempoMap,
    }

    impl Rig {
        fn new(player: MidiClips) -> Self {
            let mut g = Graph::new();
            let player = g.add(NodeKey(0), player);
            let recorder = Recorder {
                notes: Vec::with_capacity(4096),
                controllers: Vec::with_capacity(4096),
            };
            let recorder = g.add(NodeKey(1), recorder);
            g.send_notes(player, recorder).unwrap();
            g.set_output(recorder).unwrap();
            Self {
                graph: g.prepare(&CONFIG).unwrap(),
                recorder,
                transport: TransportState {
                    playing: true,
                    ..TransportState::default()
                },
                tempo: TempoMap::default(),
            }
        }

        /// Plays `frames` frames on from where the transport is, in blocks
        /// of up to `block`.
        fn play(&mut self, frames: u64, block: u64) {
            let end = self.transport.position + frames;
            while self.transport.position < end {
                let frames = block.min(end - self.transport.position) as usize;
                let block = Block {
                    frames,
                    time: 0,
                    transport: self.transport,
                    tempo: &self.tempo,
                    input: crate::AudioRef::empty(frames),
                    midi: &[],
                };
                let graph = &mut self.graph;
                rt::scope(|| {
                    graph.play_notes(&block);
                    graph.load_routed();
                    ReferenceRenderer.render(graph, &block);
                    graph.clear_routed();
                });
                if self.transport.playing {
                    self.transport.position += frames as u64;
                } else {
                    break;
                }
            }
        }

        fn locate(&mut self, position: u64) {
            self.transport.position = position;
            self.transport.jumps += 1;
        }

        fn recorder(&mut self) -> &mut Recorder {
            let node = self.graph.nodes[self.recorder.0].as_mut();
            node.downcast_mut::<Recorder>().unwrap()
        }

        /// The notes recorded since last asked.
        fn notes(&mut self) -> Vec<(u64, NoteEvent)> {
            // With as much room again, so the recorder doesn't allocate
            // while it records.
            let recorded = &mut self.recorder().notes;
            let room = recorded.capacity();
            std::mem::replace(recorded, Vec::with_capacity(room))
        }

        /// The controllers' moves recorded since last asked, with the moves
        /// in beats.
        fn controllers(&mut self) -> Vec<(f64, Controller, f32)> {
            let recorded = &mut self.recorder().controllers;
            let room = recorded.capacity();
            let moves = std::mem::replace(recorded, Vec::with_capacity(room));
            let beat = |at: u64| at as f64 / BEAT as f64;
            moves
                .into_iter()
                .map(|(at, c, v)| (beat(at), c, v))
                .collect()
        }

        /// Swaps in a player for `clips`, as a recompile would.
        fn recompile(&mut self, player: MidiClips) {
            let mut next = Rig::new(player);
            next.graph.adopt_from(&mut self.graph);
            self.graph = next.graph;
        }
    }

    fn clip(id: u32, start: f64, length: f64, offset: f64, notes: Vec<Note>) -> Clip {
        Clip {
            id: ClipId(id),
            name: String::new(),
            start: beats(start),
            length: beats(length),
            content: ClipContent::Midi(MidiClip {
                offset: beats(offset),
                next_note_id: notes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0),
                notes,
                controllers: BTreeMap::new(),
            }),
        }
    }

    /// `clip` with `controller` moving as `points`, `(at, value)` pairs, do.
    fn moving(mut clip: Clip, controller: Controller, points: &[(f64, f32)]) -> Clip {
        let ClipContent::Midi(midi) = &mut clip.content else {
            unreachable!("a MIDI clip");
        };
        let points = points.iter().map(|&(at, v)| Point::new(beats(at), v));
        midi.set_controller(controller, points.collect());
        clip
    }

    const MOD: Controller = Controller::MOD_WHEEL;
    const SUSTAIN: Controller = Controller::SUSTAIN;

    fn note(id: u32, start: f64, length: f64, key: u8) -> Note {
        Note {
            id: clip::NoteId(id),
            start: beats(start),
            length: beats(length),
            key,
            velocity: 0.75,
            release_velocity: Note::RELEASE_VELOCITY,
            expression: Default::default(),
        }
    }

    fn on(key: u8) -> impl Fn(&NoteEvent) -> bool {
        move |e| matches!(*e, NoteEvent::On { key: k, .. } if k == key)
    }

    /// The kinds of the events, by name, with where they land.
    fn kinds(events: &[(u64, NoteEvent)]) -> Vec<(u64, &'static str)> {
        let kind = |e: &NoteEvent| match e {
            NoteEvent::On { .. } => "on",
            NoteEvent::Off { .. } => "off",
            NoteEvent::Expression { .. } => "expression",
        };
        events.iter().map(|(at, e)| (*at, kind(e))).collect()
    }

    #[test]
    fn notes_play_on_their_frames_with_their_expression() {
        let mut first = note(0, 0.0, 1.0, 60);
        first.expression.pitch_bend =
            vec![Point::new(beats(0.0), 0.5), Point::new(beats(0.5), 1.0)];
        first.expression.pressure = vec![Point::new(beats(0.25), 0.3)];
        first.release_velocity = 0.25;
        // Longer than the clip, so the clip's end cuts it.
        let second = note(1, 2.0, 4.0, 64);
        let clip = clip(7, 1.0, 4.0, 0.0, vec![first, second]);
        for block in [1, 7, 64] {
            let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
            rig.play(6 * BEAT, block);
            let notes = rig.notes();
            assert_eq!(
                kinds(&notes),
                [
                    (BEAT, "on"),
                    (BEAT, "expression"),
                    (BEAT + BEAT / 4, "expression"),
                    (BEAT + BEAT / 2, "expression"),
                    (2 * BEAT, "off"),
                    (3 * BEAT, "on"),
                    (5 * BEAT, "off"),
                ],
                "blocks of {block}"
            );
            let id = notes[0].1.note();
            assert_eq!(id, note_id(ClipId(7), clip::NoteId(0)));
            assert_eq!(
                notes[0].1,
                NoteEvent::On {
                    note: id,
                    key: 60,
                    velocity: 0.75
                }
            );
            assert_eq!(
                notes[1].1,
                NoteEvent::Expression {
                    note: id,
                    dimension: Dimension::PitchBend,
                    value: 0.5
                }
            );
            assert_eq!(
                notes[2].1,
                NoteEvent::Expression {
                    note: id,
                    dimension: Dimension::Pressure,
                    value: 0.3
                }
            );
            assert_eq!(
                notes[4].1,
                NoteEvent::Off {
                    note: id,
                    velocity: 0.25
                }
            );
            assert_ne!(notes[5].1.note(), id);
            assert_eq!(notes[6].1.note(), notes[5].1.note());
        }
    }

    #[test]
    fn a_clips_offset_skips_notes_before_it_and_moves_the_rest() {
        // The clip starts playing two beats into its notes.
        let notes = vec![
            note(0, 1.0, 0.5, 60),
            note(1, 2.5, 0.5, 62),
            note(2, 3.0, 2.0, 64),
        ];
        let clip = clip(0, 4.0, 2.0, 2.0, notes);
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(8 * BEAT, 64);
        let notes = rig.notes();
        assert!(!notes.iter().any(|(_, e)| on(60)(e)));
        assert_eq!(
            kinds(&notes),
            [
                (4 * BEAT + BEAT / 2, "on"),
                (5 * BEAT, "off"),
                (5 * BEAT, "on"),
                (6 * BEAT, "off"),
            ]
        );
    }

    #[test]
    fn jumps_let_go_of_sounding_notes_without_chasing_others() {
        let clip = clip(
            0,
            0.0,
            8.0,
            0.0,
            vec![note(0, 0.0, 2.0, 60), note(1, 3.0, 2.0, 62)],
        );
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
        // Into the middle of the second note: the first lets go where the
        // playhead lands, and the second doesn't start.
        rig.locate(4 * BEAT);
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
        // Back to the start, as a loop would jump.
        rig.locate(0);
        rig.play(BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
    }

    #[test]
    fn stopping_lets_go_and_a_new_tempo_doesnt() {
        let clip = clip(0, 0.0, 8.0, 0.0, vec![note(0, 0.0, 4.0, 60)]);
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(BEAT, 64);
        // Half the tempo: the playhead keeps its place in beats, so its
        // position in frames doubles, and the note goes on.
        rig.tempo = TempoMap::new(60.0, TimeSignature::FOUR_FOUR);
        rig.transport.position *= 2;
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
        rig.transport.playing = false;
        rig.play(64, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
        // Starting again from there doesn't start the note again.
        rig.transport.playing = true;
        rig.play(BEAT, 64);
        assert!(rig.notes().is_empty());
    }

    #[test]
    fn a_recompiled_player_lets_go_of_only_the_notes_it_lost() {
        let notes = vec![
            note(0, 0.0, 4.0, 60),
            note(1, 0.0, 4.0, 64),
            note(2, 0.0, 4.0, 67),
        ];
        let full = clip(0, 0.0, 8.0, 0.0, notes.clone());
        let mut rig = Rig::new(MidiClips::new().with_clip(&full));
        rig.play(BEAT, 64);
        assert_eq!(rig.notes().len(), 3);

        // Unchanged, with a note added before the others: nothing stops.
        let mut more = notes.clone();
        more.insert(0, note(3, 6.0, 1.0, 50));
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, more)));
        rig.play(BEAT, 64);
        assert!(rig.notes().is_empty());

        // The middle note deleted, and the top one shortened to end before
        // the playhead: both let go on the next frame.
        let edited = vec![note(0, 0.0, 4.0, 60), note(2, 0.0, 1.0, 67)];
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, edited)));
        rig.play(BEAT, 64);
        let notes = rig.notes();
        assert_eq!(kinds(&notes), [(2 * BEAT, "off"), (2 * BEAT, "off")]);
        // The bottom one plays on to its end.
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
    }

    #[test]
    fn notes_moved_or_transposed_as_they_sound_stop_and_longer_ones_play_on() {
        let notes = vec![
            note(0, 0.0, 4.0, 60),
            note(1, 0.0, 4.0, 64),
            note(2, 0.0, 4.0, 67),
        ];
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, notes)));
        rig.play(BEAT, 64);
        assert_eq!(rig.notes().len(), 3);
        // Longer, transposed, and moved to start where the playhead has
        // already been, so it doesn't start again.
        let edited = vec![
            note(0, 0.0, 6.0, 60),
            note(1, 0.0, 4.0, 65),
            note(2, 0.5, 4.0, 67),
        ];
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, edited)));
        rig.play(6 * BEAT, 64);
        let notes = rig.notes();
        assert_eq!(
            kinds(&notes),
            [(BEAT, "off"), (BEAT, "off"), (6 * BEAT, "off")]
        );
        assert_eq!(notes[2].1.note(), note_id(ClipId(0), clip::NoteId(0)));
    }

    #[test]
    fn controllers_move_with_their_clips_and_are_chased_where_playback_starts() {
        // A clip from beat 1 to 5, whose last move comes after its end.
        let clip = moving(
            clip(0, 1.0, 4.0, 0.0, Vec::new()),
            MOD,
            &[(0.0, 0.25), (2.0, 0.75), (6.0, 1.0)],
        );
        for block in [1, 7, 64] {
            let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
            rig.play(8 * BEAT, block);
            assert_eq!(
                rig.controllers(),
                [(1.0, MOD, 0.25), (3.0, MOD, 0.75)],
                "blocks of {block}"
            );
        }
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        // Starting in the middle of the clip sends what its curve held
        // there, at the start.
        rig.locate(2 * BEAT);
        rig.play(2 * BEAT, 64);
        assert_eq!(rig.controllers(), [(2.0, MOD, 0.25), (3.0, MOD, 0.75)]);
        // Jumping past the clip sends where it left the controller: as
        // playing from the start of the song would.
        rig.locate(6 * BEAT);
        rig.play(BEAT, 64);
        assert_eq!(rig.controllers(), [(6.0, MOD, 0.75)]);
        // Before the first move, nothing has moved it yet, so it's left
        // where it is.
        rig.locate(0);
        rig.play(2 * BEAT, 64);
        assert_eq!(rig.controllers(), [(1.0, MOD, 0.25)]);
    }

    #[test]
    fn a_clip_that_starts_inside_its_curve_starts_with_what_the_curve_holds() {
        // Two beats into its content, and so past the curve's first point.
        let clip = moving(
            clip(0, 4.0, 4.0, 2.0, Vec::new()),
            MOD,
            &[(1.0, 0.5), (3.0, 1.0)],
        );
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(10 * BEAT, 64);
        assert_eq!(rig.controllers(), [(4.0, MOD, 0.5), (5.0, MOD, 1.0)]);
    }

    #[test]
    fn switches_held_on_are_let_go_where_clips_end_and_the_transport_stops() {
        let first = moving(
            clip(0, 0.0, 4.0, 0.0, Vec::new()),
            SUSTAIN,
            &[(0.0, 1.0), (1.0, 0.0), (2.0, 1.0)],
        );
        // Straight after the first, with the pedal down from its start. Added
        // first, it still moves after the first clip lets go.
        let second = moving(clip(1, 4.0, 4.0, 0.0, Vec::new()), SUSTAIN, &[(0.0, 1.0)]);
        let mut rig = Rig::new(MidiClips::new().with_clip(&second).with_clip(&first));
        rig.play(6 * BEAT, 64);
        assert_eq!(
            rig.controllers(),
            [
                (0.0, SUSTAIN, 1.0),
                (1.0, SUSTAIN, 0.0),
                (2.0, SUSTAIN, 1.0),
                (4.0, SUSTAIN, 0.0),
                (4.0, SUSTAIN, 1.0),
            ]
        );
        // Stopping lets go of it, once.
        rig.transport.playing = false;
        rig.play(64, 64);
        rig.play(64, 64);
        assert_eq!(rig.controllers(), [(6.0, SUSTAIN, 0.0)]);
        // And starting again from there holds it down again.
        rig.transport.playing = true;
        rig.play(BEAT, 64);
        assert_eq!(rig.controllers(), [(6.0, SUSTAIN, 1.0)]);
        // A jump to where no clip holds it lets go of it.
        rig.locate(BEAT + BEAT / 2);
        rig.play(BEAT / 4, 64);
        assert_eq!(rig.controllers(), [(1.5, SUSTAIN, 0.0)]);
    }

    #[test]
    fn a_recompiled_player_sends_what_its_curves_hold_differently() {
        let notes = vec![note(0, 0.0, 8.0, 60)];
        let clip = |id, value: f32| {
            let clip = moving(clip(id, 0.0, 8.0, 0.0, notes.clone()), MOD, &[(0.0, value)]);
            moving(clip, SUSTAIN, &[(0.0, 1.0)])
        };
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip(0, 0.5)));
        rig.play(BEAT, 64);
        assert_eq!(rig.controllers(), [(0.0, MOD, 0.5), (0.0, SUSTAIN, 1.0)]);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
        // Unchanged: nothing is sent again.
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.5)));
        rig.play(BEAT, 64);
        assert!(rig.controllers().is_empty());
        // The mod wheel's curve changed, and the sustain pedal's is gone.
        let changed = moving(clip(0, 0.8), SUSTAIN, &[]);
        rig.recompile(MidiClips::new().with_clip(&changed));
        rig.play(BEAT, 64);
        assert_eq!(rig.controllers(), [(2.0, MOD, 0.8), (2.0, SUSTAIN, 0.0)]);
        // The note sounds on through all of it.
        assert!(rig.notes().is_empty());
    }

    #[test]
    fn notes_alike_play_as_two() {
        let clip = clip(
            0,
            0.0,
            4.0,
            0.0,
            vec![note(0, 0.0, 1.0, 60), note(1, 0.0, 2.0, 60)],
        );
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(3 * BEAT, 64);
        let notes = rig.notes();
        assert_eq!(
            kinds(&notes),
            [(0, "on"), (0, "on"), (BEAT, "off"), (2 * BEAT, "off")]
        );
        assert_ne!(notes[0].1.note(), notes[1].1.note());
    }

    /// Plays `player` from frame `from` for `frames` frames, in blocks of
    /// up to `block`, and returns the left and right channels.
    fn audio(player: AudioClips, from: u64, frames: usize, block: usize) -> [Vec<f32>; 2] {
        audio_at(&TempoMap::default(), player, from, frames, block)
    }

    /// Plays `player` as [`audio`] does, at `tempo`.
    fn audio_at(
        tempo: &TempoMap,
        mut player: AudioClips,
        from: u64,
        frames: usize,
        block: usize,
    ) -> [Vec<f32>; 2] {
        let config = EngineConfig {
            max_block: block,
            ..CONFIG
        };
        player.prepare(&config);
        let mut buffer = AudioBuffer::new(2, block);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        while left.len() < frames {
            let n = block.min(frames - left.len());
            let ctx = ProcessContext {
                frames: n,
                sample_rate: 48_000,
                time: 0,
                transport: TransportState {
                    playing: true,
                    position: from + left.len() as u64,
                    ..TransportState::default()
                },
                tempo,
                events: &[],
                input: crate::AudioRef::empty(n),
                midi: &[],
            };
            rt::scope(|| player.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n)));
            left.extend_from_slice(buffer.view(n).channel(0));
            right.extend_from_slice(buffer.view(n).channel(1));
        }
        [left, right]
    }

    fn audio_clip(start: f64, length: f64, audio: AudioClip) -> Clip {
        Clip {
            id: ClipId(0),
            name: String::new(),
            start: beats(start),
            length: beats(length),
            content: ClipContent::Audio(audio),
        }
    }

    /// A stereo file whose samples count up: frame `i` is `i` on the left
    /// and `-i` on the right.
    fn counting(frames: usize, sample_rate: u32) -> Source {
        let left = (0..frames).map(|i| i as f32).collect::<Vec<_>>();
        let right = left.iter().map(|s| -s).collect();
        Source::Whole(Arc::new(AudioData::new(sample_rate, vec![left, right])))
    }

    #[test]
    fn audio_clips_play_their_files_sample_for_sample() {
        let mut take = AudioClip::new("take.wav");
        take.offset = Offset::Frames(1000);
        take.gain = 0.5;
        // From beat 1, for a beat and a half, but the file runs out first.
        let clip = audio_clip(1.0, 1.5, take);
        for block in [1, 64, 1000] {
            let player = AudioClips::new().with_clip(&clip, Some(counting(20_000, 48_000)));
            let [left, right] = audio(player, 0, 70_000, block);
            for (i, (&l, &r)) in left.iter().zip(&right).enumerate() {
                let frame = i as u64;
                let file = 1000 + frame as i64 - BEAT as i64;
                let expected = match frame {
                    _ if frame < BEAT => 0.0,
                    _ if file < 20_000 => file as f32 * 0.5,
                    _ => 0.0,
                };
                assert_eq!(
                    (l, r),
                    (expected, -expected),
                    "frame {frame}, blocks of {block}"
                );
            }
        }
    }

    #[test]
    fn mono_files_play_in_both_channels() {
        let data = AudioData::new(48_000, vec![vec![0.25; 100]]);
        let source = Source::Whole(Arc::new(data));
        let clip = audio_clip(0.0, 1.0, AudioClip::new("mono.wav"));
        let [left, right] = audio(AudioClips::new().with_clip(&clip, Some(source)), 0, 200, 64);
        assert_eq!(left[..100], [0.25; 100]);
        assert_eq!(left, right);
        assert_eq!(left[100..], [0.0; 100]);
    }

    #[test]
    fn fades_are_quarter_sines_that_crossfade_at_constant_power() {
        let dc = || Source::Whole(Arc::new(AudioData::new(48_000, vec![vec![1.0; 100_000]])));
        let mut fading = AudioClip::new("a.wav");
        fading.fade_in = beats(0.25);
        fading.fade_out = beats(0.5);
        let clip = audio_clip(0.0, 2.0, fading);
        let [out, _] = audio(
            AudioClips::new().with_clip(&clip, Some(dc())),
            0,
            50_000,
            64,
        );
        let fade_in = BEAT as usize / 4;
        let fade_out = BEAT as usize / 2;
        let quarter_sine = |t: f64| (FRAC_PI_2 * t).sin() as f32;
        assert_eq!(out[0], 0.0);
        assert_eq!(out[fade_in / 2], quarter_sine(0.5));
        assert_eq!(
            out[fade_in..48_000 - fade_out],
            vec![1.0; 48_000 - fade_out - fade_in][..]
        );
        assert_eq!(out[48_000 - fade_out / 2], quarter_sine(0.5));
        assert_eq!(out[47_999], quarter_sine(1.0 / fade_out as f64));
        assert_eq!(out[48_000..], [0.0; 2_000]);

        // One clip fades out over the frames the next fades in over.
        let mut a = AudioClip::new("a.wav");
        a.fade_out = beats(1.0);
        let mut b = AudioClip::new("b.wav");
        b.fade_in = beats(1.0);
        let (a, mut b) = (audio_clip(0.0, 2.0, a), audio_clip(1.0, 2.0, b));
        b.id = ClipId(1);
        let only = |clip: &Clip| {
            let [out, _] = audio(AudioClips::new().with_clip(clip, Some(dc())), 0, 72_000, 64);
            out
        };
        let (a, b) = (only(&a), only(&b));
        for frame in BEAT as usize..2 * BEAT as usize {
            let power = a[frame] * a[frame] + b[frame] * b[frame];
            assert!((power - 1.0).abs() < 1e-5, "frame {frame}: {power}");
        }
    }

    #[test]
    fn files_at_another_rate_are_resampled() {
        // A 1 kHz sine at 44.1 kHz, played at 48 kHz.
        let rate = 44_100;
        let sine = |i: usize, rate: u32| {
            (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(rate)).sin() as f32
        };
        let samples = (0..50_000).map(|i| sine(i, rate)).collect();
        let source = Source::Whole(Arc::new(AudioData::new(rate, vec![samples])));
        let clip = audio_clip(0.0, 2.0, AudioClip::new("44k.wav"));
        let player = || AudioClips::new().with_clip(&clip, Some(source.clone()));
        let [out, _] = audio(player(), 0, 40_000, 64);
        for (i, &s) in out.iter().enumerate().skip(100) {
            assert!((s - sine(i, 48_000)).abs() < 2e-3, "frame {i}: {s}");
        }
        // However the blocks fall.
        assert_eq!(audio(player(), 0, 40_000, 7)[0], out);
        assert_eq!(audio(player(), 1000, 5000, 64)[0], out[1000..6000]);
    }

    /// `clip`, an audio clip, warped with `markers` at `tempo`, playing from
    /// `offset` in its own time.
    fn warp(mut clip: Clip, markers: &[(u64, f64)], tempo: f64, offset: f64) -> Clip {
        let ClipContent::Audio(audio) = &mut clip.content else {
            unreachable!("an audio clip");
        };
        let markers = markers.iter().map(|&(frame, beat)| WarpMarker {
            frame,
            beat: beats(beat),
        });
        audio.warp = Some(Warp {
            markers: markers.collect(),
            ..Warp::new(0, Beats::ZERO, tempo)
        });
        audio.offset = Offset::Beats(beats(offset));
        clip
    }

    /// Where `clip`'s pieces say its file plays on song frame `frame`, and
    /// how fast.
    fn plays(player: &AudioClips, tempo: &TempoMap, frame: i64) -> (f64, f64) {
        let mut at = None;
        player.clips[0].pieces(tempo, 48_000, frame, frame + 1, |piece| {
            assert!(piece.from <= frame && frame < piece.to);
            at = Some((piece.position(frame), piece.step));
        });
        at.expect("a piece")
    }

    #[test]
    fn warped_clips_play_where_their_markers_say() {
        // Twice the song's speed for half a beat of the clip's own time,
        // then at it for a beat, then at a third of it, from an eighth of a
        // beat in.
        let markers = [(0, 0.0), (24_000, 0.5), (48_000, 1.5), (60_000, 3.0)];
        let clip = warp(
            audio_clip(2.0, 3.0, AudioClip::new("breaks.wav")),
            &markers,
            120.0,
            0.125,
        );
        let mut player = AudioClips::new().with_clip(&clip, Some(counting(72_000, 48_000)));
        player.prepare(&CONFIG);
        let tempo = TempoMap::default();
        let frame = |beat: f64| (beat * BEAT as f64) as i64;
        assert_eq!(plays(&player, &tempo, frame(2.0)), (6_000.0, 2.0));
        assert_eq!(plays(&player, &tempo, frame(2.375)), (24_000.0, 1.0));
        assert_eq!(plays(&player, &tempo, frame(3.0)), (39_000.0, 1.0));
        let (at, step) = plays(&player, &tempo, frame(4.875));
        assert!((at - 60_000.0).abs() < 1e-6 && (step - 1.0 / 3.0).abs() < 1e-12);
        // Before the first marker, at the first segment's speed.
        assert_eq!(plays(&player, &tempo, frame(1.0)), (-42_000.0, 2.0));
        let clip = &player.clips[0];
        let mut readings = Vec::new();
        clip.pieces(&tempo, 48_000, frame(2.0), frame(5.0), |piece| {
            readings.push(match clip.reading(&piece) {
                Reading::Copy { by } => format!("copy {by}"),
                Reading::Resample(r) => format!("resample {:.3}", r.step()),
            });
        });
        assert_eq!(
            readings,
            ["resample 2.000", "copy -33000", "resample 0.333"]
        );
    }

    #[test]
    fn warped_clips_follow_the_songs_tempo_changes() {
        let mut tempo = TempoMap::default();
        tempo.insert_tempo(beats(2.0), 60.0);
        // At 120 BPM, as the song is until beat 2.
        let clip = warp(
            audio_clip(0.0, 4.0, AudioClip::new("loop.wav")),
            &[(0, 0.0)],
            120.0,
            0.0,
        );
        let mut player = AudioClips::new().with_clip(&clip, Some(counting(200_000, 48_000)));
        player.prepare(&CONFIG);
        assert_eq!(plays(&player, &tempo, 24_000), (24_000.0, 1.0));
        // A beat at 60 BPM is a second: twice as many frames.
        assert_eq!(plays(&player, &tempo, 48_000), (48_000.0, 0.5));
        assert_eq!(plays(&player, &tempo, 96_000), (72_000.0, 0.5));
        // Sample for sample until the change, then at half speed.
        let [left, _] = audio_at(&tempo, player, 0, 100_000, 64);
        assert_eq!(
            left[..48_000],
            (0..48_000).map(|i| i as f32).collect::<Vec<_>>()[..]
        );
        for (i, &s) in left.iter().enumerate().skip(48_100).step_by(997) {
            let expected = 48_000.0 + (i - 48_000) as f32 / 2.0;
            assert!((s - expected).abs() < 0.5, "frame {i}: {s}");
        }
    }

    #[test]
    fn a_clip_warped_to_the_songs_tempo_plays_as_it_would_unwarped() {
        let mut take = AudioClip::new("take.wav");
        take.offset = Offset::Frames(1000);
        take.gain = 0.5;
        let unwarped = audio_clip(1.0, 1.5, take);
        // The marker pins frame 1,000 to the clip's start.
        let warped = warp(unwarped.clone(), &[(1000, 0.0)], 120.0, 0.0);
        for block in [1, 64, 1000] {
            let play = |clip: &Clip| {
                let player = AudioClips::new().with_clip(clip, Some(counting(20_000, 48_000)));
                audio(player, 0, 70_000, block)
            };
            assert_eq!(play(&warped), play(&unwarped), "blocks of {block}");
        }
    }

    #[test]
    fn warped_clips_are_resampled_to_their_speed_however_the_blocks_fall() {
        let sine = |frequency: f64, i: usize| {
            (std::f64::consts::TAU * frequency * i as f64 / 48_000.0).sin() as f32
        };
        let samples = (0..100_000).map(|i| sine(1_000.0, i)).collect();
        let source = Source::Whole(Arc::new(AudioData::new(48_000, vec![samples])));
        // At 60 BPM in a song at 120 the file plays twice as fast, an
        // octave up, and at 240, half as fast, an octave down.
        for (tempo, frequency) in [(60.0, 2_000.0), (240.0, 500.0)] {
            let clip = warp(
                audio_clip(0.0, 3.0, AudioClip::new("sine.wav")),
                &[(0, 0.0)],
                tempo,
                0.0,
            );
            let player = || AudioClips::new().with_clip(&clip, Some(source.clone()));
            let [out, _] = audio(player(), 0, 40_000, 64);
            for (i, &s) in out.iter().enumerate().skip(300) {
                let e = (s - sine(frequency, i)).abs();
                assert!(e < 2e-3, "{tempo} BPM, frame {i}: off by {e}");
            }
            assert_eq!(audio(player(), 0, 40_000, 7)[0], out);
            assert_eq!(audio(player(), 0, 40_000, 1000)[0], out);
            assert_eq!(audio(player(), 1000, 5000, 64)[0], out[1000..6000]);
        }
    }

    #[test]
    fn gain_curves_follow_the_clips_own_time() {
        let dc = || Source::Whole(Arc::new(AudioData::new(48_000, vec![vec![1.0; 200_000]])));
        let point = musicable_model::automation::Point::new;
        let rising = vec![point(beats(0.0), 0.0), point(beats(1.0), 1.0)];
        // Halfway up a fader's taper is a sixteenth: the fourth root spreads
        // evenly.
        let halfway = 0.0625;
        // Unwarped, the clip's own time is the file's seconds: from half a
        // second in, it reaches the top half a second later, at the song's
        // beat 1, and its gain of 0.5 doesn't count.
        let mut take = AudioClip::new("dc.wav");
        take.offset = Offset::Frames(24_000);
        take.gain = 0.5;
        take.set_lane(ClipParam::Gain, rising.clone());
        let clip = audio_clip(0.0, 3.0, take);
        let [out, _] = audio(
            AudioClips::new().with_clip(&clip, Some(dc())),
            0,
            48_000,
            64,
        );
        assert_eq!(out[0], halfway);
        assert_eq!(out[24_000..], [1.0; 24_000]);
        // Warped, it's in beats: at 60 BPM, a beat of the clip's own time
        // is two of the song's. Resampled, the file's 1.0 comes through to
        // within the weights' rounding.
        let mut loop_ = AudioClip::new("dc.wav");
        loop_.set_lane(ClipParam::Gain, rising);
        let clip = warp(audio_clip(0.0, 3.0, loop_), &[(0, 0.0)], 60.0, 0.0);
        let [out, _] = audio(
            AudioClips::new().with_clip(&clip, Some(dc())),
            0,
            72_000,
            64,
        );
        assert_eq!(out[0], 0.0);
        assert!((out[12_000] - halfway).abs() < 1e-6, "{}", out[12_000]);
        assert!(out[24_000..].iter().all(|&s| (s - 1.0).abs() < 1e-6));
    }

    /// Reads `data` as a file on disk would be.
    struct InMemory(Arc<AudioData>);

    impl crate::media::AudioReader for InMemory {
        fn channels(&self) -> usize {
            self.0.channels()
        }

        fn sample_rate(&self) -> u32 {
            self.0.sample_rate()
        }

        fn frames(&self) -> u64 {
            self.0.frames()
        }

        fn read(&mut self, start: u64, out: &mut [f32]) -> std::io::Result<usize> {
            let channels = self.channels();
            let frames = (out.len() / channels).min(self.frames().saturating_sub(start) as usize);
            for (i, frame) in out.chunks_exact_mut(channels).take(frames).enumerate() {
                for (c, sample) in frame.iter_mut().enumerate() {
                    *sample = self.0.channel(c)[start as usize + i];
                }
            }
            Ok(frames)
        }
    }

    #[test]
    fn long_blocks_read_a_stream_a_piece_at_a_time() {
        let Source::Whole(data) = counting(400_000, 48_000) else {
            unreachable!()
        };
        // Twice the song's speed: a block of 65,536 frames reads more of the
        // file than a stream holds at once.
        let clip = warp(
            audio_clip(0.5, 6.0, AudioClip::new("long.wav")),
            &[(1_000, 0.0)],
            60.0,
            0.0,
        );
        let whole = AudioClips::new().with_clip(&clip, Some(Source::Whole(data.clone())));
        let expected = audio(whole, 0, 150_000, 64);
        let streamer = crate::media::Streamer::spawn(true);
        for block in [65_536, 512] {
            let stream = streamer.stream(Box::new(InMemory(data.clone())));
            let player = AudioClips::new().with_clip(&clip, Some(Source::Stream(stream.clone())));
            let played = audio(player, 0, 150_000, block);
            assert!(played == expected, "blocks of {block}");
            assert_eq!(stream.missed(), 0);
        }
    }

    #[test]
    fn clips_without_a_source_are_silent() {
        let clip = audio_clip(0.0, 1.0, AudioClip::new("missing.wav"));
        let [left, right] = audio(AudioClips::new().with_clip(&clip, None), 0, 1000, 64);
        assert!(left.iter().chain(&right).all(|&s| s == 0.0));
    }
}
