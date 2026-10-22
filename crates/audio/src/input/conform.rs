//! Lining an input up with the output: from the packets the input device
//! captures, timed by its own clock, to the engine's timeline.
//!
//! Each packet comes with the device's position for its first frame and the
//! time it was captured. The output publishes which engine frame it was
//! playing when (see [`Playback`]). Together they give, for each packet,
//! the offset between the input's position and the engine frame heard as it
//! was captured. The input's frames go into its [`InputBuffer`](super::InputBuffer)
//! at those engine frames:
//!
//! - **Same clock** (the output device's own inputs, or a loopback of it,
//!   at the output's rate): the offset can't drift, so it's a whole number
//!   of frames, the median of the first few packets', and frames are copied
//!   exactly. Should the medians move by more than a few frames for good,
//!   the offset moves with them: a slip. A second slip the same way means
//!   the clocks drift apart after all, and the input goes over to being
//!   resampled.
//! - **Another clock**: the offset drifts by a few parts per million, so the
//!   input is resampled, with a phase-locked loop following the offsets: a
//!   fast one at first, which settles to a slow one so that jitter in the
//!   timings doesn't come through as wow.

use std::f64::consts::TAU;
use std::sync::Arc;

use musicable_engine::resample::Resampler;

use super::InputCounters;
use super::clock::Playback;
use super::ring::InputWriter;

/// Packets whose offsets go into the first median.
const ACQUIRE: usize = 16;
/// Offsets kept for medians.
const WINDOW: usize = 64;
/// How often a locked input checks its offset, in packets.
const CHECK_EVERY: u32 = 16;
/// How far the median offset can move from a locked one before it counts,
/// in frames: well beyond what jitter in the timings moves it. Twice in a
/// row, and the input slips.
const SLIP_LIMIT: f64 = 4.0;
/// Frames of an input on another clock kept for resampling.
const HISTORY: usize = 16_384;
/// How far off a resampled input's timing can be before it starts again
/// rather than slowly catching up, in frames.
const RESTART_LIMIT: f64 = 2_048.0;
/// The most a resampled input's rate can stray from its nominal one: half a
/// percent, far beyond any clock's drift.
const MAX_DRIFT: f64 = 0.005;
/// Frames resampled before they go to the buffer together.
const CHUNK: usize = 256;

/// One packet from the device, as it captured it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Packet<'a> {
    /// The device's position for the first frame: frames since the input
    /// started, by its own count. A position past the end of the packet
    /// before means the device lost the frames in between.
    pub position: u64,
    /// When the first frame was captured, in nanoseconds on the time base
    /// of the playback clock, or `None` when the device can't say.
    pub time: Option<u64>,
    pub frames: usize,
    /// The frames, interleaved, or `None` for silence.
    pub samples: Option<&'a [f32]>,
}

/// Lines one input up with the output, on the thread that captures it.
/// Real-time safe once made.
pub(crate) struct Conformer {
    writer: InputWriter,
    channels: usize,
    clock: Arc<Playback>,
    counters: Arc<InputCounters>,
    /// Capture frames per engine frame, by the two nominal rates.
    ratio: f64,
    engine_rate: u32,
    /// The device position just after the last frame captured.
    captured: Option<u64>,
    offsets: Offsets,
    state: State,
    /// For resampling: kept up to date on the output's clock too, in case
    /// the input turns out to drift.
    tracking: Box<Tracking>,
}

enum State {
    /// Gathering offsets for the first median.
    Acquiring,
    /// On the output's clock: capture frame `c` is engine frame `c - offset`.
    Locked {
        offset: i64,
        /// Checks in a row that found the median offset moved.
        strikes: u32,
        since_check: u32,
        /// Whether the offset has been checked against a full window.
        settled: bool,
        /// Which way the last slip went: 1 later, -1 earlier, 0 none yet.
        slipped: i64,
    },
    /// On another clock, resampled.
    Tracking,
}

impl Conformer {
    /// Lines up an input of `channels` channels at `rate` Hz with an output
    /// at `engine_rate` Hz, writing into `writer`. `same_clock` says the two
    /// run on one clock. Allocates.
    pub fn new(
        writer: InputWriter,
        rate: u32,
        engine_rate: u32,
        same_clock: bool,
        clock: Arc<Playback>,
        counters: Arc<InputCounters>,
    ) -> Conformer {
        let channels = writer.channels();
        let ratio = f64::from(rate) / f64::from(engine_rate);
        let locks = same_clock && rate == engine_rate;
        let state = if locks {
            State::Acquiring
        } else {
            State::Tracking
        };
        counters.set_resampled(!locks);
        Conformer {
            writer,
            channels,
            clock,
            counters,
            ratio,
            engine_rate,
            captured: None,
            offsets: Offsets::default(),
            state,
            tracking: Box::new(Tracking::new(channels, rate, engine_rate)),
        }
    }

    /// Takes a packet the device captured. Real-time safe.
    pub fn push(&mut self, packet: Packet<'_>) {
        debug_assert!(
            packet
                .samples
                .is_none_or(|s| s.len() == packet.frames * self.channels)
        );
        // Frames the device lost before this packet, or already sent.
        let (mut gap, mut skip) = (0, 0);
        if let Some(captured) = self.captured {
            if packet.position > captured {
                gap = packet.position - captured;
                self.counters.lost(gap);
            } else {
                skip = ((captured - packet.position) as usize).min(packet.frames);
            }
        }
        let end = packet.position + packet.frames as u64;
        self.captured = Some(self.captured.map_or(end, |captured| captured.max(end)));
        self.counters.captured((packet.frames - skip) as u64);

        // The offset between the device's position and the engine frame
        // heard as the packet's first frame was captured.
        let observed = packet
            .time
            .zip(self.clock.reading())
            .map(|(time, reading)| reading.frame_at(time));
        let offset = observed.map(|w| packet.position as f64 - w * self.ratio);
        if let Some(offset) = offset {
            self.offsets.push(offset);
        }

        let tracking = &mut self.tracking;
        tracking.store(packet, gap, skip);
        match &mut self.state {
            State::Acquiring => {
                if self.offsets.len() >= ACQUIRE {
                    let offset = self.offsets.median().round() as i64;
                    self.state = State::Locked {
                        offset,
                        strikes: 0,
                        since_check: 0,
                        settled: false,
                        slipped: 0,
                    };
                    self.counters.set_aligned(true);
                    self.write_locked(packet, skip);
                }
            }
            State::Locked { .. } => {
                if offset.is_some() && self.check_lock() {
                    // Drifting: resampled from here on.
                    self.state = State::Tracking;
                    self.counters.set_resampled(true);
                    let median = self.offsets.median();
                    self.tracking.resume(median, self.ratio, self.writer.end());
                    self.tracking.produce(&mut self.writer);
                } else {
                    self.write_locked(packet, skip);
                }
            }
            State::Tracking => {
                if !tracking.started {
                    if self.offsets.len() >= ACQUIRE {
                        tracking.start(self.offsets.median(), self.ratio, &mut self.writer);
                        self.counters.set_aligned(true);
                    }
                } else if let (Some(w), Some(_)) = (observed, offset)
                    && tracking.follow(w, packet.position, self.engine_rate)
                {
                    // Too far off to catch up: start again from here.
                    self.counters.slip();
                    self.offsets.clear();
                    self.offsets.push(packet.position as f64 - w * self.ratio);
                    tracking.restart(packet.position as f64 - w * self.ratio, self.ratio);
                }
                if tracking.started {
                    tracking.produce(&mut self.writer);
                    self.counters
                        .set_drift(tracking.step / tracking.nominal - 1.0);
                }
            }
        }
    }

    /// Checks a locked offset against the median, and slips to the median
    /// if it has moved for good. Returns `true` if it slipped the same way
    /// twice: the clocks drift apart.
    fn check_lock(&mut self) -> bool {
        let State::Locked {
            offset,
            strikes,
            since_check,
            settled,
            slipped,
        } = &mut self.state
        else {
            return false;
        };
        *since_check += 1;
        if *since_check < CHECK_EVERY {
            return false;
        }
        *since_check = 0;
        let median = self.offsets.median();
        // The first median of a full window replaces the first rough one,
        // without counting as a slip.
        if !*settled && self.offsets.len() == WINDOW {
            *settled = true;
            *offset = median.round() as i64;
            return false;
        }
        if (median - *offset as f64).abs() > SLIP_LIMIT {
            *strikes += 1;
        } else {
            *strikes = 0;
        }
        if *strikes < 2 {
            return false;
        }
        *strikes = 0;
        let moved = median.round() as i64 - *offset;
        *offset += moved;
        self.counters.slip();
        let drifting = *slipped == moved.signum();
        *slipped = moved.signum();
        drifting
    }

    /// Writes a packet's frames from frame `skip` on, at the engine frames a
    /// locked offset puts them at.
    fn write_locked(&mut self, packet: Packet<'_>, mut skip: usize) {
        let State::Locked { offset, .. } = self.state else {
            return;
        };
        let channels = self.channels;
        let mut first = (packet.position + skip as u64) as i64 - offset;
        if first < 0 {
            // From before the engine started: nothing to line up with.
            skip += (-first) as usize;
            first = 0;
        }
        if skip >= packet.frames {
            return;
        }
        let first = first as u64;
        if !self.writer.has_begun() {
            self.writer.begin(first);
        }
        let end = self.writer.end();
        if first > end {
            // Frames the device lost, or a slip forward.
            self.writer.append_silence(first - end);
        } else {
            // Frames already written, after a slip back.
            skip += (end - first) as usize;
            if skip >= packet.frames {
                return;
            }
        }
        match packet.samples {
            Some(samples) => self.writer.append(&samples[skip * channels..]),
            None => self.writer.append_silence((packet.frames - skip) as u64),
        }
    }
}

/// The last [`WINDOW`] offsets, for their median. Real-time safe.
#[derive(Debug)]
struct Offsets {
    values: [f64; WINDOW],
    next: usize,
    len: usize,
    scratch: [f64; WINDOW],
}

impl Default for Offsets {
    fn default() -> Self {
        Offsets {
            values: [0.0; WINDOW],
            next: 0,
            len: 0,
            scratch: [0.0; WINDOW],
        }
    }
}

impl Offsets {
    fn push(&mut self, value: f64) {
        self.values[self.next] = value;
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        self.len = 0;
        self.next = 0;
    }

    /// The median of the offsets kept. 0 when there are none.
    fn median(&mut self) -> f64 {
        if self.len == 0 {
            return 0.0;
        }
        let values = &mut self.scratch[..self.len];
        values.copy_from_slice(&self.values[..self.len]);
        let middle = self.len / 2;
        let (_, upper, _) = values.select_nth_unstable_by(middle, f64::total_cmp);
        let upper = *upper;
        if self.len % 2 == 1 {
            upper
        } else {
            let lower = values[..middle]
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            (lower + upper) / 2.0
        }
    }
}

/// An input on another clock, resampled onto the engine's timeline.
struct Tracking {
    resampler: Resampler,
    weights: Box<[f32]>,
    channels: usize,
    /// The last [`HISTORY`] frames captured, interleaved, by device
    /// position.
    history: Box<[f32]>,
    /// The device position just after the newest frame in `history`.
    end: u64,
    /// The device position of the first frame stored.
    first: Option<u64>,
    started: bool,
    /// The next engine frame to write, and the device position it falls on.
    next_w: u64,
    next_x: f64,
    /// Device frames per engine frame, as the loop has it.
    step: f64,
    /// Device frames per engine frame, by the nominal rates.
    nominal: f64,
    /// Offsets followed since the start.
    followed: u64,
    /// The engine frame of the last offset followed.
    last_w: Option<f64>,
    /// Frames resampled, waiting to go to the buffer.
    out: Box<[f32]>,
    pending: usize,
}

impl Tracking {
    fn new(channels: usize, rate: u32, engine_rate: u32) -> Tracking {
        let resampler = Resampler::new(rate, engine_rate);
        Tracking {
            resampler,
            weights: vec![0.0; 2 * resampler.reach()].into_boxed_slice(),
            channels,
            history: vec![0.0; HISTORY * channels].into_boxed_slice(),
            end: 0,
            first: None,
            started: false,
            next_w: 0,
            next_x: 0.0,
            step: resampler.step(),
            nominal: resampler.step(),
            followed: 0,
            last_w: None,
            out: vec![0.0; CHUNK * channels].into_boxed_slice(),
            pending: 0,
        }
    }

    /// Keeps a packet's frames from `skip` on, after `gap` frames the device
    /// lost, which are silent.
    fn store(&mut self, packet: Packet<'_>, gap: u64, skip: usize) {
        let channels = self.channels;
        if self.first.is_none() {
            self.first = Some(packet.position);
            self.end = packet.position;
        }
        let silent = gap.min(HISTORY as u64);
        for at in packet.position - silent..packet.position {
            self.slot(at).fill(0.0);
        }
        for i in skip..packet.frames {
            let frame = match packet.samples {
                Some(samples) => &samples[i * channels..(i + 1) * channels],
                None => &[][..],
            };
            let slot = self.slot(packet.position + i as u64);
            if frame.is_empty() {
                slot.fill(0.0);
            } else {
                slot.copy_from_slice(frame);
            }
        }
        self.end = self.end.max(packet.position + packet.frames as u64);
    }

    fn slot(&mut self, position: u64) -> &mut [f32] {
        let i = (position as usize) % HISTORY;
        &mut self.history[i * self.channels..(i + 1) * self.channels]
    }

    /// Starts with the input's position `offset` + `ratio` × the engine
    /// frame, at the oldest engine frame whose frames it still has.
    fn start(&mut self, offset: f64, ratio: f64, writer: &mut InputWriter) {
        self.restart(offset, ratio);
        if !writer.has_begun() {
            writer.begin(self.next_w);
        }
    }

    /// Takes over from frame `next_w` on, where an input that seemed to be
    /// on the output's clock left off, with the input's position `offset` +
    /// `ratio` × the engine frame.
    fn resume(&mut self, offset: f64, ratio: f64, next_w: u64) {
        self.started = true;
        self.next_w = next_w;
        self.restart(offset, ratio);
    }

    /// Starts following the offset `offset` afresh, from the engine frame
    /// after the last one written.
    fn restart(&mut self, offset: f64, ratio: f64) {
        let reach = self.resampler.reach() as f64;
        let oldest = self
            .first
            .unwrap_or(0)
            .max(self.end.saturating_sub(HISTORY as u64)) as f64;
        // The first engine frame whose frames are all still there.
        let earliest = ((oldest + reach - offset) / ratio).ceil().max(0.0) as u64;
        if !self.started || earliest > self.next_w {
            self.next_w = earliest;
        }
        self.next_x = offset + self.next_w as f64 * ratio;
        self.step = self.nominal;
        self.followed = 0;
        self.last_w = None;
        self.started = true;
    }

    /// Follows the offset seen at engine frame `w`, where the device was at
    /// `position`. Returns `true` if it's too far off to follow.
    fn follow(&mut self, w: f64, position: u64, engine_rate: u32) -> bool {
        let predicted = self.next_x + (w - self.next_w as f64) * self.step;
        let error = position as f64 - predicted;
        if error.abs() > RESTART_LIMIT {
            return true;
        }
        let interval = self
            .last_w
            .map_or(0.01, |last| (w - last) / f64::from(engine_rate))
            .clamp(0.000_5, 0.1);
        self.last_w = Some(w);
        // A critically damped loop, wide at first to lock on quickly, then
        // narrow so timing jitter doesn't come through.
        let bandwidth = match self.followed {
            0..100 => 2.0,
            100..1_000 => 0.5,
            _ => 0.1,
        };
        self.followed += 1;
        let omega = TAU * bandwidth * interval;
        self.next_x += 2.0 * omega * error;
        self.step += omega * omega / (interval * f64::from(engine_rate)) * error;
        let (low, high) = (
            self.nominal * (1.0 - MAX_DRIFT),
            self.nominal * (1.0 + MAX_DRIFT),
        );
        self.step = self.step.clamp(low, high);
        false
    }

    /// Resamples every engine frame whose input frames have all arrived.
    fn produce(&mut self, writer: &mut InputWriter) {
        let reach = self.resampler.reach() as i64;
        let channels = self.channels;
        loop {
            let base = self.next_x.floor() as i64;
            if base + reach >= self.end as i64 {
                break;
            }
            let oldest = self.end.saturating_sub(HISTORY as u64) as i64;
            if base - reach + 1 < oldest {
                // Fallen behind what's kept: skip to what's there.
                let behind = (oldest - (base - reach + 1)) as f64 / self.step;
                let frames = behind.ceil() as u64;
                self.flush(writer);
                writer.append_silence(frames);
                self.next_w += frames;
                self.next_x += frames as f64 * self.step;
                continue;
            }
            let first = self.resampler.weights(self.next_x, &mut self.weights);
            let out = &mut self.out[self.pending * channels..(self.pending + 1) * channels];
            out.fill(0.0);
            for (j, &weight) in self.weights.iter().enumerate() {
                let i = ((first + j as i64).max(0) as usize) % HISTORY;
                let frame = &self.history[i * channels..(i + 1) * channels];
                for (sum, &sample) in out.iter_mut().zip(frame) {
                    *sum += weight * sample;
                }
            }
            self.pending += 1;
            self.next_w += 1;
            self.next_x += self.step;
            if self.pending == CHUNK {
                self.flush(writer);
            }
        }
        self.flush(writer);
    }

    fn flush(&mut self, writer: &mut InputWriter) {
        if self.pending > 0 {
            writer.append(&self.out[..self.pending * self.channels]);
            self.pending = 0;
        }
    }
}
