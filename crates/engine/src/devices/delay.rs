//! The delay: see [`musicable_model::native::delay`].
//!
//! Each channel has a line that holds what went into it, long enough for
//! the longest time. The echoes are read from it between frames by cubic
//! interpolation, so a modulated time moves smoothly, and a new time fades
//! in from a second read of the line while the first fades out ([`Time`]).
//! What goes in is the signal and the echoes fed back, through the cuts.

use std::f64::consts::PI;

use musicable_model::native::delay::{
    FADE_SECONDS, FEEDBACK, HIGH_CUT, LOW_CUT, MIX, NOTE, NOTE_BEATS, PING_PONG, SYNC, TIME,
    high_cut, low_cut, mix_levels,
};
use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::time::{TempoChange, TempoMap};

use super::svf::{Coefficients, State};
use super::{Params, Span, Values};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The delay's node.
#[derive(Debug, Clone)]
pub struct Delay {
    params: Params,
    echoes: Echoes,
}

impl Delay {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            echoes: Echoes {
                lines: Vec::new(),
                mask: 0,
                write: 0,
                time: Time::default(),
                filters: [[State::default(); 2]; 2],
                cuts: Cuts::new(),
                sample_rate: 48_000.0,
                fade: 1,
            },
        }
    }
}

impl Node for Delay {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        let echoes = &mut self.echoes;
        echoes.sample_rate = f64::from(config.sample_rate);
        let [_, longest] = time_range(echoes.sample_rate);
        // Room for the longest time, and the frames around it that the
        // interpolation reads.
        let len = (longest as usize + 4).next_power_of_two();
        echoes.lines = vec![0.0; 2 * len];
        echoes.mask = len - 1;
        echoes.fade = (FADE_SECONDS * echoes.sample_rate).round().max(1.0) as u32;
        echoes.cuts = Cuts::new();
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        self.params.automate(param, automation)
    }

    fn modulated(&mut self, param: ParamId) {
        self.params.modulated(param);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            // The echoes ring on, read where they were.
            let (echoes, old) = (&mut self.echoes, &mut previous.echoes);
            if old.lines.len() == echoes.lines.len() && old.sample_rate == echoes.sample_rate {
                std::mem::swap(&mut echoes.lines, &mut old.lines);
                echoes.write = old.write;
                echoes.time = old.time;
                echoes.filters = old.filters;
            }
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self { params, echoes } = self;
        params.run(ctx, |span| echoes.render(ctx, &span, output));
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.echoes.reset();
    }
}

/// The delay's lines and what reads and fills them.
#[derive(Debug, Clone)]
struct Echoes {
    /// Each line's frames, the left's and then the right's, `mask + 1` of
    /// them each.
    lines: Vec<f32>,
    mask: usize,
    /// Where the lines' next frame goes, before the mask.
    write: usize,
    time: Time,
    /// Each line's low cut and high cut.
    filters: [[State; 2]; 2],
    cuts: Cuts,
    sample_rate: f64,
    /// How many frames a new time takes to fade in.
    fade: u32,
}

impl Echoes {
    /// Empties the lines and the filters, for a time set afresh.
    fn reset(&mut self) {
        self.lines.fill(0.0);
        self.write = 0;
        self.time = Time::default();
        self.filters = [[State::default(); 2]; 2];
    }

    /// Renders one span of the output, in place.
    fn render(&mut self, ctx: &ProcessContext<'_>, span: &Span<'_>, output: &mut AudioMut<'_>) {
        let values = span.values;
        let frames = span.frames.len();
        if frames == 0 {
            return;
        }
        let mut out = output.range_mut(span.frames.clone());
        let sample_rate = self.sample_rate;
        // Where in the song the span starts, for the tempo it's heard at,
        // and where the tempo next changes.
        let playing = ctx.transport.playing;
        let mut position = ctx.transport.position;
        if playing {
            position += span.frames.start as u64;
        }
        let (mut bpm, mut next_change) = tempo_at(ctx.tempo, position, ctx.sample_rate);
        let [shortest, longest] = time_range(sample_rate);
        // Modulation moves the time in ratios, as its scale is a log one.
        let modulation = values.modulations(TIME);
        let steady_cuts = values.steady(LOW_CUT) && values.steady(HIGH_CUT);
        if steady_cuts {
            let (low, high) = (values.first(LOW_CUT), values.first(HIGH_CUT));
            self.cuts.tune(low, high, sample_rate);
        }
        let (feedback, ping_pong, low, high, mix) = (
            values.frames(FEEDBACK),
            values.frames(PING_PONG),
            values.frames(LOW_CUT),
            values.frames(HIGH_CUT),
            values.frames(MIX),
        );
        let stereo = out.channels() >= 2;
        let len = self.mask + 1;
        for i in 0..frames {
            if playing && next_change == Some(position) {
                (bpm, next_change) = tempo_at(ctx.tempo, position, ctx.sample_rate);
            }
            // Automation can move the time set on any frame.
            self.time.wanted = wanted(values, i, bpm, sample_rate);
            if !steady_cuts {
                self.cuts.tune(low[i], high[i], sample_rate);
            }
            let left = out.channel(0)[i];
            let x = [left, if stereo { out.channel(1)[i] } else { left }];
            let (now, next, take) = self.time.next(self.fade);
            let ratio = match modulation[i] {
                0.0 => 1.0,
                m => f64::from(m).exp(),
            };
            let at = |time: f64| (time * ratio).clamp(shortest, longest);
            let echo = [0, 1].map(|c| {
                let line = &self.lines[c * len..][..len];
                let first = read(line, self.mask, self.write, at(now));
                if take == 0.0 {
                    first
                } else {
                    first + take * (read(line, self.mask, self.write, at(next)) - first)
                }
            });
            // What goes in: each side's signal and its echoes, or for ping
            // pong, both sides into the left and each side's echoes into
            // the other, blended while the switch fades.
            let back = echo.map(|e| saturate(feedback[i] / 100.0 * e));
            let p = ping_pong[i];
            let mono = 0.5 * (x[0] + x[1]);
            let straight = [x[0] + back[0], x[1] + back[1]];
            let crossed = [mono + back[1], back[0]];
            for c in 0..2 {
                let into = straight[c] + p * (crossed[c] - straight[c]);
                let [cut_low, cut_high] = &self.cuts.coefficients;
                let [state_low, state_high] = &mut self.filters[c];
                let filtered = state_high.tick(cut_high, state_low.tick(cut_low, f64::from(into)));
                self.lines[c * len + (self.write & self.mask)] = filtered as f32;
            }
            self.write = self.write.wrapping_add(1);
            let [dry, wet] = mix_levels(mix[i]);
            out.channel_mut(0)[i] = dry * x[0] + wet * echo[0];
            if stereo {
                out.channel_mut(1)[i] = dry * x[1] + wet * echo[1];
            }
            if playing {
                position += 1;
            }
        }
    }
}

/// The time the echoes are read at, in frames, which fades to a new one
/// rather than sliding there.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Time {
    /// The time read.
    now: f64,
    /// The time fading in, and how many frames of the fade have passed.
    fade: Option<(f64, u32)>,
    /// The time set: the next fade's, once the one under way ends.
    wanted: f64,
    /// Whether the node has rendered a frame: until it has, a new time
    /// applies at once.
    started: bool,
}

impl Time {
    /// Moves on a frame, and returns the times to read at and how much of
    /// the second to take, from 0.0 to 1.0, over fades of `fade` frames.
    fn next(&mut self, fade: u32) -> (f64, f64, f32) {
        if self.fade.is_none() && self.wanted != self.now {
            if self.started {
                self.fade = Some((self.wanted, 0));
            } else {
                self.now = self.wanted;
            }
        }
        self.started = true;
        let Some((next, frame)) = &mut self.fade else {
            return (self.now, self.now, 0.0);
        };
        *frame += 1;
        // A raised cosine: it starts and ends gently.
        let x = f64::from(*frame) / f64::from(fade);
        let read = (self.now, *next, (0.5 - 0.5 * (PI * x).cos()) as f32);
        if *frame >= fade {
            self.now = *next;
            self.fade = None;
        }
        read
    }
}

/// The cuts' coefficients, and the frequencies they're tuned to.
#[derive(Debug, Clone, Copy)]
struct Cuts {
    coefficients: [Coefficients; 2],
    tuned: Option<[f32; 2]>,
}

impl Cuts {
    /// Tuned to nothing yet.
    fn new() -> Self {
        let flat = |band: musicable_model::native::eq::Band| {
            let section = band.sections().next().expect("a 12 dB cut is one section");
            Coefficients::new(&section, band.freq, 48_000.0)
        };
        Self {
            coefficients: [flat(low_cut(20.0)), flat(high_cut(20_000.0))],
            tuned: None,
        }
    }

    /// Works out the coefficients for a low cut at `low` Hz and a high cut
    /// at `high`, unless they're for them already.
    fn tune(&mut self, low: f32, high: f32, sample_rate: f64) {
        if self.tuned == Some([low, high]) {
            return;
        }
        let bands = [low_cut(f64::from(low)), high_cut(f64::from(high))];
        for (coefficients, band) in self.coefficients.iter_mut().zip(bands) {
            let section = band.sections().next().expect("a 12 dB cut is one section");
            *coefficients = Coefficients::new(&section, band.corner(sample_rate), sample_rate);
        }
        self.tuned = Some([low, high]);
    }
}

/// The shortest and the longest time, in frames: the time parameter's
/// range, and never under the two frames reading needs.
fn time_range(sample_rate: f64) -> [f64; 2] {
    let time = &NativeKind::Delay.params()[TIME];
    let frames = |ms: f64| ms * sample_rate / 1000.0;
    [frames(time.min).max(2.0), frames(time.max)]
}

/// The time set on the span's frame `frame`, in whole frames: the note at
/// `bpm` if synced, or the time in ms, held to the time's range.
fn wanted(values: &Values, frame: usize, bpm: f64, sample_rate: f64) -> f64 {
    let ms = if values.target(SYNC, frame) >= 0.5 {
        let note = values.target(NOTE, frame).round().max(0.0) as usize;
        NOTE_BEATS[note.min(NOTE_BEATS.len() - 1)] * 60_000.0 / bpm
    } else {
        f64::from(values.target(TIME, frame))
    };
    let time = &NativeKind::Delay.params()[TIME];
    let ms = if ms.is_nan() {
        time.default
    } else {
        ms.clamp(time.min, time.max)
    };
    (ms * sample_rate / 1000.0).round().max(2.0)
}

/// The tempo at song position `position`, in bpm, and the position of the
/// next tempo change after it, if there is one. A change takes effect on
/// the frame it plays on.
fn tempo_at(tempo: &TempoMap, position: u64, sample_rate: u32) -> (f64, Option<u64>) {
    let changes = tempo.tempos();
    let frame = |change: &TempoChange| tempo.frame_at(change.at, sample_rate);
    let position = i64::try_from(position).unwrap_or(i64::MAX);
    let next = changes.partition_point(|change| frame(change) <= position);
    let bpm = changes[next.saturating_sub(1)].bpm;
    let after = changes.get(next).map(|change| frame(change).max(0) as u64);
    (bpm, after)
}

/// The line's signal `delay` frames before the frame about to be written,
/// two or more, between frames by cubic (Catmull-Rom) interpolation: exact
/// on a whole frame. `line` is a power of two long, `mask` one less, and
/// `write` where the next frame goes, before the mask.
#[inline]
pub(super) fn read(line: &[f32], mask: usize, write: usize, delay: f64) -> f32 {
    let whole = delay.floor();
    let back = whole as usize;
    let at = |back: usize| line[write.wrapping_sub(back) & mask];
    let t = (delay - whole) as f32;
    if t == 0.0 {
        return at(back);
    }
    // Wrapping, so even a delay that isn't a number reads the line.
    let (y0, y1, y2, y3) = (
        at(back.wrapping_sub(1)),
        at(back),
        at(back + 1),
        at(back + 2),
    );
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + y1
}

/// Linear up to full scale, then softly up to twice it.
#[inline]
fn saturate(x: f32) -> f32 {
    let size = x.abs();
    if size <= 1.0 {
        x
    } else {
        (1.0 + (size - 1.0).tanh()).copysign(x)
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::automation::Point;
    use musicable_model::native::delay::{NOTES, repeat_response};
    use musicable_model::time::{Beats, TimeSignature};

    use super::*;
    use crate::devices::testing::{Run, db, gain_at, modulation, param};

    /// A delay set as `settings`, the others at their defaults.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut delay = NativeDevice::new(NativeKind::Delay);
        for &(param, value) in settings {
            delay.set(param, value);
        }
        delay
    }

    fn run(settings: &[(usize, f64)]) -> Run {
        Run::new(Box::new(Delay::new(&device(settings))))
    }

    /// `frames` of silence on both sides, but for a click on frame 0.
    fn click(frames: usize) -> [Vec<f32>; 2] {
        let mut click = vec![0.0; frames];
        click[0] = 1.0;
        [click.clone(), click]
    }

    /// A sine at `freq` Hz and `level`, at 48 kHz.
    fn sine(freq: f64, level: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / 48_000.0).sin() as f32 * level)
            .collect()
    }

    /// Something to delay that changes all the time.
    fn noise(frames: usize) -> [Vec<f32>; 2] {
        let wave = |seed: usize| -> Vec<f32> {
            (0..frames)
                .map(|i| ((i * 7919 + seed) % 2000) as f32 / 2000.0 - 0.5)
                .collect()
        };
        [wave(0), wave(1013)]
    }

    /// Only echoes, `ms` apart, through cuts that let most things by and
    /// ring for no longer than a few ms.
    const WET: [(usize, f64); 5] = [
        (SYNC, 0.0),
        (FEEDBACK, 0.0),
        (LOW_CUT, 200.0),
        (HIGH_CUT, 20_000.0),
        (MIX, 100.0),
    ];

    fn wet(ms: f64, more: &[(usize, f64)]) -> Vec<(usize, f64)> {
        let mut settings = WET.to_vec();
        settings.push((TIME, ms));
        settings.extend_from_slice(more);
        settings
    }

    /// The first frame of `signal` that isn't silent.
    fn first_sound(signal: &[f32]) -> Option<usize> {
        signal.iter().position(|&s| s != 0.0)
    }

    /// The first frame of `signal` louder than -60 dB: where an echo a
    /// fraction of a frame off its time starts, past the tail its
    /// interpolation leaves on the frame before.
    fn first_loud(signal: &[f32]) -> Option<usize> {
        signal.iter().position(|&s| s.abs() > 1e-3)
    }

    /// Whether `signal` is silent, near enough: what's left of an echo
    /// once its cuts have stopped ringing.
    fn quiet(signal: &[f32]) -> bool {
        signal.iter().all(|s| s.abs() < 1e-9)
    }

    #[test]
    fn a_new_delay_passes_the_signal_until_its_first_echo() {
        let input = noise(20_000);
        let output = run(&[]).render(&input, &[]);
        // Synced to a dotted eighth at 120 bpm: 375 ms, 18,000 frames.
        assert_eq!(output[0][..18_000], input[0][..18_000]);
        assert_eq!(output[1][..18_000], input[1][..18_000]);
        assert_ne!(output[0][18_000..], input[0][18_000..]);
    }

    /// The reference test: each echo is the last at the feedback, through
    /// the cuts as the design has them, to within a thousandth of a dB, at
    /// two sample rates.
    #[test]
    fn echoes_repeat_at_the_feedback_through_the_cuts() {
        let (feedback, low, high) = (70.0, 150.0, 6000.0);
        let settings = wet(
            100.0,
            &[(FEEDBACK, feedback), (LOW_CUT, low), (HIGH_CUT, high)],
        );
        for sample_rate in [44_100, 96_000] {
            let fs = f64::from(sample_rate);
            let time = (0.1 * fs) as usize;
            let output = run(&settings)
                .with_sample_rate(sample_rate)
                .render(&click(4 * time), &[])
                .swap_remove(0);
            assert!(output[..time].iter().all(|&s| s == 0.0));
            let echo = |n: usize| &output[n * time..(n + 1) * time];
            let mut freq = 30.0;
            while freq < 20_000.0 {
                // The first echo has been through the cuts once, at full
                // level with the mix all echoes, and each one after is the
                // last through them again, at the feedback.
                let cuts = repeat_response(100.0, low, high, freq, fs);
                let repeat = repeat_response(feedback, low, high, freq, fs);
                for n in 1..4 {
                    let measured = gain_at(echo(n), freq, fs);
                    let design = cuts * repeat.powi(n as i32 - 1);
                    // Far down, the echoes' own rounding to f32 shows:
                    // there it's within a millionth of full scale.
                    let close = if design > 1e-3 {
                        (db(measured) - db(design)).abs() < 1e-3
                    } else {
                        (measured - design).abs() < 1e-6
                    };
                    assert!(
                        close,
                        "{sample_rate} Hz, echo {n} at {freq:.0} Hz: {measured} {design}"
                    );
                }
                freq *= 1.05;
            }
        }
    }

    #[test]
    fn synced_echoes_follow_the_tempo() {
        let note = |name: &str| NOTES.iter().position(|&n| n == name).unwrap() as f64;
        for (bpm, name, frames) in [
            (120.0, "1/16", 6000),
            (90.0, "1/8T", 10_667),
            (140.0, "1/4D", 30_857),
            (20.0, "1/1", 240_000),
        ] {
            let mut settings = wet(100.0, &[(NOTE, note(name))]);
            settings[0] = (SYNC, 1.0);
            let tempo = TempoMap::new(bpm, TimeSignature::new(4, 4).unwrap());
            let output = run(&settings)
                .with_tempo(tempo)
                .render(&click(frames + 2000), &[]);
            assert_eq!(first_sound(&output[0]), Some(frames), "{name} at {bpm}");
        }
    }

    #[test]
    fn synced_times_stop_at_five_seconds() {
        let mut settings = wet(100.0, &[(NOTE, 13.0)]);
        settings[0] = (SYNC, 1.0);
        // A whole note at 40 bpm is 6 s.
        let tempo = TempoMap::new(40.0, TimeSignature::new(4, 4).unwrap());
        let output = run(&settings)
            .with_tempo(tempo)
            .render(&click(5 * 48_000 + 100), &[]);
        assert_eq!(first_sound(&output[0]), Some(5 * 48_000));
    }

    #[test]
    fn a_tempo_change_lands_on_its_frame() {
        // 120 bpm for two beats, a second, then 60: a sixteenth goes from
        // 6000 frames to 12,000.
        let mut tempo = TempoMap::new(120.0, TimeSignature::new(4, 4).unwrap());
        tempo.insert_tempo(Beats::new(2), 60.0);
        let mut settings = wet(100.0, &[(NOTE, 2.0)]);
        settings[0] = (SYNC, 1.0);
        let mut input = click(70_000);
        // The second click comes once the new time has faded in.
        input[0][52_000] = 1.0;
        input[1][52_000] = 1.0;
        let render = |block| {
            run(&settings)
                .with_tempo(tempo.clone())
                .playing()
                .with_block(block)
                .render(&input, &[])
                .swap_remove(0)
        };
        let output = render(512);
        assert_eq!(first_sound(&output), Some(6000));
        assert!(quiet(&output[12_000..64_000]));
        assert!(output[64_000].abs() > 0.1);
        // However the blocks fall.
        for block in [77, 1000] {
            assert_eq!(render(block), output, "blocks of {block}");
        }
        // Stopped, the tempo is the one where the playhead stands.
        let stopped = run(&settings)
            .with_tempo(tempo.clone())
            .render(&input, &[])
            .swap_remove(0);
        assert_ne!(stopped[58_000], 0.0);
    }

    #[test]
    fn ping_pong_sends_each_echo_to_the_other_side() {
        let settings = wet(50.0, &[(FEEDBACK, 50.0)]);
        let silence = vec![0.0; 5 * 2400];
        let mut left = silence.clone();
        left[0] = 1.0;
        let mut ping_pong = settings.clone();
        ping_pong.push((PING_PONG, 1.0));
        let output = run(&ping_pong).render(&[left, silence.clone()], &[]);
        // Each echo is a plain delay's, of half the click: from the mono
        // mix, half of each side.
        let mut half = silence.clone();
        half[0] = 0.5;
        let plain = run(&settings)
            .render(&[half.clone(), half], &[])
            .swap_remove(0);
        let echo = |n: usize| n * 2400..(n + 1) * 2400;
        // The first goes left, bit for bit, and nothing goes right.
        assert_eq!(output[0][echo(1)], plain[echo(1)]);
        assert!(output[1][..echo(2).start].iter().all(|&s| s == 0.0));
        // Then each goes to the other side.
        for n in 2..5 {
            let (side, other) = if n % 2 == 0 { (1, 0) } else { (0, 1) };
            let same = output[side][echo(n)]
                .iter()
                .zip(&plain[echo(n)])
                .all(|(a, b)| (a - b).abs() < 1e-9);
            assert!(same, "echo {n}");
            assert!(quiet(&output[other][echo(n)]), "echo {n}");
        }
    }

    #[test]
    fn the_dry_signal_stays_full_up_to_half_way() {
        let input = noise(12_000);
        let echoes = run(&wet(100.0, &[(FEEDBACK, 40.0)]));
        let echoes = { echoes }.render(&input, &[]);
        let mixed = |mix| run(&wet(100.0, &[(FEEDBACK, 40.0), (MIX, mix)])).render(&input, &[]);
        // None at 0%, bit for bit.
        assert_eq!(mixed(0.0), input);
        for (mix, dry, wet) in [(30.0, 1.0, 0.6), (50.0, 1.0, 1.0), (75.0, 0.5, 1.0)] {
            let output = mixed(mix);
            for c in 0..2 {
                for i in 0..12_000 {
                    let expected = dry * input[c][i] + wet * echoes[c][i];
                    assert!((output[c][i] - expected).abs() < 1e-6, "{mix}%");
                }
            }
        }
    }

    #[test]
    fn a_new_time_fades_in_after_the_last() {
        let mut input = click(60_000);
        input[0][40_000] = 1.0;
        input[1][40_000] = 1.0;
        let output = run(&wet(100.0, &[]))
            .render(&input, &[param(20_000, TIME, 150.0)])
            .swap_remove(0);
        assert_eq!(first_sound(&output), Some(4800));
        assert!(quiet(&output[10_000..47_200]));
        assert!(output[47_200].abs() > 0.1);
    }

    #[test]
    fn changing_the_time_never_clicks() {
        // A sine and its echoes, while the time jumps about: whatever the
        // echoes' phases, no frame moves further than the steepest sine
        // they could add up to.
        let level = 0.25;
        let tone = sine(440.0, level, 96_000);
        let settings = wet(100.0, &[(FEEDBACK, 60.0), (MIX, 50.0)]);
        let events = [
            param(20_000, TIME, 170.0),
            param(21_000, TIME, 40.0),
            param(40_000, SYNC, 1.0),
            param(60_000, NOTE, 13.0),
            param(70_000, TIME, 3.0),
            param(70_000, SYNC, 0.0),
        ];
        let output = run(&settings).render(&[tone.clone(), tone], &events);
        let loudest = level + level / (1.0 - 0.6);
        let steepest = loudest * (std::f32::consts::TAU * 440.0 / 48_000.0);
        for channel in &output {
            let step = channel
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            assert!(step < steepest * 1.05, "{step} against {steepest}");
        }
    }

    #[test]
    fn modulating_the_time_moves_the_echoes_at_once() {
        // A tenth longer, from the first frame: no fade. The time's scale
        // is a log one, so modulation moves it in ratios.
        let longer = |ratio: f64| modulation(0, TIME, ratio.ln());
        let output = run(&wet(100.0, &[]))
            .render(&click(6000), &[longer(1.1)])
            .swap_remove(0);
        assert_eq!(first_loud(&output), Some(5280));
        // Synced too: 125 ms, and 10 ms more.
        let mut settings = wet(100.0, &[(NOTE, 2.0)]);
        settings[0] = (SYNC, 1.0);
        let output = run(&settings)
            .render(&click(7000), &[longer(1.08)])
            .swap_remove(0);
        assert_eq!(first_loud(&output), Some(6480));
    }

    #[test]
    fn full_feedback_never_runs_away() {
        // A tone whose echoes all line up, so they'd add up forever.
        let tone = sine(1000.0, 0.5, 4 * 48_000);
        let settings = wet(10.0, &[(FEEDBACK, 100.0)]);
        let output = run(&settings).render(&[tone.clone(), tone], &[]);
        let loudest = output
            .iter()
            .flatten()
            .fold(0.0, |m: f32, s| m.max(s.abs()));
        assert!(loudest < 2.5, "{loudest}");
        assert!(loudest > 1.5, "{loudest}");
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let settings = wet(
            30.0,
            &[(FEEDBACK, 70.0), (LOW_CUT, 300.0), (HIGH_CUT, 5000.0)],
        );
        let input = noise(20_000);
        let events = [
            param(700, TIME, 12.5),
            modulation(2300, TIME, -0.5),
            param(5000, PING_PONG, 1.0),
            param(6001, HIGH_CUT, 2000.0),
            param(9000, SYNC, 1.0),
            modulation(12_000, TIME, 0.0),
            param(15_000, MIX, 60.0),
        ];
        let render = |block| run(&settings).with_block(block).render(&input, &events);
        let reference = render(512);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    /// The reference for automation: a lane's step plays as setting the
    /// time on the step's frame does, a fade to the new time, however the
    /// value glides there.
    #[test]
    fn a_lane_on_the_time_plays_as_setting_it_does() {
        let input = noise(48_000);
        let settings = wet(100.0, &[(FEEDBACK, 50.0)]);
        // Beat 1 at 120 bpm: half a second in.
        let set = run(&settings)
            .playing()
            .render(&input, &[param(24_000, TIME, 200.0)]);
        let mut delay = Delay::new(&device(&settings));
        let step =
            [(0, 100.0), (1, 100.0), (1, 200.0)].map(|(at, ms)| Point::new(Beats::new(at), ms));
        let lane = Automation::new(&step, NativeKind::Delay.params()[TIME].scale).unwrap();
        assert!(delay.automate(ParamId(TIME as u64), lane).is_none());
        let automated = Run::new(Box::new(delay)).playing().render(&input, &[]);
        assert_eq!(automated, set);
        assert_ne!(automated, run(&settings).playing().render(&input, &[]));
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let settings = wet(30.0, &[(FEEDBACK, 70.0), (PING_PONG, 1.0)]);
        let input = noise(12_000);
        let new = || Box::new(Delay::new(&device(&settings)));
        let events = [param(2000, TIME, 45.0)];
        let whole = Run::new(new()).render(&input, &events);
        let mut run = Run::new(new());
        let first = input.each_ref().map(|c| c[..2100].to_vec());
        let second = input.each_ref().map(|c| c[2100..].to_vec());
        let mut swapped = run.render(&first, &events);
        // Halfway through the fade to the new time, a node made from the
        // project, which has it.
        let mut changed = device(&settings);
        changed.set(TIME, 45.0);
        run.swap(Box::new(Delay::new(&changed)));
        let rest = run.render(&second, &[]);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }
}
