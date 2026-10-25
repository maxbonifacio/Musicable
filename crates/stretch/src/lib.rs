//! Signalsmith Stretch: the time-stretch and pitch-shift that warped audio's
//! Spectral mode renders with, and plays live while a render isn't ready;
//! and Rubber Band Library's R3 engine, which Spectral renders with while
//! High quality is on ([`RubberBand`], in [`rubber_band`]).
//!
//! Signalsmith Stretch is header-only C++ by Signalsmith Audio, under the
//! MIT licence, built by `build.rs` with Musicable's C interface to it
//! (`src/stretch.cpp`) and bound here by hand, so the build needs no LLVM.
//! `vendor/README.md` says where it came from.
//!
//! A [`Stretch`] is a phase vocoder. Every [interval](Stretch::interval) of
//! output, it analyses a [block](Stretch::block) of its input, moves the
//! spectrum's frequencies by its transpose, and adds the spectrum back into
//! its output, so the output plays the input faster or slower as the counts
//! of frames each [`process`](Stretch::process) is handed say, at the pitch
//! the transpose gives. Each spectrum is of the input from
//! [`input_latency`](Stretch::input_latency) frames before the last frame
//! written to it, and is heard [`output_latency`](Stretch::output_latency)
//! frames after the frame output then: the input is read ahead of where the
//! output plays it, and comes out behind.
//!
//! A stretcher is seeded, so the same calls to a new one give the same
//! audio, bit for bit, on the same build: it draws random phases where it
//! stretches past twice as slow. [`process`](Stretch::process) and [`seek`](Stretch::seek)
//! don't allocate, so they can run on the audio thread.

use std::ffi::c_int;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

pub mod rubber_band;

pub use rubber_band::{RUBBER_BAND_VERSION, RubberBand};

/// Changes when what a stretcher renders from the same calls may change: a
/// new release of the vendored code, or a new preset. The render cache keys
/// its segments with it, so they're rendered again.
pub const VERSION: u32 = 1;

/// The most channels a stretcher takes.
pub const MAX_CHANNELS: usize = 8;

/// The pitch, in Hz, a stretcher takes a voice to be on as it measures the
/// formants: it smooths the outline of the spectrum over about that much,
/// so the outline follows the formants and not each harmonic. Not given
/// one, Signalsmith Stretch guesses the pitch from the spectrum's highest
/// peaks, roughly, as its README says, and for some voices the guess comes
/// to almost nothing: the outline then dips between the harmonics, and a
/// transposed voice came out as much as 27 dB quieter. On two vowels sung
/// with vibrato and moved 2 and 5 semitones each way, 250 Hz undid two
/// thirds of the change that moving the formants with the pitch makes to
/// the outline of a voice on 100 to 220 Hz, and half of it from 330 to 600
/// Hz, where the voice also came out 3 to 5 dB quieter. A lower pitch keeps
/// more of a low voice's formants and leaves a high voice quieter still.
const FORMANT_BASE: f64 = 250.0;

mod ffi {
    use std::ffi::c_int;

    /// A stretcher, as `src/stretch.cpp` makes it.
    #[repr(C)]
    pub(crate) struct RawStretch {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        pub(crate) fn musicable_stretch_new(
            channels: c_int,
            sample_rate: f32,
            seed: u32,
        ) -> *mut RawStretch;
        pub(crate) fn musicable_stretch_free(stretch: *mut RawStretch);
        pub(crate) fn musicable_stretch_block_samples(stretch: *const RawStretch) -> c_int;
        pub(crate) fn musicable_stretch_interval_samples(stretch: *const RawStretch) -> c_int;
        pub(crate) fn musicable_stretch_input_latency(stretch: *const RawStretch) -> c_int;
        pub(crate) fn musicable_stretch_output_latency(stretch: *const RawStretch) -> c_int;
        pub(crate) fn musicable_stretch_reset(stretch: *mut RawStretch);
        pub(crate) fn musicable_stretch_set_transpose(stretch: *mut RawStretch, factor: f32);
        pub(crate) fn musicable_stretch_set_formants(
            stretch: *mut RawStretch,
            factor: f32,
            compensate_pitch: c_int,
        );
        pub(crate) fn musicable_stretch_set_formant_base(stretch: *mut RawStretch, frequency: f32);
        pub(crate) fn musicable_stretch_seek(
            stretch: *mut RawStretch,
            inputs: *const *const f32,
            frames: c_int,
            rate: f64,
        ) -> c_int;
        pub(crate) fn musicable_stretch_process(
            stretch: *mut RawStretch,
            inputs: *const *const f32,
            input_frames: c_int,
            outputs: *const *mut f32,
            output_frames: c_int,
        ) -> c_int;
        pub(crate) fn musicable_stretch_process_tail(
            stretch: *mut RawStretch,
            tails: *const *const f32,
            tail_frames: c_int,
            input_frames: c_int,
            outputs: *const *mut f32,
            output_frames: c_int,
        ) -> c_int;
    }
}

/// One Signalsmith Stretch, set up with its default preset: blocks of 120
/// ms, a spectrum every 30 ms.
pub struct Stretch {
    raw: NonNull<ffi::RawStretch>,
    channels: usize,
}

// SAFETY: a stretcher holds nothing tied to the thread that made it, and
// `&mut self` keeps two threads from using it at once.
unsafe impl Send for Stretch {}

impl Stretch {
    /// A stretcher of `channels` channels at `sample_rate`, drawing its
    /// random phases from `seed`. Allocates.
    ///
    /// # Panics
    ///
    /// With no channels or more than [`MAX_CHANNELS`], no sample rate, or
    /// no memory for it.
    pub fn new(channels: usize, sample_rate: u32, seed: u32) -> Self {
        assert!(
            (1..=MAX_CHANNELS).contains(&channels),
            "a stretcher has 1 to {MAX_CHANNELS} channels, not {channels}"
        );
        assert!(sample_rate > 0, "a stretcher needs a sample rate");
        // SAFETY: the interface takes any channel count and rate, and gives
        // back null if it can't allocate.
        let raw =
            unsafe { ffi::musicable_stretch_new(channels as c_int, sample_rate as f32, seed) };
        let raw = NonNull::new(raw).expect("no memory for a stretcher");
        let base = FORMANT_BASE / f64::from(sample_rate);
        // SAFETY: `raw` is a live stretcher, and this its only user.
        unsafe { ffi::musicable_stretch_set_formant_base(raw.as_ptr(), base as f32) };
        Self { raw, channels }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// How many frames of input each spectrum analyses.
    pub fn block(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_stretch_block_samples(self.raw.as_ptr()) as usize }
    }

    /// How many frames of output come between two spectra.
    pub fn interval(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_stretch_interval_samples(self.raw.as_ptr()) as usize }
    }

    /// How far before the last frame of input written the input a spectrum
    /// analyses is centred, in frames.
    pub fn input_latency(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_stretch_input_latency(self.raw.as_ptr()) as usize }
    }

    /// How many frames after it's analysed a spectrum is heard in the
    /// output, at its centre.
    pub fn output_latency(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_stretch_output_latency(self.raw.as_ptr()) as usize }
    }

    /// Forgets the input and output so far, keeping the transpose and the
    /// formants. A stretcher reset doesn't render quite as a new one does,
    /// so a render that must come out the same every time takes a new one.
    pub fn reset(&mut self) {
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_stretch_reset(self.raw.as_ptr()) }
    }

    /// Moves the pitch: `factor` times each frequency, so 2 is an octave
    /// up. It applies from the next spectrum.
    pub fn set_transpose(&mut self, factor: f64) {
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_stretch_set_transpose(self.raw.as_ptr(), factor as f32) }
    }

    /// Moves the formants, the peaks of the spectrum's outline that give a
    /// voice its character, by `factor`. With `compensate_pitch`, they move
    /// that far from where they were before the transpose, so 1 keeps them
    /// in place as the pitch moves; without, from where the transpose puts
    /// them.
    pub fn set_formants(&mut self, factor: f64, compensate_pitch: bool) {
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe {
            ffi::musicable_stretch_set_formants(
                self.raw.as_ptr(),
                factor as f32,
                c_int::from(compensate_pitch),
            )
        }
    }

    /// Starts again from new input, a channel to each slice: the input that
    /// comes before where the next spectrum is, ideally a block and an
    /// interval of it, at `rate` times as fast as the output plays it. Only
    /// the last block and interval count. Doesn't allocate.
    ///
    /// # Panics
    ///
    /// If `input` doesn't have a slice for each channel, all as long.
    pub fn seek(&mut self, input: &[&[f32]], rate: f64) {
        let (inputs, frames) = pointers(input, self.channels);
        // SAFETY: each pointer is to a slice of `frames` frames, which the
        // stretcher only reads.
        let done = unsafe {
            ffi::musicable_stretch_seek(self.raw.as_ptr(), inputs.as_ptr(), frames as c_int, rate)
        };
        assert!(done != 0, "the stretcher failed to seek");
    }

    /// Writes `input`'s frames and renders `output`'s, a channel to each
    /// slice, so the input plays at the output's length. A new spectrum
    /// comes every [interval](Self::interval) of output, from the input up
    /// to as far into this input as the output is into its frames. Doesn't
    /// allocate.
    ///
    /// # Panics
    ///
    /// If `input` or `output` doesn't have a slice for each channel, all as
    /// long.
    pub fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]]) {
        let (inputs, input_frames) = pointers(input, self.channels);
        assert_eq!(output.len(), self.channels, "a slice for each channel");
        let output_frames = output.first().map_or(0, |c| c.len());
        assert!(
            output.iter().all(|c| c.len() == output_frames),
            "the channels are all as long"
        );
        let mut outputs = [std::ptr::null_mut(); MAX_CHANNELS];
        for (pointer, channel) in outputs.iter_mut().zip(output.iter_mut()) {
            *pointer = channel.as_mut_ptr();
        }
        // SAFETY: each input pointer is to a slice of `input_frames` frames,
        // which the stretcher only reads, and each output pointer to one of
        // `output_frames` that only it writes while this runs.
        let done = unsafe {
            ffi::musicable_stretch_process(
                self.raw.as_ptr(),
                inputs.as_ptr(),
                input_frames as c_int,
                outputs.as_ptr(),
                output_frames as c_int,
            )
        };
        assert!(done != 0, "the stretcher failed to render");
    }

    /// As [`process`](Self::process), for `frames` frames of input of which
    /// only the last are given, `tail`'s, the ones before reading as
    /// silence. A stretcher keeps only the last [block](Self::block) and
    /// [interval](Self::interval) of a call's input, so a call that plays
    /// far more input than that, as one that plays its file many times as
    /// fast does, needs only those: what came before is never heard, and
    /// only counts towards whether the call's input is silent. Doesn't
    /// allocate.
    ///
    /// # Panics
    ///
    /// If `tail` or `output` doesn't have a slice for each channel, all as
    /// long, or `tail` is longer than `frames`.
    pub fn process_tail(&mut self, tail: &[&[f32]], frames: usize, output: &mut [&mut [f32]]) {
        let (tails, tail_frames) = pointers(tail, self.channels);
        assert!(tail_frames <= frames, "the tail is of the input");
        assert!(frames <= c_int::MAX as usize, "too many frames at once");
        assert_eq!(output.len(), self.channels, "a slice for each channel");
        let output_frames = output.first().map_or(0, |c| c.len());
        assert!(
            output.iter().all(|c| c.len() == output_frames),
            "the channels are all as long"
        );
        let mut outputs = [std::ptr::null_mut(); MAX_CHANNELS];
        for (pointer, channel) in outputs.iter_mut().zip(output.iter_mut()) {
            *pointer = channel.as_mut_ptr();
        }
        // SAFETY: each tail pointer is to a slice of `tail_frames` frames,
        // which the stretcher only reads, through an accessor that reads the
        // frames before them as silence, and each output pointer to one of
        // `output_frames` that only it writes while this runs.
        let done = unsafe {
            ffi::musicable_stretch_process_tail(
                self.raw.as_ptr(),
                tails.as_ptr(),
                tail_frames as c_int,
                frames as c_int,
                outputs.as_ptr(),
                output_frames as c_int,
            )
        };
        assert!(done != 0, "the stretcher failed to render");
    }

    /// Its block, interval and latencies.
    pub fn sizes(&self) -> Sizes {
        Sizes {
            block: self.block(),
            interval: self.interval(),
            input_latency: self.input_latency(),
            output_latency: self.output_latency(),
        }
    }
}

/// A stretcher's sizes, in frames: see [`Stretch::block`],
/// [`Stretch::interval`], [`Stretch::input_latency`] and
/// [`Stretch::output_latency`]. They follow from the sample rate alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sizes {
    pub block: usize,
    pub interval: usize,
    pub input_latency: usize,
    pub output_latency: usize,
}

/// The sizes of a stretcher at `sample_rate`, worked out with one the first
/// time a rate is asked for.
///
/// # Panics
///
/// With no sample rate.
pub fn sizes(sample_rate: u32) -> Sizes {
    static KNOWN: Mutex<Vec<(u32, Sizes)>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&(_, sizes)) = known.iter().find(|(rate, _)| *rate == sample_rate) {
        return sizes;
    }
    let sizes = Stretch::new(1, sample_rate, 0).sizes();
    known.push((sample_rate, sizes));
    sizes
}

impl Drop for Stretch {
    fn drop(&mut self) {
        // SAFETY: `raw` is a live stretcher, freed once.
        unsafe { ffi::musicable_stretch_free(self.raw.as_ptr()) }
    }
}

impl std::fmt::Debug for Stretch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stretch")
            .field("channels", &self.channels)
            .field("block", &self.block())
            .field("interval", &self.interval())
            .finish()
    }
}

/// A pointer to each of `input`'s slices, one for each of `channels`, and
/// how long they are.
fn pointers(input: &[&[f32]], channels: usize) -> ([*const f32; MAX_CHANNELS], usize) {
    assert_eq!(input.len(), channels, "a slice for each channel");
    let frames = input.first().map_or(0, |c| c.len());
    assert!(
        input.iter().all(|c| c.len() == frames),
        "the channels are all as long"
    );
    assert!(frames <= c_int::MAX as usize, "too many frames at once");
    let mut pointers = [std::ptr::null(); MAX_CHANNELS];
    for (pointer, channel) in pointers.iter_mut().zip(input) {
        *pointer = channel.as_ptr();
    }
    (pointers, frames)
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    const RATE: u32 = 44_100;

    fn sine(frequency: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| (TAU * frequency * n as f64 / f64::from(RATE)).sin() as f32 * 0.5)
            .collect()
    }

    /// Noise from a seed, the same every time.
    fn noise(frames: usize, mut seed: u32) -> Vec<f32> {
        (0..frames)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    /// `input`, mono, through `stretch` a `chunk` of input at a time, with
    /// `ratio` times as many frames out.
    fn stretched(stretch: &mut Stretch, input: &[f32], chunk: usize, ratio: f64) -> Vec<f32> {
        let mut output = Vec::new();
        let mut out = vec![0.0; (chunk as f64 * ratio).ceil() as usize + 1];
        let mut written = 0;
        for piece in input.chunks(chunk) {
            let wanted = ((written + piece.len()) as f64 * ratio).round() as usize - output.len();
            let out = &mut out[..wanted];
            stretch.process(&[piece], &mut [out]);
            output.extend_from_slice(out);
            written += piece.len();
        }
        output
    }

    /// How often `signal` crosses zero upwards, in Hz.
    fn frequency(signal: &[f32]) -> f64 {
        let ups = signal.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0);
        ups.count() as f64 * f64::from(RATE) / signal.len() as f64
    }

    fn level(signal: &[f32]) -> f64 {
        let power: f64 = signal.iter().map(|&s| f64::from(s).powi(2)).sum();
        (power / signal.len() as f64).sqrt()
    }

    /// A voice on 150 Hz at `rate`, its harmonics shaped by a formant at 1
    /// kHz.
    fn voice(rate: u32, frames: usize) -> Vec<f32> {
        let sample = |n: usize| {
            let t = n as f64 / f64::from(rate);
            let harmonic = |k: u32| {
                let f = 150.0 * f64::from(k);
                let shape = 1.0 / (1.0 + ((f - 1000.0) / 150.0).powi(2));
                0.05 * shape * (TAU * f * t).sin()
            };
            (1..=26).map(harmonic).sum::<f64>() as f32
        };
        (0..frames).map(sample).collect()
    }

    #[test]
    fn its_preset_has_blocks_of_120_ms_and_a_spectrum_every_30() {
        let stretch = Stretch::new(2, RATE, 1);
        assert_eq!((stretch.block(), stretch.interval()), (5292, 1323));
        assert_eq!(
            (stretch.input_latency(), stretch.output_latency()),
            (2646, 2646)
        );
        assert_eq!(stretch.channels(), 2);
    }

    #[test]
    fn at_its_own_speed_a_sine_comes_out_as_it_went_in_behind_the_latency() {
        let mut stretch = Stretch::new(1, RATE, 1);
        let input = sine(1000.0, RATE as usize * 2);
        let output = stretched(&mut stretch, &input, 512, 1.0);
        let behind = stretch.input_latency() + stretch.output_latency();
        // Once the first blocks are in, it matches the input from that far
        // back to within a small fraction of its level.
        let settled = behind + stretch.block();
        let error = (settled..output.len())
            .map(|n| (output[n] - input[n - behind]).abs())
            .fold(0.0, f32::max);
        assert!(error < 0.02, "{error}");
    }

    #[test]
    fn transposed_an_octave_up_a_sine_comes_out_at_twice_its_frequency() {
        let mut stretch = Stretch::new(1, RATE, 1);
        stretch.set_transpose(2.0);
        let input = sine(440.0, RATE as usize * 2);
        let output = stretched(&mut stretch, &input, 512, 1.0);
        let heard = frequency(&output[RATE as usize / 2..]);
        assert!((heard - 880.0).abs() < 3.0, "{heard}");
        assert_eq!(output.len(), input.len(), "as long as it was");
    }

    #[test]
    fn transposed_with_its_formants_in_place_a_voice_keeps_its_level() {
        // At 48 kHz, where Signalsmith Stretch's own guess at this voice's
        // pitch comes to almost nothing.
        let rate = 48_000;
        let second = rate as usize;
        let input = voice(rate, second * 3);
        let heard = level(&input[second..]);
        // Down 5 and 1 semitones, then up, in dB from the input's level.
        let levels = [-5.0, -1.0, 1.0, 5.0].map(|semitones: f64| {
            let mut stretch = Stretch::new(1, rate, 1);
            stretch.set_transpose(2f64.powf(semitones / 12.0));
            stretch.set_formants(1.0, true);
            let output = stretched(&mut stretch, &input, 512, 1.0);
            20.0 * (level(&output[second..]) / heard).log10()
        });
        assert!(levels.iter().all(|db| db.abs() < 4.0), "{levels:.1?}");
    }

    #[test]
    fn more_frames_out_than_in_play_the_input_slower_at_its_pitch() {
        let mut stretch = Stretch::new(1, RATE, 1);
        let input = sine(440.0, RATE as usize * 2);
        let output = stretched(&mut stretch, &input, 512, 1.25);
        assert_eq!(output.len(), input.len() * 5 / 4);
        let heard = frequency(&output[RATE as usize / 2..]);
        assert!((heard - 440.0).abs() < 3.0, "{heard}");
    }

    #[test]
    fn the_same_calls_with_the_same_seed_give_the_same_audio() {
        let input = noise(RATE as usize, 7);
        // Three times as slow, where it draws random phases.
        let render = |seed| stretched(&mut Stretch::new(1, RATE, seed), &input, 300, 3.0);
        let (first, again, other) = (render(5), render(5), render(6));
        assert_eq!(first, again);
        assert_ne!(first, other, "the seed is used");
    }

    #[test]
    fn a_call_needs_only_the_end_of_its_input() {
        // Five times as fast: each call plays more input than it keeps.
        let input = noise(RATE as usize * 2, 3);
        let (mut whole, mut tail) = (Stretch::new(1, RATE, 1), Stretch::new(1, RATE, 1));
        let (interval, keep) = (whole.interval(), whole.block() + whole.interval());
        let call = 5 * interval;
        let (mut a, mut b) = (vec![0.0; interval], vec![0.0; interval]);
        for piece in input.chunks_exact(call) {
            whole.process(&[piece], &mut [&mut a]);
            tail.process_tail(&[&piece[call - keep..]], call, &mut [&mut b]);
            assert_eq!(a, b);
        }
        assert_eq!(tail.sizes().block + tail.sizes().interval, keep);
        assert_eq!(sizes(RATE), whole.sizes());
    }

    #[test]
    fn a_seek_starts_it_from_the_input_given_without_a_fade_in() {
        // Seeking with the input up to the latency before the start, so the
        // first spectrum is centred on the input's first frame: its sine
        // comes out at full level as soon as it's heard.
        let mut stretch = Stretch::new(2, RATE, 1);
        let input = sine(500.0, RATE as usize);
        let before = vec![0.0; stretch.block()];
        let lead = [&before[..], &input[..stretch.input_latency()]].concat();
        let lead = &lead[lead.len() - stretch.block() - stretch.interval()..];
        stretch.seek(&[lead, lead], 1.0);
        let rest = &input[stretch.input_latency()..];
        let (mut left, mut right) = (vec![0.0; rest.len()], vec![0.0; rest.len()]);
        stretch.process(&[rest, rest], &mut [&mut left, &mut right]);
        let latency = stretch.output_latency();
        let level = |s: &[f32]| s.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
        let first = level(&left[latency..latency + 400]);
        assert!(first > 0.45, "{first}");
        // Both channels alike, to a rounding error.
        let apart = left.iter().zip(&right).map(|(l, r)| (l - r).abs());
        assert!(apart.fold(0.0, f32::max) < 1e-5);
    }
}
