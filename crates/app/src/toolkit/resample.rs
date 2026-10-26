//! Changes audio's sample rate for `audio.convert`, as it streams through:
//! rubato's FFT resampler, flat to 21 kHz from 44.1 kHz, whose aliasing
//! is far below what 64-bit floats hold. Its delay is cut off, so the
//! output starts where the input does, and it gives `round(n * to / from)`
//! frames for `n`.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler as _};

/// The frames the resampler takes at a time.
const CHUNK: usize = 1024;

/// Resamples interleaved frames from one rate to another.
pub(super) struct Resampler {
    inner: Fft<f64>,
    channels: usize,
    from: u32,
    to: u32,
    /// What's been taken and not yet resampled, interleaved.
    pending: Vec<f64>,
    /// The frames taken in all.
    taken: u64,
    /// The resampler's output, interleaved.
    out: Vec<f64>,
    /// The frames of output still to drop: the resampler's delay.
    delay: usize,
    /// The frames given so far.
    given: u64,
}

impl Resampler {
    /// Resamples `channels` channels from `from` Hz to `to` Hz.
    pub(super) fn new(from: u32, to: u32, channels: usize) -> Result<Self, String> {
        let inner = Fft::<f64>::new(
            from as usize,
            to as usize,
            CHUNK,
            1,
            channels,
            FixedSync::Input,
        )
        .map_err(|error| format!("Couldn't resample from {from} Hz to {to} Hz: {error}."))?;
        Ok(Self {
            channels,
            from,
            to,
            pending: Vec::new(),
            taken: 0,
            out: vec![0.0; inner.output_frames_max() * channels],
            delay: inner.output_delay(),
            given: 0,
            inner,
        })
    }

    /// Takes `samples`, whole frames, and gives what's resampled so far to
    /// `each`.
    pub(super) fn process(
        &mut self,
        samples: &[f64],
        each: &mut dyn FnMut(&[f64]) -> Result<(), String>,
    ) -> Result<(), String> {
        self.pending.extend_from_slice(samples);
        self.taken += (samples.len() / self.channels) as u64;
        while self.pending.len() >= self.inner.input_frames_next() * self.channels {
            self.step(None, u64::MAX, each)?;
        }
        Ok(())
    }

    /// Gives the rest to `each`: what's pending, and what the delay holds
    /// back, up to the length the input makes.
    pub(super) fn finish(
        mut self,
        each: &mut dyn FnMut(&[f64]) -> Result<(), String>,
    ) -> Result<(), String> {
        let (from, to) = (u128::from(self.from), u128::from(self.to));
        let length = ((u128::from(self.taken) * to + from / 2) / from) as u64;
        let mut left = self.pending.len() / self.channels;
        while self.given < length {
            self.step(Some(left), length, each)?;
            left = 0;
        }
        Ok(())
    }

    /// Resamples a chunk of what's pending, or of the `partial` frames
    /// there are and silence after them, and gives the output past the
    /// delay to `each`, up to frame `length` of it.
    fn step(
        &mut self,
        partial: Option<usize>,
        length: u64,
        each: &mut dyn FnMut(&[f64]) -> Result<(), String>,
    ) -> Result<(), String> {
        let channels = self.channels;
        let needed = self.inner.input_frames_next();
        if let Some(partial) = partial {
            self.pending.truncate(partial * channels);
            self.pending.resize(needed * channels, 0.0);
        }
        let failed = |error: &dyn std::fmt::Display| format!("Couldn't resample: {error}.");
        let input = InterleavedSlice::new(&self.pending[..needed * channels], channels, needed)
            .map_err(|error| failed(&error))?;
        let room = self.out.len() / channels;
        let mut output = InterleavedSlice::new_mut(&mut self.out, channels, room)
            .map_err(|error| failed(&error))?;
        let indexing = Indexing {
            input_offset: 0,
            output_offset: 0,
            partial_len: partial,
            active_channels_mask: None,
        };
        let (used, made) = (self
            .inner
            .process_into_buffer(&input, &mut output, Some(&indexing)))
        .map_err(|error| failed(&error))?;
        self.pending.drain(..used.min(needed) * channels);
        let skipped = self.delay.min(made);
        self.delay -= skipped;
        let wanted = (made - skipped).min((length - self.given) as usize);
        self.given += wanted as u64;
        let start = skipped * channels;
        each(&self.out[start..start + wanted * channels])
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    /// `samples` resampled from `from` Hz to `to` Hz, fed in pieces of
    /// `piece` frames.
    fn resample(samples: &[f64], channels: usize, from: u32, to: u32, piece: usize) -> Vec<f64> {
        let mut resampler = Resampler::new(from, to, channels).unwrap();
        let mut out = Vec::new();
        let mut keep = |samples: &[f64]| {
            out.extend_from_slice(samples);
            Ok(())
        };
        for chunk in samples.chunks(piece * channels) {
            resampler.process(chunk, &mut keep).unwrap();
        }
        resampler.finish(&mut keep).unwrap();
        out
    }

    /// A sine's amplitude in `samples` at `rate`, from its middle half.
    fn amplitude(samples: &[f64], hz: f64, rate: f64) -> f64 {
        let (skip, take) = (samples.len() / 4, samples.len() / 2);
        let (mut sin, mut cos) = (0.0, 0.0);
        for (i, sample) in samples.iter().enumerate().skip(skip).take(take) {
            let phase = TAU * hz * i as f64 / rate;
            sin += sample * phase.sin();
            cos += sample * phase.cos();
        }
        2.0 * sin.hypot(cos) / take as f64
    }

    #[test]
    fn the_output_is_as_long_as_the_input_and_starts_where_it_does() {
        for (from, to) in [
            (44_100, 48_000),
            (48_000, 44_100),
            (96_000, 44_100),
            (8_000, 11_025),
        ] {
            for frames in [0, 1, 999, 44_100] {
                let mut input = vec![0.0; frames];
                if frames > 500 {
                    input[500] = 1.0;
                }
                for piece in [7, 4096] {
                    let out = resample(&input, 1, from, to, piece);
                    let length =
                        (frames as u64 * u64::from(to) + u64::from(from) / 2) / u64::from(from);
                    assert_eq!(out.len() as u64, length, "{from} to {to}, {frames} frames");
                    if frames > 500 {
                        // The impulse lands where frame 500 is at the new rate.
                        let (at, _) = (out.iter().enumerate())
                            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                            .unwrap();
                        let expected = 500.0 * f64::from(to) / f64::from(from);
                        assert!((at as f64 - expected).abs() <= 1.0, "{at} for {expected}");
                    }
                }
            }
        }
    }

    #[test]
    fn it_keeps_what_the_new_rate_holds_and_leaves_out_what_it_doesnt() {
        let rate = 48_000.0;
        let tone = |hz: f64| -> Vec<f64> {
            (0..48_000)
                .flat_map(|i| {
                    let sample = 0.5 * (TAU * hz * f64::from(i) / rate).sin();
                    [sample, -sample]
                })
                .collect()
        };
        let left = |samples: &[f64]| samples.iter().step_by(2).copied().collect::<Vec<_>>();
        // 20 kHz stays as loud, in both channels.
        let out = resample(&tone(20_000.0), 2, 48_000, 44_100, 1000);
        let kept = amplitude(&left(&out), 20_000.0, 44_100.0);
        assert!((20.0 * (kept / 0.5).log10()).abs() < 0.01, "{kept}");
        let right: Vec<f64> = out
            .iter()
            .skip(1)
            .step_by(2)
            .map(|sample| -sample)
            .collect();
        assert_eq!(left(&out), right);
        // 23 kHz, above the new Nyquist frequency, doesn't come back as
        // 21.1 kHz.
        let out = resample(&tone(23_000.0), 2, 48_000, 44_100, 1000);
        let alias = amplitude(&left(&out), 44_100.0 - 23_000.0, 44_100.0);
        assert!(20.0 * (alias / 0.5).log10() < -100.0, "{alias}");
    }
}
