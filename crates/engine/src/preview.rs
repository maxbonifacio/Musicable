//! Previews: sounds the engine plays on their own, outside the song, such as
//! the sample selected in the browser.
//!
//! A [`Preview`] plays a file's audio once, from its start, at the file's
//! own speed: resampled when the file isn't at the engine's sample rate, as
//! clips are. Mono files play in both channels, and files with more than two
//! play their first two. The engine adds previews to its output where it
//! adds the cue output, so they play while the song does or doesn't, and
//! never reach an export's taps. One preview plays at a time. Another one,
//! or stopping, fades the one playing out over [`FADE`] frames, so it ends
//! without a click; the next one starts at once.
//!
//! A preview of a file that streams waits until the stream has its first
//! frames in memory, then plays. Where the disk falls behind after that, it
//! plays silence, as a clip does.

use std::fmt;

use crate::clips::read;
use crate::media::{CHUNK, Source};
use crate::resample::Resampler;
use crate::{AudioBuffer, AudioMut, AudioRef, EngineConfig};

/// Frames a preview fades out over when it's stopped or another replaces
/// it: under 3 ms at 48 kHz.
pub const FADE: u32 = 128;

/// A sound to preview. Made off the audio thread, where it allocates what it
/// needs to play, and sent with [`EngineHandle::preview`](crate::EngineHandle::preview).
pub struct Preview {
    id: u64,
    source: Source,
    /// For a file that isn't at the engine's rate.
    resampler: Option<Resampler>,
    /// The next frame it plays, counted from its first.
    played: u64,
    /// How many frames it lasts, at the engine's rate.
    length: u64,
    /// The file's frames for one block, a channel after another.
    input: Vec<f32>,
    /// The resampler's weights for one frame.
    weights: Vec<f32>,
    /// Frames of fading out left, once it's stopping.
    fading: Option<u32>,
    /// Whether it has started: one that streams waits for its first frames.
    started: bool,
}

impl Preview {
    /// A preview of `source` for an engine with `config`, which the engine's
    /// [`Stats::preview`](crate::Stats::preview) names `id`.
    pub fn new(id: u64, source: Source, config: EngineConfig) -> Self {
        let rate = source.sample_rate();
        let resampler =
            (rate != config.sample_rate).then(|| Resampler::new(rate, config.sample_rate));
        let (input, weights, length) = match &resampler {
            None => (config.max_block, 0, source.frames()),
            Some(r) => {
                let span = ((config.max_block - 1) as f64 * r.step()).ceil() as usize;
                let length = (source.frames() as f64 / r.step()).ceil() as u64;
                (span + 2 * r.reach() + 2, 2 * r.reach(), length)
            }
        };
        if let Source::Stream(stream) = &source {
            // So the disk thread starts reading before the engine has it.
            stream.hint(0, None);
        }
        Self {
            id,
            source,
            resampler,
            played: 0,
            length,
            input: vec![0.0; 2 * input],
            weights: vec![0.0; weights],
            fading: None,
            started: false,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// The file frame it's at.
    pub fn frame(&self) -> u64 {
        match &self.resampler {
            None => self.played,
            Some(r) => ((self.played as f64 * r.step()) as u64).min(self.source.frames()),
        }
    }

    /// Whether it has played to its end, or faded out.
    pub fn finished(&self) -> bool {
        self.played >= self.length || self.fading == Some(0)
    }

    /// Starts fading it out.
    fn fade(&mut self) {
        if self.fading.is_none() {
            self.fading = Some(FADE);
        }
    }

    /// Adds its next frames to `output`, as many as `output` is long, at a
    /// gain going from `from` to `to` across them. Real-time safe.
    fn play(&mut self, output: &mut AudioMut<'_>, (from, to): (f32, f32)) {
        if let Source::Stream(stream) = &self.source {
            let reach = self.resampler.map_or(1, |r| r.reach() as u64);
            stream.hint(self.frame().saturating_sub(reach - 1), None);
            if !self.started {
                let first = CHUNK.min(stream.frames());
                if !stream.is_loaded(0, first) {
                    return;
                }
                self.started = true;
            }
        }
        let frames = output.frames();
        let count = (frames as u64).min(self.length.saturating_sub(self.played)) as usize;
        let count = match self.fading {
            Some(left) => count.min(left as usize),
            None => count,
        };
        if count == 0 {
            return;
        }
        let fading = self.fading;
        let level = |k: usize| {
            let ramp = from + (to - from) * k as f32 / frames as f32;
            match fading {
                Some(left) => ramp * (left as usize - k) as f32 / FADE as f32,
                None => ramp,
            }
        };
        let channels = self.source.channels().min(2);
        match &self.resampler {
            None => {
                let input = &mut self.input[..channels * count];
                read(&self.source, self.played as i64, input, channels);
                for c in 0..output.channels() {
                    let input = &input[c.min(channels - 1) * count..][..count];
                    let out = &mut output.channel_mut(c)[..count];
                    for (k, (o, &x)) in out.iter_mut().zip(input).enumerate() {
                        *o += x * level(k);
                    }
                }
            }
            Some(r) => {
                let position = |frame: u64| frame as f64 * r.step();
                let reach = r.reach() as i64;
                let first = self.played;
                let first_in = position(first).floor() as i64 - reach + 1;
                let last = first + count as u64 - 1;
                let span = (position(last).floor() as i64 + reach - first_in + 1) as usize;
                let input = &mut self.input[..channels * span];
                read(&self.source, first_in, input, channels);
                let weights = &mut self.weights[..2 * reach as usize];
                for k in 0..count {
                    let base = (r.weights(position(first + k as u64), weights) - first_in) as usize;
                    let mut sums = [0.0f32; 2];
                    for (c, sum) in sums.iter_mut().enumerate().take(channels) {
                        let input = &input[c * span + base..][..weights.len()];
                        *sum = input.iter().zip(weights.iter()).map(|(x, w)| x * w).sum();
                    }
                    let gain = level(k);
                    for c in 0..output.channels() {
                        output.channel_mut(c)[k] += sums[c.min(channels - 1)] * gain;
                    }
                }
            }
        }
        self.played += count as u64;
        if let Some(left) = &mut self.fading {
            *left -= count as u32;
        }
    }
}

impl fmt::Debug for Preview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Preview")
            .field("id", &self.id)
            .field("source", &self.source)
            .field("played", &self.played)
            .field("length", &self.length)
            .field("fading", &self.fading)
            .finish_non_exhaustive()
    }
}

/// The engine's previews: the one playing, and the one fading out after
/// another replaced it.
pub(crate) struct Previews {
    playing: Option<Box<Preview>>,
    fading: Option<Box<Preview>>,
    /// What they played in the last block.
    buffer: AudioBuffer,
    /// The gain the last block ended on, once a block has played.
    gain: Option<f32>,
}

impl Previews {
    pub(crate) fn new(config: EngineConfig) -> Self {
        Self {
            playing: None,
            fading: None,
            buffer: AudioBuffer::new(2, config.max_block),
            gain: None,
        }
    }

    /// Plays `preview` from now on, or nothing, fading out the one playing.
    /// Returns the previews it lets go of, to be dropped off the audio
    /// thread: at most two.
    pub(crate) fn replace(&mut self, preview: Option<Box<Preview>>) -> [Option<Box<Preview>>; 2] {
        let mut gone = [self.fading.take(), None];
        match self.playing.take() {
            Some(old) if old.finished() => gone[1] = Some(old),
            Some(mut old) => {
                old.fade();
                self.fading = Some(old);
            }
            None => {}
        }
        self.playing = preview;
        gone
    }

    /// Plays the next `frames` frames, at a gain ramping from the last
    /// block's to `gain`, and returns them if a preview played. Real-time
    /// safe.
    pub(crate) fn play(&mut self, frames: usize, gain: f32) -> Option<AudioRef<'_>> {
        let last = self.gain.replace(gain).unwrap_or(gain);
        if self.playing.is_none() && self.fading.is_none() {
            return None;
        }
        let ramp = (last, gain);
        let mut output = self.buffer.view_mut(frames);
        output.clear();
        for preview in [&mut self.playing, &mut self.fading].into_iter().flatten() {
            preview.play(&mut output, ramp);
        }
        Some(self.buffer.view(frames))
    }

    /// The preview playing, if one is: its id and the file frame it's at.
    pub(crate) fn at(&self) -> Option<(u64, u64)> {
        let playing = self.playing.as_ref().filter(|p| !p.finished())?;
        Some((playing.id, playing.frame()))
    }

    /// Takes a preview that has finished, to be dropped off the audio
    /// thread.
    pub(crate) fn take_finished(&mut self) -> Option<Box<Preview>> {
        for slot in [&mut self.fading, &mut self.playing] {
            if slot.as_ref().is_some_and(|p| p.finished()) {
                return slot.take();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::media::{AudioData, AudioReader, Streamer};
    use crate::{Engine, EngineHandle, PreviewAt};

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    fn engine() -> (Engine, EngineHandle) {
        Engine::new(CONFIG)
    }

    /// A mono file of `samples` at `rate`.
    fn file(rate: u32, samples: Vec<f32>) -> Source {
        Source::Whole(Arc::new(AudioData::new(rate, vec![samples])))
    }

    /// Renders `frames` frames, a channel after another.
    fn render(engine: &mut Engine, frames: usize) -> [Vec<f32>; 2] {
        let mut buffer = AudioBuffer::new(2, frames);
        engine.process(&mut buffer.view_mut(frames));
        let view = buffer.view(frames);
        [view.channel(0).to_vec(), view.channel(1).to_vec()]
    }

    #[test]
    fn a_preview_plays_its_file_once_in_both_channels() {
        let (mut engine, mut handle) = engine();
        let samples: Vec<f32> = (1..=200).map(|i| i as f32 / 256.0).collect();
        handle
            .preview(Preview::new(7, file(48_000, samples.clone()), CONFIG))
            .unwrap();
        let [left, right] = render(&mut engine, 100);
        assert_eq!(left, samples[..100]);
        assert_eq!(right, samples[..100]);
        let at = PreviewAt { id: 7, frame: 100 };
        assert_eq!(engine.stats().preview, Some(at));
        let [left, _] = render(&mut engine, 200);
        assert_eq!(left[..100], samples[100..]);
        assert!(left[100..].iter().all(|&x| x == 0.0));
        // Played to its end, it goes back to be dropped.
        assert_eq!(engine.stats().preview, None);
        assert_eq!(handle.collect_garbage(), 1);
    }

    #[test]
    fn a_preview_at_another_rate_is_resampled() {
        let (mut engine, mut handle) = engine();
        // At half the engine's rate, so it lasts twice as many frames.
        handle
            .preview(Preview::new(1, file(24_000, vec![0.5; 1000]), CONFIG))
            .unwrap();
        let [left, right] = render(&mut engine, 2100);
        assert_eq!(left, right);
        // A constant comes through as it is, away from the ends.
        assert!(left[100..1900].iter().all(|&x| (x - 0.5).abs() < 1e-4));
        assert!(left[2000..].iter().all(|&x| x == 0.0));
        assert_eq!(engine.stats().preview, None);
    }

    #[test]
    fn a_new_preview_fades_the_playing_one_out_and_starts_at_once() {
        let (mut engine, mut handle) = engine();
        handle
            .preview(Preview::new(1, file(48_000, vec![1.0; 10_000]), CONFIG))
            .unwrap();
        render(&mut engine, 64);
        handle
            .preview(Preview::new(2, file(48_000, vec![0.25; 10_000]), CONFIG))
            .unwrap();
        let [left, _] = render(&mut engine, 300);
        for (k, &x) in left.iter().enumerate() {
            let fading = FADE.saturating_sub(k as u32) as f32 / FADE as f32;
            let expected = 0.25 + fading;
            assert!((x - expected).abs() < 1e-6, "frame {k}: {x} for {expected}");
        }
        let at = PreviewAt { id: 2, frame: 300 };
        assert_eq!(engine.stats().preview, Some(at));
        // The faded one went back.
        assert_eq!(handle.collect_garbage(), 1);

        // Stopping fades it out too.
        handle.stop_preview().unwrap();
        let [left, _] = render(&mut engine, 200);
        for (k, &x) in left.iter().enumerate() {
            let expected = 0.25 * FADE.saturating_sub(k as u32) as f32 / FADE as f32;
            assert!((x - expected).abs() < 1e-6, "frame {k}: {x} for {expected}");
        }
        assert_eq!(engine.stats().preview, None);
        assert_eq!(handle.collect_garbage(), 1);
    }

    #[test]
    fn previews_ramp_to_their_gain_and_never_reach_the_taps() {
        let (mut engine, mut handle) = engine();
        handle
            .preview(Preview::new(1, file(48_000, vec![1.0; 10_000]), CONFIG))
            .unwrap();
        // The first block plays at the gain set, with none before it.
        handle.set_preview_gain(0.25);
        let [left, _] = render(&mut engine, 64);
        assert!(left.iter().all(|&x| x == 0.25), "{left:?}");
        // A change ramps over the next block of 64 frames.
        handle.set_preview_gain(0.5);
        let [left, _] = render(&mut engine, 128);
        for (k, &x) in left.iter().enumerate() {
            let expected = if k < 64 {
                0.25 + 0.25 * k as f32 / 64.0
            } else {
                0.5
            };
            assert!((x - expected).abs() < 1e-6, "frame {k}: {x} for {expected}");
        }
        // Interleaved, as a device has it.
        let mut interleaved = vec![9.0; 2 * 64];
        engine.process_interleaved(&mut interleaved, 2);
        assert!(interleaved.iter().all(|&x| x == 0.5));
        // An export hears none of it.
        let mut output = AudioBuffer::new(2, 64);
        let mut tap = AudioBuffer::new(2, 64);
        engine.process_with_taps(&mut output.view_mut(64), &mut [tap.view_mut(64)]);
        assert!(output.view(64).iter().flatten().all(|&x| x == 0.0));
        assert!(tap.view(64).iter().flatten().all(|&x| x == 0.0));
    }

    /// A file whose reads wait until `open` is set, each frame its number.
    struct Slow {
        open: Arc<AtomicBool>,
    }

    impl AudioReader for Slow {
        fn channels(&self) -> usize {
            1
        }

        fn sample_rate(&self) -> u32 {
            48_000
        }

        fn frames(&self) -> u64 {
            100_000
        }

        fn read(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
            while !self.open.load(Ordering::Relaxed) {
                thread::yield_now();
            }
            let frames = out.len().min(self.frames().saturating_sub(start) as usize);
            for (i, sample) in out[..frames].iter_mut().enumerate() {
                *sample = (start + i as u64) as f32;
            }
            Ok(frames)
        }
    }

    #[test]
    fn a_preview_that_streams_waits_for_its_first_frames() {
        let (mut engine, mut handle) = engine();
        let streamer = Streamer::spawn(false);
        let open = Arc::new(AtomicBool::new(false));
        let stream = streamer.stream(Box::new(Slow { open: open.clone() }));
        handle
            .preview(Preview::new(3, Source::Stream(stream.clone()), CONFIG))
            .unwrap();
        let [left, _] = render(&mut engine, 64);
        assert!(left.iter().all(|&x| x == 0.0));
        assert_eq!(engine.stats().preview, Some(PreviewAt { id: 3, frame: 0 }));
        open.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stream.is_loaded(0, CHUNK) {
            assert!(Instant::now() < deadline, "the stream didn't load");
            thread::yield_now();
        }
        // From the file's first frame on.
        let [left, right] = render(&mut engine, 64);
        let expected: Vec<f32> = (0..64).map(|i| i as f32).collect();
        assert_eq!(left, expected);
        assert_eq!(right, expected);
    }
}
