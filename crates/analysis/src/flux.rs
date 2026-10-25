//! Log-filtered spectral flux with SuperFlux's vibrato suppression (Böck
//! and Widmer, 2013): how much the spectrum rises from one 10 ms frame to
//! the next. It peaks where a sound starts, so the onsets are its peaks,
//! and the tempo and the classic beat tracker come from its rhythm.

use std::sync::Arc;

use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};

/// Frames a second, about: the hop is the sample rate over this, rounded.
const FRAMES_A_SECOND: f64 = 100.0;
/// The window's length, about: 2048 samples at 44.1 kHz.
const WINDOW_SECONDS: f64 = 0.0464;
/// The filters, 12 to an octave, start at A0 and end at 16 kHz.
const LOWEST: f64 = 27.5;
const HIGHEST: f64 = 16_000.0;
/// Where the three bands meet: below 200 Hz, 200 Hz to 4 kHz, and above.
/// An onset is found again in the band its flux rose most in.
pub(crate) const CROSSOVERS: [f64; 2] = [200.0, 4_000.0];

/// The spectral flux of a sound, a value for each frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Flux {
    pub sample_rate: u32,
    /// Samples from one frame to the next: about 10 ms.
    pub hop: usize,
    /// The flux of each frame, frame `n` centred on sample `n × hop`. The
    /// first frame's is its rise from silence.
    pub values: Vec<f32>,
    /// How much of each frame's flux rose in each band.
    pub bands: Vec<[f32; 3]>,
    /// Each frame's RMS level, through its window.
    pub levels: Vec<f32>,
}

impl Flux {
    /// Frames a second.
    pub fn rate(&self) -> f64 {
        f64::from(self.sample_rate) / self.hop as f64
    }

    /// The sample frame `n` is centred on.
    pub fn sample_of(&self, n: usize) -> u64 {
        (n * self.hop) as u64
    }

    /// The band frame `n`'s flux rose most in: 0 low, 1 middle, 2 high.
    pub(crate) fn band_of(&self, n: usize) -> usize {
        let [low, mid, high] = self.bands[n];
        if high >= mid && high >= low {
            2
        } else if mid >= low {
            1
        } else {
            0
        }
    }
}

/// The hop and window for `sample_rate`: 441 and 2048 at 44.1 kHz.
pub(crate) fn framing(sample_rate: u32) -> (usize, usize) {
    let rate = f64::from(sample_rate);
    let hop = (rate / FRAMES_A_SECOND).round().max(1.0) as usize;
    // The power of two nearest the window's length.
    let window = 2f64.powf((rate * WINDOW_SECONDS).log2().round()).max(64.0) as usize;
    (hop, window)
}

/// A triangular filter: the bins it sums, with their weights.
struct Filter {
    bins: Vec<(usize, f32)>,
    band: usize,
}

/// Triangular filters, 12 to an octave from A0 to 16 kHz, each from the
/// centre below to the one above. Where the bins are too coarse for that,
/// at the bottom, filters merge (Böck, Krebs and Schedl, 2012).
fn filters(sample_rate: u32, window: usize) -> Vec<Filter> {
    let bins = window / 2 + 1;
    let rate = f64::from(sample_rate);
    let bin_of = |hz: f64| ((hz * window as f64 / rate).round() as usize).min(bins - 1);
    let mut centres = Vec::new();
    let mut hz = LOWEST;
    while hz <= HIGHEST {
        centres.push(bin_of(hz));
        hz *= 2f64.powf(1.0 / 12.0);
    }
    centres.dedup();
    centres
        .windows(3)
        .map(|w| {
            let (low, centre, high) = (w[0], w[1], w[2]);
            let bins = (low..=high)
                .filter_map(|bin| {
                    let weight = if bin <= centre {
                        if centre == low {
                            1.0
                        } else {
                            (bin - low) as f64 / (centre - low) as f64
                        }
                    } else {
                        (high - bin) as f64 / (high - centre) as f64
                    };
                    (weight > 0.0).then_some((bin, weight as f32))
                })
                .collect();
            let hz = centre as f64 * rate / window as f64;
            let band = CROSSOVERS
                .iter()
                .filter(|&&crossover| hz >= crossover)
                .count();
            Filter { bins, band }
        })
        .collect()
}

/// Works out a sound's flux as its samples come, so a long file needn't be
/// in memory at once.
pub(crate) struct Spectrum {
    fft: Arc<dyn RealToComplex<f32>>,
    hop: usize,
    window: Vec<f32>,
    /// The window's power, which its RMS levels are relative to.
    power: f32,
    filters: Vec<Filter>,
    /// Scales the magnitudes so they come out as a 2048-sample window's
    /// would, whatever the window.
    scale: f32,
    /// Samples not yet framed for good. `pending[0]` is the sound's sample
    /// `first`, which is negative for the silence before it.
    pending: Vec<f32>,
    first: i64,
    /// Samples pushed.
    pushed: u64,
    input: Vec<f32>,
    output: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// The last frame's filtered spectrum, and this one's.
    previous: Vec<f32>,
    current: Vec<f32>,
    flux: Flux,
}

impl Spectrum {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let (hop, size) = framing(sample_rate);
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let window: Vec<f32> = (0..size)
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / size as f64).cos()) as f32)
            .collect();
        let power = window.iter().map(|w| w * w).sum();
        let filters = filters(sample_rate, size);
        let bands = filters.len();
        Self {
            input: fft.make_input_vec(),
            output: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            hop,
            window,
            power,
            filters,
            scale: 2048.0 / size as f32,
            pending: vec![0.0; size / 2],
            first: -((size / 2) as i64),
            pushed: 0,
            previous: vec![0.0; bands],
            current: vec![0.0; bands],
            flux: Flux {
                sample_rate,
                hop,
                values: Vec::new(),
                bands: Vec::new(),
                levels: Vec::new(),
            },
        }
    }

    /// Takes the sound's next samples.
    pub(crate) fn push(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        self.pushed += samples.len() as u64;
        self.frame_what_it_can();
    }

    /// The flux, once every sample has come: a frame for every hop, and one
    /// more.
    pub(crate) fn finish(mut self) -> Flux {
        let frames = (self.pushed / self.hop as u64 + 1) as usize;
        let size = self.window.len();
        while self.flux.values.len() < frames {
            // Silence after the end, for the frames that reach past it.
            self.pending.resize(self.pending.len() + size, 0.0);
            self.frame_what_it_can();
        }
        self.flux.values.truncate(frames);
        self.flux.bands.truncate(frames);
        self.flux.levels.truncate(frames);
        self.flux
    }

    fn frame_what_it_can(&mut self) {
        let size = self.window.len();
        let mut done = 0;
        loop {
            let n = self.flux.values.len();
            let centre = (n * self.hop) as i64;
            let start = (centre - (size / 2) as i64 - self.first) as usize;
            if start + size > self.pending.len() {
                break;
            }
            self.frame(start);
            done = start + self.hop;
        }
        // What the next frame needn't keep.
        let done = done.min(self.pending.len());
        self.pending.drain(..done);
        self.first += done as i64;
    }

    /// Works out the frame whose window starts at `pending[start]`.
    fn frame(&mut self, start: usize) {
        let samples = &self.pending[start..start + self.window.len()];
        let mut energy = 0.0;
        for ((input, sample), weight) in self.input.iter_mut().zip(samples).zip(&self.window) {
            *input = sample * weight;
            energy += *input * *input;
        }
        self.flux.levels.push((energy / self.power).sqrt());
        // Every buffer has the length the plan asked for.
        let _ =
            (self.fft).process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch);
        for (filter, value) in self.filters.iter().zip(&mut self.current) {
            let sum: f32 = (filter.bins.iter())
                .map(|&(bin, weight)| self.output[bin].norm() * weight)
                .sum();
            *value = (1.0 + sum * self.scale).log10();
        }
        // SuperFlux: each band against the loudest of it and its neighbours
        // in the frame before, so vibrato doesn't read as a rise.
        let mut flux = 0.0;
        let mut bands = [0.0; 3];
        let last = self.current.len().saturating_sub(1);
        for (b, (&value, filter)) in self.current.iter().zip(&self.filters).enumerate() {
            let before = self.previous[b.saturating_sub(1)..=(b + 1).min(last)]
                .iter()
                .fold(f32::MIN, |a, &b| a.max(b));
            let rise = (value - before).max(0.0);
            flux += rise;
            bands[filter.band] += rise;
        }
        self.flux.values.push(flux);
        self.flux.bands.push(bands);
        std::mem::swap(&mut self.previous, &mut self.current);
    }
}

/// The flux of `samples`, all at once.
#[cfg(test)]
pub(crate) fn flux_of(samples: &[f32], sample_rate: u32) -> Flux {
    let mut spectrum = Spectrum::new(sample_rate);
    spectrum.push(samples);
    spectrum.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_about_10_ms_and_windows_about_46() {
        assert_eq!(framing(44_100), (441, 2048));
        assert_eq!(framing(48_000), (480, 2048));
        assert_eq!(framing(96_000), (960, 4096));
        assert_eq!(framing(22_050), (221, 1024));
    }

    #[test]
    fn the_filters_split_into_three_bands() {
        let filters = filters(44_100, 2048);
        assert!(filters.len() > 70, "{} filters", filters.len());
        let bands: Vec<usize> = filters.iter().map(|f| f.band).collect();
        assert!(bands.is_sorted());
        assert_eq!((bands[0], bands[bands.len() - 1]), (0, 2));
        assert!(bands.contains(&1));
    }

    #[test]
    fn however_the_samples_come_the_flux_is_the_same() {
        let mut noise = 1u32;
        let samples: Vec<f32> = (0..30_000)
            .map(|i| {
                noise = noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let burst = if (i / 4410) % 2 == 0 { 0.5 } else { 0.0 };
                burst * (noise as f32 / u32::MAX as f32 - 0.5)
            })
            .collect();
        let whole = flux_of(&samples, 44_100);
        assert_eq!(whole.values.len(), 30_000 / 441 + 1);
        let mut spectrum = Spectrum::new(44_100);
        for chunk in samples.chunks(1000) {
            spectrum.push(chunk);
        }
        assert_eq!(spectrum.finish(), whole);
    }

    #[test]
    fn a_sound_starting_rises_from_the_silence_before() {
        let mut samples = vec![0.0f32; 44_100];
        for (i, sample) in samples.iter_mut().enumerate().skip(22_050) {
            *sample = 0.5 * (i as f32 * 0.1).sin();
        }
        let flux = flux_of(&samples, 44_100);
        let peak = (0..flux.values.len())
            .max_by(|&a, &b| flux.values[a].total_cmp(&flux.values[b]))
            .unwrap();
        // The tone starts at frame 50; its window reaches it a little before.
        assert!((47..=50).contains(&peak), "peak at frame {peak}");
        assert!(flux.values[..40].iter().all(|&v| v == 0.0));
        // A steady tone, once it's in the whole window, doesn't rise.
        assert!(
            flux.values[60..90]
                .iter()
                .all(|&v| v < 1e-3 * flux.values[peak])
        );
    }
}
