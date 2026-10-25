//! The audio an analysis reads: one channel, from any frame, so a long
//! file never has to be in memory at once.

use std::path::Path;

use musicable_io::audio::{AudioError, Decoder};

/// How many frames a file is read in at a time.
pub(crate) const CHUNK: usize = 16_384;

/// Mono audio to analyse, read from any frame.
pub trait Audio {
    fn sample_rate(&self) -> u32;
    /// How long it is, in frames.
    fn frames(&self) -> u64;
    /// Reads frames from `start` on into `out`, as many as fit or as are
    /// left, and returns how many: 0 at the end.
    fn read(&mut self, start: u64, out: &mut [f32]) -> Result<usize, AudioError>;
}

/// Samples in memory.
#[derive(Debug, Clone, Copy)]
pub struct Samples<'a> {
    pub samples: &'a [f32],
    pub sample_rate: u32,
}

impl Audio for Samples<'_> {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn frames(&self) -> u64 {
        self.samples.len() as u64
    }

    fn read(&mut self, start: u64, out: &mut [f32]) -> Result<usize, AudioError> {
        let start = usize::try_from(start)
            .unwrap_or(usize::MAX)
            .min(self.samples.len());
        let read = out.len().min(self.samples.len() - start);
        out[..read].copy_from_slice(&self.samples[start..start + read]);
        Ok(read)
    }
}

/// An audio file, its channels mixed to one.
pub struct File {
    decoder: Decoder,
    /// Interleaved frames, read before mixing.
    buffer: Vec<f32>,
}

impl File {
    pub fn open(path: &Path) -> Result<Self, AudioError> {
        Ok(Self::new(Decoder::open(path)?))
    }

    pub fn new(decoder: Decoder) -> Self {
        Self {
            decoder,
            buffer: Vec::new(),
        }
    }

    /// The tempo the file's loop data gives, if it has some.
    pub fn tempo(&self) -> Option<f64> {
        self.decoder.tempo()
    }
}

impl Audio for File {
    fn sample_rate(&self) -> u32 {
        self.decoder.info().sample_rate
    }

    fn frames(&self) -> u64 {
        self.decoder.info().frames
    }

    fn read(&mut self, start: u64, out: &mut [f32]) -> Result<usize, AudioError> {
        let channels = self.decoder.info().channels;
        self.buffer.resize(out.len() * channels, 0.0);
        let read = self.decoder.read(start, &mut self.buffer)?;
        let scale = 1.0 / channels as f32;
        for (mono, frame) in out
            .iter_mut()
            .zip(self.buffer.chunks_exact(channels))
            .take(read)
        {
            *mono = frame.iter().sum::<f32>() * scale;
        }
        Ok(read)
    }
}

/// Reads `audio` from `start` into `out`, filling what's before its start
/// or past its end with silence.
pub(crate) fn read_padded(
    audio: &mut dyn Audio,
    start: i64,
    out: &mut [f32],
) -> Result<(), AudioError> {
    out.fill(0.0);
    let skip = usize::try_from(-start).unwrap_or(0).min(out.len());
    let mut at = start.max(0) as u64;
    let mut filled = skip;
    while filled < out.len() {
        let read = audio.read(at, &mut out[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
        at += read as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_before_the_start_and_past_the_end_are_silent() {
        let samples = [1.0, 2.0, 3.0];
        let mut audio = Samples {
            samples: &samples,
            sample_rate: 48_000,
        };
        let mut out = [9.0; 6];
        read_padded(&mut audio, -2, &mut out).unwrap();
        assert_eq!(out, [0.0, 0.0, 1.0, 2.0, 3.0, 0.0]);
        read_padded(&mut audio, 2, &mut out[..2]).unwrap();
        assert_eq!(out[..2], [3.0, 0.0]);
        read_padded(&mut audio, 10, &mut out[..2]).unwrap();
        assert_eq!(out[..2], [0.0, 0.0]);
    }
}
