//! Rubber Band Library's R3 engine: the time-stretch and pitch-shift that
//! Spectral renders with while High quality is on.
//!
//! It's C++ by Particular Programs Ltd, under the GNU GPL, version 2 or
//! later, built by `build.rs` from its single-file source with Musicable's
//! C interface to it (`src/rubber_band.cpp`), and bound here by hand.
//! `vendor/README.md` says where it came from, and what Musicable changed in
//! it.
//!
//! A [`RubberBand`] runs offline or in real time, and either way is told
//! before it starts where its key frames are: input frames it puts out on
//! given output frames, stretching the input between them evenly. Offline,
//! it's told first how much input is coming ([`study`](RubberBand::study)),
//! plays all of it at one pitch, and its output lines up with its input from
//! the first frame. In real time, its pitch, formants and envelope can change
//! from one hop to the next; its output starts
//! [`start_delay`](RubberBand::start_delay) frames late, and lines up with its
//! input from [`start_pad`](RubberBand::start_pad) frames in. Given
//! [`wants`](RubberBand::wants) frames at a time, it works on one hop a call,
//! which keeps it to its key frames to the frame. The same calls give the
//! same audio, bit for bit, on the same build. It allocates as it goes, so
//! it's not for the audio thread.

use std::ffi::c_int;
use std::ptr::NonNull;
use std::sync::{Mutex, PoisonError};

use crate::MAX_CHANNELS;

/// Changes when what Rubber Band renders from the same calls may change: a
/// new release of the vendored code, or a change to how it's set up. The
/// render cache keys its segments with it, so they're rendered again.
pub const RUBBER_BAND_VERSION: u32 = 1;

/// The most frames of input one [`process`](RubberBand::process) takes:
/// the stereo stretcher splits each call's input into mid and side in a
/// buffer this long or longer.
pub const MOST_FRAMES: usize = 16_384;

mod ffi {
    use std::ffi::c_int;

    /// A stretcher, as `src/rubber_band.cpp` makes it.
    #[repr(C)]
    pub(crate) struct RawRubberBand {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        pub(crate) fn musicable_rubber_band_new(
            channels: c_int,
            sample_rate: f64,
            real_time: c_int,
            ratio: f64,
            pitch: f64,
        ) -> *mut RawRubberBand;
        pub(crate) fn musicable_rubber_band_free(stretch: *mut RawRubberBand);
        pub(crate) fn musicable_rubber_band_set_pitch_scale(
            stretch: *mut RawRubberBand,
            scale: f64,
        );
        pub(crate) fn musicable_rubber_band_set_formants(
            stretch: *mut RawRubberBand,
            preserved: c_int,
            scale: f64,
        );
        pub(crate) fn musicable_rubber_band_set_envelope(stretch: *mut RawRubberBand, scale: f64);
        pub(crate) fn musicable_rubber_band_set_key_frames(
            stretch: *mut RawRubberBand,
            inputs: *const usize,
            outputs: *const usize,
            count: usize,
        ) -> c_int;
        pub(crate) fn musicable_rubber_band_study(stretch: *mut RawRubberBand, frames: usize);
        pub(crate) fn musicable_rubber_band_start_pad(stretch: *const RawRubberBand) -> usize;
        pub(crate) fn musicable_rubber_band_start_delay(stretch: *const RawRubberBand) -> usize;
        pub(crate) fn musicable_rubber_band_samples_required(
            stretch: *const RawRubberBand,
        ) -> usize;
        pub(crate) fn musicable_rubber_band_process(
            stretch: *mut RawRubberBand,
            inputs: *const *const f32,
            frames: usize,
            last: c_int,
        ) -> c_int;
        pub(crate) fn musicable_rubber_band_available(stretch: *const RawRubberBand) -> c_int;
        pub(crate) fn musicable_rubber_band_retrieve(
            stretch: *const RawRubberBand,
            outputs: *const *mut f32,
            frames: usize,
        ) -> usize;
    }
}

/// One Rubber Band stretcher, on its R3 engine, which the library calls
/// its finer one: stereo as a pair, mid and side, which keeps a mix
/// mono-compatible.
pub struct RubberBand {
    raw: NonNull<ffi::RawRubberBand>,
    channels: usize,
    sample_rate: u32,
    real_time: bool,
    /// Whether it has had input.
    started: bool,
}

// SAFETY: a stretcher holds nothing tied to the thread that made it, and
// `&mut self` keeps two threads from using it at once.
unsafe impl Send for RubberBand {}

impl RubberBand {
    /// An offline stretcher of `channels` channels at `sample_rate`, putting
    /// out `ratio` frames for each frame of input, unless key frames say
    /// otherwise, at `pitch` times each frequency. Allocates.
    ///
    /// # Panics
    ///
    /// With no channels or more than [`MAX_CHANNELS`], no sample rate, a
    /// ratio or pitch that isn't above 0, or no memory for it.
    pub fn offline(channels: usize, sample_rate: u32, ratio: f64, pitch: f64) -> Self {
        Self::new(channels, sample_rate, false, ratio, pitch)
    }

    /// A real-time stretcher, as [`offline`](Self::offline) makes one, whose
    /// pitch, formants and envelope may change as it plays: its start pad
    /// and delay are for `pitch`.
    pub fn real_time(channels: usize, sample_rate: u32, ratio: f64, pitch: f64) -> Self {
        Self::new(channels, sample_rate, true, ratio, pitch)
    }

    fn new(channels: usize, sample_rate: u32, real_time: bool, ratio: f64, pitch: f64) -> Self {
        assert!(
            (1..=MAX_CHANNELS).contains(&channels),
            "a stretcher has 1 to {MAX_CHANNELS} channels, not {channels}"
        );
        assert!(sample_rate > 0, "a stretcher needs a sample rate");
        assert!(ratio > 0.0 && pitch > 0.0, "a stretcher plays forwards");
        // SAFETY: the interface takes any channel count, rate, ratio and
        // pitch, and gives back null if it can't allocate.
        let raw = unsafe {
            ffi::musicable_rubber_band_new(
                channels as c_int,
                f64::from(sample_rate),
                c_int::from(real_time),
                ratio,
                pitch,
            )
        };
        let raw = NonNull::new(raw).expect("no memory for a stretcher");
        Self {
            raw,
            channels,
            sample_rate,
            real_time,
            started: false,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn is_real_time(&self) -> bool {
        self.real_time
    }

    /// Moves the pitch: `scale` times each frequency, so 2 is an octave up.
    /// In real time, from the audio its next hop is centred on; offline,
    /// only before its first input.
    pub fn set_pitch(&mut self, scale: f64) {
        assert!(scale > 0.0, "a pitch scale is above 0");
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_rubber_band_set_pitch_scale(self.raw.as_ptr(), scale) }
    }

    /// Moves the formants, the peaks of the spectrum's outline that give a
    /// voice its character, by `scale` before the pitch moves them, so the
    /// inverse of the pitch the audio is heard at keeps them in place; or,
    /// with `None`, leaves them for the pitch to move, which costs least. In
    /// real time, from its next hop on; offline, only before its first
    /// input.
    pub fn set_formants(&mut self, scale: Option<f64>) {
        let (preserved, scale) = match scale {
            Some(scale) => (1, scale),
            None => (0, 1.0),
        };
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_rubber_band_set_formants(self.raw.as_ptr(), preserved, scale) }
    }

    /// How finely the outline of the spectrum that the formants are kept by
    /// follows the spectrum, as a factor on Rubber Band's own, which smooths
    /// it over about 650 Hz: 2 follows details half as wide, for low voices,
    /// and 0.5 smooths it twice as much, for high ones. In real time, from
    /// its next hop on.
    pub fn set_envelope(&mut self, scale: f64) {
        assert!(scale > 0.0, "an envelope scale is above 0");
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_rubber_band_set_envelope(self.raw.as_ptr(), scale) }
    }

    /// Before its first input: the input frames that must come out on given
    /// output frames, each `(input, output)`, both counted from the first
    /// frame of input, which comes out first. From one to the next, and
    /// offline from the last to the end, the ratio is what gets there; in
    /// real time, after the last, it stays.
    ///
    /// # Panics
    ///
    /// Unless the inputs and outputs both rise, from above 0.
    pub fn set_key_frames(&mut self, keys: &[(usize, usize)]) {
        let mut last = (0, 0);
        for &key in keys {
            assert!(
                key.0 > last.0 && key.1 > last.1,
                "key frames rise, from above 0"
            );
            last = key;
        }
        let inputs: Vec<usize> = keys.iter().map(|k| k.0).collect();
        let outputs: Vec<usize> = keys.iter().map(|k| k.1).collect();
        // SAFETY: `raw` is a live stretcher, and both pointers are to `count`
        // frames, which it only reads.
        let done = unsafe {
            ffi::musicable_rubber_band_set_key_frames(
                self.raw.as_ptr(),
                inputs.as_ptr(),
                outputs.as_ptr(),
                keys.len(),
            )
        };
        assert!(done != 0, "the stretcher failed to take its key frames");
    }

    /// Offline, before its first input: how many frames of input are coming,
    /// so it puts out the ratio times as many.
    pub fn study(&mut self, frames: usize) {
        // SAFETY: `raw` is a live stretcher, and `&mut self` its only user.
        unsafe { ffi::musicable_rubber_band_study(self.raw.as_ptr(), frames) }
    }

    /// In real time, the frames of input to give it before the first that
    /// should be heard, so its output lines up with it after
    /// [`start_delay`](Self::start_delay): [`lookahead`]'s. Offline, 0.
    pub fn start_pad(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_rubber_band_start_pad(self.raw.as_ptr()) }
    }

    /// In real time, how many frames of output come before the one that
    /// plays the frame of input [`start_pad`](Self::start_pad) in. Offline,
    /// 0.
    pub fn start_delay(&self) -> usize {
        // SAFETY: `raw` is a live stretcher.
        unsafe { ffi::musicable_rubber_band_start_delay(self.raw.as_ptr()) }
    }

    /// How many frames of input to give it next, once everything it had
    /// ready has been retrieved, for it to work on exactly one hop: the hop
    /// centred on the frame [`lookahead`] before the last of them, counting
    /// a real-time stretcher's start pad as input before its first frame.
    /// Given its input a hop at a time, it keeps to its key frames to the
    /// frame, and its pitch, formants and envelope can change from one hop
    /// to the next.
    pub fn wants(&self) -> usize {
        if !self.real_time && !self.started {
            // Offline, it pads its start with half a window of its own.
            return lookahead(self.sample_rate);
        }
        // SAFETY: `raw` is a live stretcher.
        let wanted = unsafe { ffi::musicable_rubber_band_samples_required(self.raw.as_ptr()) };
        wanted.max(1)
    }

    /// Takes `input`'s frames, a channel to each slice, at most
    /// [`MOST_FRAMES`], working on what it can; with `last`, its last, so it
    /// works on everything and puts it all out.
    ///
    /// # Panics
    ///
    /// If `input` doesn't have a slice for each channel, all as long, or
    /// they're too long.
    pub fn process(&mut self, input: &[&[f32]], last: bool) {
        assert_eq!(input.len(), self.channels, "a slice for each channel");
        let frames = input.first().map_or(0, |c| c.len());
        assert!(
            input.iter().all(|c| c.len() == frames),
            "the channels are all as long"
        );
        assert!(frames <= MOST_FRAMES, "too many frames at once");
        let mut inputs = [std::ptr::null(); MAX_CHANNELS];
        for (pointer, channel) in inputs.iter_mut().zip(input) {
            *pointer = channel.as_ptr();
        }
        // SAFETY: each pointer is to a slice of `frames` frames, which the
        // stretcher only reads.
        let done = unsafe {
            ffi::musicable_rubber_band_process(
                self.raw.as_ptr(),
                inputs.as_ptr(),
                frames,
                c_int::from(last),
            )
        };
        assert!(done != 0, "the stretcher failed to render");
        self.started = true;
    }

    /// How many frames of output it has ready, or `None` once it has put out
    /// everything after its last input.
    pub fn available(&self) -> Option<usize> {
        // SAFETY: `raw` is a live stretcher.
        let available = unsafe { ffi::musicable_rubber_band_available(self.raw.as_ptr()) };
        usize::try_from(available).ok()
    }

    /// Moves as much of its output as is ready into `output`, a channel to
    /// each slice, as far as they're long, and says how many frames.
    ///
    /// # Panics
    ///
    /// If `output` doesn't have a slice for each channel, all as long.
    pub fn retrieve(&mut self, output: &mut [&mut [f32]]) -> usize {
        assert_eq!(output.len(), self.channels, "a slice for each channel");
        let frames = output.first().map_or(0, |c| c.len());
        assert!(
            output.iter().all(|c| c.len() == frames),
            "the channels are all as long"
        );
        let mut outputs = [std::ptr::null_mut(); MAX_CHANNELS];
        for (pointer, channel) in outputs.iter_mut().zip(output.iter_mut()) {
            *pointer = channel.as_mut_ptr();
        }
        // SAFETY: each pointer is to a slice of `frames` frames that only the
        // stretcher writes while this runs.
        unsafe { ffi::musicable_rubber_band_retrieve(self.raw.as_ptr(), outputs.as_ptr(), frames) }
    }
}

impl Drop for RubberBand {
    fn drop(&mut self) {
        // SAFETY: `raw` is a live stretcher, freed once.
        unsafe { ffi::musicable_rubber_band_free(self.raw.as_ptr()) }
    }
}

impl std::fmt::Debug for RubberBand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RubberBand")
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .field("real_time", &self.real_time)
            .finish()
    }
}

/// How far a stretcher at `sample_rate` reads past the middle of the
/// window it works on: half of it, which is also a real-time one's
/// [start pad](RubberBand::start_pad). Worked out with one the first time a
/// rate is asked for.
///
/// # Panics
///
/// With no sample rate.
pub fn lookahead(sample_rate: u32) -> usize {
    static KNOWN: Mutex<Vec<(u32, usize)>> = Mutex::new(Vec::new());
    let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&(_, frames)) = known.iter().find(|(rate, _)| *rate == sample_rate) {
        return frames;
    }
    let frames = RubberBand::real_time(1, sample_rate, 1.0, 1.0).start_pad();
    known.push((sample_rate, frames));
    frames
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    const RATE: u32 = 44_100;

    fn seconds(s: f64) -> usize {
        (s * f64::from(RATE)).round() as usize
    }

    fn sine(frequency: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|n| (TAU * frequency * n as f64 / f64::from(RATE)).sin() as f32 * 0.5)
            .collect()
    }

    /// Silence with a short burst of a 2 kHz tone at each of `at`, in
    /// seconds: a sharp attack, dying away over 30 ms.
    fn bursts(at: &[f64], frames: usize) -> Vec<f32> {
        let mut signal = vec![0.0; frames];
        for &start in at {
            let start = seconds(start);
            for n in 0..seconds(0.03) {
                let t = n as f64 / f64::from(RATE);
                let decay = (-t / 0.008).exp();
                signal[start + n] += (0.5 * decay * (TAU * 2000.0 * t).sin()) as f32;
            }
        }
        signal
    }

    /// Where in `signal` each burst starts, in seconds: the first frame
    /// louder than a fifth of a burst's peak after each stretch of quiet.
    fn onsets(signal: &[f32]) -> Vec<f64> {
        let mut found = Vec::new();
        let mut quiet = seconds(0.1);
        for (n, &s) in signal.iter().enumerate() {
            if s.abs() > 0.1 {
                if quiet >= seconds(0.1) {
                    found.push(n as f64 / f64::from(RATE));
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        found
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

    /// A voice on 150 Hz, its harmonics shaped by a formant at 1 kHz.
    fn voice(frames: usize) -> Vec<f32> {
        let sample = |n: usize| {
            let t = n as f64 / f64::from(RATE);
            let harmonic = |k: u32| {
                let f = 150.0 * f64::from(k);
                let shape = 1.0 / (1.0 + ((f - 1000.0) / 150.0).powi(2));
                0.05 * shape * (TAU * f * t).sin()
            };
            (1..=26).map(harmonic).sum::<f64>() as f32
        };
        (0..frames).map(sample).collect()
    }

    /// Moves what `stretch` has ready onto the end of `output`, mono.
    fn drain(stretch: &mut RubberBand, output: &mut Vec<f32>) {
        while let Some(ready) = stretch.available().filter(|&n| n > 0) {
            let at = output.len();
            output.resize(at + ready, 0.0);
            let got = stretch.retrieve(&mut [&mut output[at..]]);
            output.truncate(at + got);
        }
    }

    /// `input`, mono, through `stretch` a hop at a time, with `before`
    /// called before each hop with the frame of input it's centred on: in
    /// real time after its start pad of silence and without its start delay.
    fn run(
        stretch: &mut RubberBand,
        input: &[f32],
        mut before: impl FnMut(&mut RubberBand, i64),
    ) -> Vec<f32> {
        if !stretch.is_real_time() {
            stretch.study(input.len());
        }
        let (pad, delay) = (stretch.start_pad(), stretch.start_delay());
        let mut padded = vec![0.0; pad];
        padded.extend_from_slice(input);
        let mut output = Vec::new();
        let mut at = 0;
        while at < padded.len() {
            let frames = stretch.wants().min(padded.len() - at);
            before(
                stretch,
                (at + frames) as i64 - (pad + lookahead(RATE)) as i64,
            );
            stretch.process(&[&padded[at..at + frames]], false);
            at += frames;
            drain(stretch, &mut output);
        }
        stretch.process(&[&[]], true);
        drain(stretch, &mut output);
        output.split_off(delay)
    }

    fn assert_near(heard: &[f64], expected: &[f64], within: f64) {
        assert_eq!(
            heard.len(),
            expected.len(),
            "{heard:.4?} for {expected:.4?}"
        );
        for (heard, expected) in heard.iter().zip(expected) {
            assert!(
                (heard - expected).abs() < within,
                "{heard:.4} for {expected:.4}"
            );
        }
    }

    /// Key frames over 2 s of input: 1.25 times as long to 0.5 s, 0.8 times
    /// to 1.5 s, and as long to the end; bursts on them and between them,
    /// and where each should be heard.
    fn keyed() -> (Vec<f32>, [(usize, usize); 3], [f64; 5]) {
        let at = [0.25, 0.5, 1.0, 1.5, 1.75];
        let keys = [
            (seconds(0.5), seconds(0.625)),
            (seconds(1.5), seconds(1.425)),
            (seconds(2.0), seconds(1.925)),
        ];
        (
            bursts(&at, seconds(2.0)),
            keys,
            [0.3125, 0.625, 1.025, 1.425, 1.675],
        )
    }

    #[test]
    fn offline_it_puts_out_the_ratio_times_its_input_at_its_pitch() {
        let input = sine(440.0, seconds(2.0));
        for ratio in [0.8, 1.25] {
            let mut stretch = RubberBand::offline(1, RATE, ratio, 1.0);
            let output = run(&mut stretch, &input, |_, _| {});
            let expected = (input.len() as f64 * ratio).round() as usize;
            assert_eq!(output.len(), expected, "at {ratio}");
            let middle = &output[seconds(0.5)..output.len() - seconds(0.5)];
            let heard = frequency(middle);
            assert!((heard - 440.0).abs() < 3.0, "{heard} at {ratio}");
        }
    }

    #[test]
    fn offline_an_octave_up_a_sine_comes_out_at_twice_its_frequency() {
        let input = sine(440.0, seconds(2.0));
        let mut stretch = RubberBand::offline(1, RATE, 1.0, 2.0);
        let output = run(&mut stretch, &input, |_, _| {});
        assert_eq!(output.len(), input.len());
        let heard = frequency(&output[seconds(0.5)..seconds(1.5)]);
        assert!((heard - 880.0).abs() < 3.0, "{heard}");
    }

    #[test]
    fn offline_its_output_lines_up_with_its_input_at_any_pitch() {
        // Stretched, a burst comes out a little early, the more so the more
        // the stretcher stretches it before resampling: at 1.25 and up a
        // fifth, by about 4 ms, in real time as offline.
        let at = [0.25, 0.75, 1.25, 1.75];
        let input = bursts(&at, seconds(2.0));
        for pitch in [1.0, 1.5] {
            let mut stretch = RubberBand::offline(1, RATE, 1.25, pitch);
            let heard = onsets(&run(&mut stretch, &input, |_, _| {}));
            assert_near(&heard, &at.map(|t| t * 1.25), 0.005);
        }
    }

    #[test]
    fn key_frames_put_their_input_on_their_output() {
        let (input, keys, expected) = keyed();
        let mut stretch = RubberBand::offline(1, RATE, 1.925 / 2.0, 1.0);
        stretch.set_key_frames(&keys);
        assert_near(
            &onsets(&run(&mut stretch, &input, |_, _| {})),
            &expected,
            0.002,
        );
    }

    #[test]
    fn in_real_time_its_output_lines_up_after_its_start_pad_and_delay() {
        let at = [0.25, 0.75, 1.25, 1.75];
        let input = bursts(&at, seconds(2.0));
        let mut stretch = RubberBand::real_time(1, RATE, 1.25, 1.0);
        let output = run(&mut stretch, &input, |_, _| {});
        assert_near(&onsets(&output), &at.map(|t| t * 1.25), 0.003);
    }

    #[test]
    fn in_real_time_key_frames_put_their_input_on_their_output() {
        let (input, keys, expected) = keyed();
        let mut stretch = RubberBand::real_time(1, RATE, 1.0, 1.0);
        stretch.set_key_frames(&keys);
        assert_near(
            &onsets(&run(&mut stretch, &input, |_, _| {})),
            &expected,
            0.002,
        );
    }

    #[test]
    fn in_real_time_a_pitch_change_moves_its_own_audio_on_and_nothing_in_time() {
        // A fifth up from 1.5 s in, set on the hop centred there.
        let change = seconds(1.5) as i64;
        let fifth = |stretch: &mut RubberBand, centre: i64| {
            if centre >= change {
                stretch.set_pitch(1.5);
            }
        };
        let at = [0.25, 0.75, 1.25, 1.75, 2.25, 2.75];
        let mut stretch = RubberBand::real_time(1, RATE, 1.0, 1.0);
        let heard = onsets(&run(&mut stretch, &bursts(&at, seconds(3.0)), fifth));
        assert_near(&heard, &at, 0.003);
        let mut stretch = RubberBand::real_time(1, RATE, 1.0, 1.0);
        let tone = run(&mut stretch, &sine(440.0, seconds(3.0)), fifth);
        let before = frequency(&tone[seconds(0.5)..seconds(1.49)]);
        let after = frequency(&tone[seconds(1.51)..seconds(2.5)]);
        assert!((before - 440.0).abs() < 3.0, "{before}");
        assert!((after - 660.0).abs() < 3.0, "{after}");
    }

    #[test]
    fn the_same_calls_give_the_same_audio() {
        let input = voice(seconds(1.0));
        let offline = || {
            let mut stretch = RubberBand::offline(1, RATE, 1.3, 1.2);
            stretch.set_formants(Some(1.0 / 1.2));
            run(&mut stretch, &input, |_, _| {})
        };
        assert_eq!(offline(), offline());
        // In real time, gliding up a tone with its formants in place, and
        // its envelope changing.
        let real_time = || {
            let mut stretch = RubberBand::real_time(1, RATE, 1.3, 1.0);
            run(&mut stretch, &input, |stretch, centre| {
                let pitch = 1.0 + centre.max(0) as f64 / input.len() as f64 / 2.0;
                stretch.set_pitch(pitch);
                stretch.set_formants(Some(1.0 / pitch));
                stretch.set_envelope(pitch);
            })
        };
        assert_eq!(real_time(), real_time());
    }

    #[test]
    fn transposed_with_its_formants_in_place_a_voice_keeps_its_level() {
        let input = voice(seconds(3.0));
        let heard = level(&input[seconds(1.0)..]);
        // Down 5 and 1 semitones, then up, in dB from the input's level.
        let levels = [-5.0, -1.0, 1.0, 5.0].map(|semitones: f64| {
            let pitch = 2f64.powf(semitones / 12.0);
            let mut stretch = RubberBand::offline(1, RATE, 1.0, pitch);
            stretch.set_formants(Some(1.0 / pitch));
            let output = run(&mut stretch, &input, |_, _| {});
            20.0 * (level(&output[seconds(1.0)..]) / heard).log10()
        });
        assert!(levels.iter().all(|db| db.abs() < 4.0), "{levels:.1?}");
    }

    #[test]
    fn the_envelope_changes_how_the_formants_are_kept() {
        let input = voice(seconds(2.0));
        let pitch = 2f64.powf(5.0 / 12.0);
        let render = |envelope| {
            let mut stretch = RubberBand::offline(1, RATE, 1.0, pitch);
            stretch.set_formants(Some(1.0 / pitch));
            stretch.set_envelope(envelope);
            run(&mut stretch, &input, |_, _| {})
        };
        let (own, finer) = (render(1.0), render(4.0));
        let apart: Vec<f32> = own.iter().zip(&finer).map(|(a, b)| a - b).collect();
        let (heard, change) = (level(&own), level(&apart));
        assert!(change > heard * 0.05, "{change} of {heard}");
        assert_eq!(render(1.0), own, "1 is Rubber Band's own");
    }

    #[test]
    fn its_lookahead_is_half_its_window() {
        assert_eq!(lookahead(44_100), 2048);
        assert_eq!(lookahead(48_000), 2048);
        assert_eq!(lookahead(96_000), 4096);
        let stretch = RubberBand::real_time(2, 48_000, 1.0, 1.0);
        assert_eq!(stretch.start_pad(), lookahead(48_000));
        assert_eq!(RubberBand::offline(2, 48_000, 1.0, 1.0).start_pad(), 0);
    }
}
