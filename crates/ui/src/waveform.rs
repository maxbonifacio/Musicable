//! Audio files' waveforms, as audio clips draw them.

use std::collections::HashMap;
use std::sync::Arc;

/// The waveforms of the audio files that clips play, by the path the clips
/// give. The app reads the files and fills it in.
pub type Waveforms = HashMap<String, Arc<Peaks>>;

/// The lowest and the highest sample of a stretch of audio, across its
/// channels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
}

impl Peak {
    /// No samples yet: anything widens it.
    const EMPTY: Peak = Peak {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
    };

    fn widen(&mut self, other: Peak) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }
}

/// An audio file's peaks: the lowest and the highest sample in each block of
/// its frames, at a few block sizes, so its waveform draws quickly at any
/// zoom. Build one with [`PeaksBuilder`].
#[derive(Debug, Clone, PartialEq)]
pub struct Peaks {
    sample_rate: u32,
    frames: u64,
    /// The finest level has a peak for each [`BLOCK`](Self::BLOCK) frames,
    /// and each level after it one for each [`FACTOR`](Self::FACTOR) of the
    /// level before.
    levels: Vec<Vec<Peak>>,
}

impl Peaks {
    /// How many frames each peak of the finest level covers.
    pub const BLOCK: u64 = 64;
    /// How many peaks of a level each peak of the next one covers.
    pub const FACTOR: usize = 4;

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// How long the file is.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The lowest and the highest sample from frame `start` up to frame
    /// `end`, which needn't be whole, exact to a block of the finest level.
    /// It takes the stretch's middle from coarser levels, so it's quick
    /// however long the stretch is. `None` where the file has no frames.
    pub fn range(&self, start: f64, end: f64) -> Option<Peak> {
        let start = start.max(0.0);
        let end = end.min(self.frames as f64);
        if end <= start {
            return None;
        }
        let block = Self::BLOCK as f64;
        let mut first = (start / block) as usize;
        let mut last = ((end / block).ceil() as usize).min(self.levels[0].len());
        let mut peak = Peak::EMPTY;
        for peaks in &self.levels {
            // The blocks on either side that the next level's can't stand
            // for, then the rest from the next level.
            while first < last && !first.is_multiple_of(Self::FACTOR) {
                peak.widen(peaks[first]);
                first += 1;
            }
            while first < last && !last.is_multiple_of(Self::FACTOR) {
                last -= 1;
                peak.widen(peaks[last]);
            }
            // The coarsest level has one block, so it always ends here.
            if first >= last {
                break;
            }
            first /= Self::FACTOR;
            last /= Self::FACTOR;
        }
        Some(peak)
    }
}

/// Builds an audio file's [`Peaks`] from its frames, as they're read.
#[derive(Debug, Clone)]
pub struct PeaksBuilder {
    sample_rate: u32,
    channels: usize,
    frames: u64,
    finest: Vec<Peak>,
    /// The block being filled, and how many frames it has.
    block: Peak,
    in_block: u64,
}

impl PeaksBuilder {
    /// For a file with `channels` channels at `sample_rate`.
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            sample_rate,
            channels: channels.max(1),
            frames: 0,
            finest: Vec::new(),
            block: Peak::EMPTY,
            in_block: 0,
        }
    }

    /// Adds the next frames, interleaved.
    pub fn push(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels) {
            for &sample in frame {
                self.block.min = self.block.min.min(sample);
                self.block.max = self.block.max.max(sample);
            }
            self.in_block += 1;
            if self.in_block == Peaks::BLOCK {
                self.finest.push(self.block);
                self.block = Peak::EMPTY;
                self.in_block = 0;
            }
        }
        self.frames += (interleaved.len() / self.channels) as u64;
    }

    pub fn finish(mut self) -> Peaks {
        if self.in_block > 0 {
            self.finest.push(self.block);
        }
        let mut levels = vec![self.finest];
        while levels.last().is_some_and(|level| level.len() > 1) {
            let coarser = levels.last().map_or_else(Vec::new, |level| {
                level
                    .chunks(Peaks::FACTOR)
                    .map(|peaks| {
                        let mut peak = Peak::EMPTY;
                        peaks.iter().for_each(|p| peak.widen(*p));
                        peak
                    })
                    .collect()
            });
            levels.push(coarser);
        }
        Peaks {
            sample_rate: self.sample_rate,
            frames: self.frames,
            levels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ramp from -1 up to 1, on two channels, the second half as loud.
    fn ramp(frames: usize) -> Peaks {
        let mut builder = PeaksBuilder::new(48_000, 2);
        let samples: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let x = i as f32 / (frames - 1) as f32 * 2.0 - 1.0;
                [x, x / 2.0]
            })
            .collect();
        // In uneven pieces, as a file is read.
        for piece in samples.chunks(2 * 1000) {
            builder.push(piece);
        }
        builder.finish()
    }

    #[test]
    fn peaks_hold_the_lowest_and_highest_samples() {
        let peaks = ramp(100_001);
        assert_eq!(peaks.frames(), 100_001);
        assert_eq!(peaks.sample_rate(), 48_000);
        let all = peaks.range(0.0, 100_001.0).unwrap();
        assert_eq!((all.min, all.max), (-1.0, 1.0));
        // The first block: the second channel is higher while the ramp is
        // below zero.
        let first = peaks.range(0.0, 10.0).unwrap();
        assert_eq!(first.min, -1.0);
        assert!(first.max > -0.51 && first.max < -0.49, "{first:?}");
        // The middle, from a coarse level, is exact to a block or so.
        let middle = peaks.range(40_000.0, 60_000.0).unwrap();
        assert!((middle.min + 0.2).abs() < 0.01, "{middle:?}");
        assert!((middle.max - 0.2).abs() < 0.01, "{middle:?}");
        assert_eq!(peaks.range(100_001.0, 200_000.0), None);
        assert_eq!(peaks.range(-50.0, -10.0), None);
        assert_eq!(peaks.levels.last().unwrap().len(), 1);
    }

    #[test]
    fn an_empty_file_has_no_peaks() {
        let peaks = PeaksBuilder::new(44_100, 1).finish();
        assert_eq!(peaks.frames(), 0);
        assert_eq!(peaks.range(0.0, 10.0), None);
    }
}
