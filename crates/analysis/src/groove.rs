//! A synthetic groove with known onsets, beats and downbeats, for testing
//! the analysis: kicks, snares, eighth-note hats and quiet ghost snares
//! over a three-note pad with vibrato, as in the onset research.

use std::f64::consts::{PI, TAU};

pub const SAMPLE_RATE: u32 = 44_100;
const RATE: f64 = SAMPLE_RATE as f64;

/// A seeded random number generator, the same everywhere.
pub struct Lcg(pub u64);

impl Lcg {
    /// From −1 to 1.
    pub fn next(&mut self) -> f64 {
        self.0 = (self.0)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    /// From 0 to 1.
    pub fn unit(&mut self) -> f64 {
        (self.next() + 1.0) * 0.5
    }
}

/// A fade over the last 30% of a hit, so its end doesn't click.
fn tail(k: usize, length: usize) -> f64 {
    let start = length as f64 * 0.7;
    if (k as f64) < start {
        1.0
    } else {
        0.5 + 0.5 * (PI * (k as f64 - start) / (length as f64 - start)).cos()
    }
}

pub fn kick(x: &mut [f64], at: usize, gain: f64) {
    let length = (0.4 * RATE) as usize;
    let mut phase = 0.0;
    for k in 0..length.min(x.len().saturating_sub(at)) {
        let t = k as f64 / RATE;
        let hz = 50.0 + 110.0 * (-t / 0.03).exp();
        phase += TAU * hz / RATE;
        x[at + k] += gain * 0.9 * phase.sin() * (-t / 0.15).exp() * tail(k, length);
    }
}

fn snare(x: &mut [f64], at: usize, gain: f64, random: &mut Lcg) {
    let length = (0.3 * RATE) as usize;
    let mut phase = 0.0;
    for k in 0..length.min(x.len().saturating_sub(at)) {
        let t = k as f64 / RATE;
        phase += TAU * 190.0 / RATE;
        let noise = 0.5 * random.next() * (-t / 0.08).exp();
        let body = 0.4 * phase.sin() * (-t / 0.05).exp();
        x[at + k] += gain * (noise + body) * tail(k, length);
    }
}

fn hat(x: &mut [f64], at: usize, gain: f64, random: &mut Lcg) {
    let length = (0.08 * RATE) as usize;
    let mut previous = 0.0;
    for k in 0..length.min(x.len().saturating_sub(at)) {
        let t = k as f64 / RATE;
        let noise = random.next();
        x[at + k] += gain * 0.35 * (noise - previous) * (-t / 0.015).exp() * tail(k, length);
        previous = noise;
    }
}

/// How the groove moves in time.
#[derive(Debug, Clone, Copy)]
pub struct Groove {
    pub bpm: f64,
    /// The tempo swings by this fraction either way, as a sine with this
    /// period, in seconds.
    pub drift: f64,
    pub drift_period: f64,
    /// Each hit lands up to this many milliseconds early or late.
    pub jitter_ms: f64,
    /// When the first beat falls, in seconds.
    pub start: f64,
}

impl Groove {
    pub fn steady(bpm: f64) -> Self {
        Self {
            bpm,
            drift: 0.0,
            drift_period: 60.0,
            jitter_ms: 0.0,
            start: 0.5,
        }
    }
}

pub struct Song {
    pub audio: Vec<f32>,
    /// The sample of every hit, as played.
    pub onsets: Vec<usize>,
    /// Where each beat and each bar's first beat fall, in samples.
    pub beats: Vec<f64>,
    pub downbeats: Vec<f64>,
}

/// A groove in 4/4, `seconds` long, with random velocities, a second
/// kick in every other bar, and ghost snares about 20 dB down.
pub fn synth(seconds: f64, groove: &Groove) -> Song {
    let n = (seconds * RATE) as usize;
    let mut x = vec![0f64; n];
    let mut random = Lcg(0x5eed);
    let chord = [220.0, 277.18, 329.63];
    let mut phases = [0.0f64; 3];
    for (i, sample) in x.iter_mut().enumerate() {
        let t = i as f64 / RATE;
        let vibrato = 1.0 + 0.006 * (TAU * 5.5 * t).sin();
        for (phase, hz) in phases.iter_mut().zip(chord) {
            *phase += TAU * hz * vibrato / RATE;
            *sample += 0.06 * phase.sin();
        }
    }
    let mut onsets = Vec::new();
    let (mut beats, mut downbeats) = (Vec::new(), Vec::new());
    let mut jitter = Lcg(0x7177e5);
    let mut t = groove.start;
    let mut step = 0usize;
    while t < seconds - 1.0 {
        if step.is_multiple_of(4) {
            beats.push(t * RATE);
        }
        if step.is_multiple_of(16) {
            downbeats.push(t * RATE);
        }
        let played = t + jitter.next() * groove.jitter_ms / 1000.0;
        let at = (played * RATE).round().max(0.0) as usize;
        let place = step % 16;
        let mut hit = false;
        if place == 0 || place == 8 || (place == 10 && step % 32 >= 16) {
            kick(&mut x, at, 0.6 + 0.4 * random.unit());
            hit = true;
        }
        if place == 4 || place == 12 {
            snare(&mut x, at, 0.6 + 0.4 * random.unit(), &mut random);
            hit = true;
        } else if (place == 7 || place == 15) && random.unit() < 0.5 {
            snare(&mut x, at, 0.08 + 0.05 * random.unit(), &mut random);
            hit = true;
        }
        if place.is_multiple_of(2) {
            let accent = if place.is_multiple_of(4) { 0.8 } else { 0.45 };
            hat(
                &mut x,
                at,
                accent * (0.7 + 0.3 * random.unit()),
                &mut random,
            );
            hit = true;
        }
        if hit {
            onsets.push(at);
        }
        let bpm = groove.bpm * (1.0 + groove.drift * (TAU * t / groove.drift_period).sin());
        t += 60.0 / bpm / 4.0;
        step += 1;
    }
    Song {
        audio: x.iter().map(|&v| v as f32).collect(),
        onsets,
        beats,
        downbeats,
    }
}
