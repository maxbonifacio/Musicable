//! The compressor: see [`musicable_model::native::compressor`].
//!
//! A feed-forward design in decibels, after Giannoulis, Massberg and Reiss
//! ("Digital Dynamic Range Compressor Design: A Tutorial and Analysis",
//! JAES, 2012). Each frame it takes the level, reads how far to turn it
//! down off the static curve ([`gain_reduction`]), and smooths that amount
//! with a one-pole filter: the attack's while it grows and the release's
//! while it shrinks (their smooth branching detector, after the gain
//! computer), so each is a time constant whatever the level.

use musicable_model::native::NativeDevice;
use musicable_model::native::compressor::{
    ATTACK, DETECTOR, KNEE, MIX, OUTPUT, RATIO, RELEASE, THRESHOLD, gain_reduction,
};

use super::{Params, Span, Values};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The parameters the DSP reads.
const PARAMS: [usize; 8] = [
    THRESHOLD, RATIO, ATTACK, RELEASE, KNEE, OUTPUT, MIX, DETECTOR,
];

/// The compressor's node.
#[derive(Debug, Clone)]
pub struct Compressor {
    params: Params,
    state: State,
    sample_rate: f64,
}

impl Compressor {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            state: State::default(),
            sample_rate: 48_000.0,
        }
    }
}

impl Node for Compressor {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.sample_rate = f64::from(config.sample_rate);
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        self.params.automate(param, automation)
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            self.state = previous.state;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self {
            params,
            state,
            sample_rate,
        } = self;
        params.run(ctx, |span| state.render(&span, output, *sample_rate));
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.state = State::default();
    }
}

/// What the compressor remembers from frame to frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct State {
    /// How far it's turning the signal down, in dB.
    reduction: f64,
    /// Each channel's power, averaged over the attack time, for RMS. It's
    /// kept up with detection on peaks too, so a switch to RMS finds it
    /// ready.
    power: [f64; 2],
}

impl State {
    /// Compresses one span of the output, in place.
    fn render(&mut self, span: &Span<'_>, output: &mut AudioMut<'_>, sample_rate: f64) {
        let frames = span.frames.len();
        if frames == 0 {
            return;
        }
        let values = span.values;
        let mut out = output.range_mut(span.frames.clone());
        let mut channels = out.channels_mut();
        let Some(left) = channels.next() else {
            return;
        };
        let mut right = channels.next();
        let steady = PARAMS.iter().all(|&p| values.steady(p));
        let mut settings = Settings::at(values, 0, sample_rate);
        for i in 0..frames {
            if !steady && i > 0 {
                settings = Settings::at(values, i, sample_rate);
            }
            let gain = match &right {
                Some(right) => self.gain(&settings, &[left[i], right[i]]),
                None => self.gain(&settings, &[left[i]]),
            };
            left[i] = (f64::from(left[i]) * gain) as f32;
            if let Some(right) = &mut right {
                right[i] = (f64::from(right[i]) * gain) as f32;
            }
        }
    }

    /// Takes in a frame, a sample for each channel, and returns its gain,
    /// with the output gain and the mix.
    fn gain(&mut self, settings: &Settings, frame: &[f32]) -> f64 {
        let mut peak = 0.0_f64;
        let mut power = 0.0_f64;
        for (&sample, average) in frame.iter().zip(&mut self.power) {
            let level = level(sample);
            peak = peak.max(level);
            *average = settings.attack * *average + (1.0 - settings.attack) * level * level;
            power = power.max(*average);
        }
        // Between the two while the switch fades.
        let level = peak * (1.0 - settings.rms) + power.sqrt() * settings.rms;
        let target = gain_reduction(
            20.0 * level.log10(),
            settings.threshold,
            settings.ratio,
            settings.knee,
        );
        let coefficient = if target > self.reduction {
            settings.attack
        } else {
            settings.release
        };
        self.reduction = coefficient * self.reduction + (1.0 - coefficient) * target;
        let gain = linear(settings.output - self.reduction);
        gain * settings.mix + (1.0 - settings.mix)
    }
}

/// The settings on a frame, as the DSP uses them.
#[derive(Debug, Clone, Copy)]
struct Settings {
    /// In dB.
    threshold: f64,
    ratio: f64,
    /// In dB.
    knee: f64,
    /// The one-pole coefficients that go most of the way in the attack
    /// time, which the RMS average uses too, and in the release time.
    attack: f64,
    release: f64,
    /// In dB.
    output: f64,
    /// From 0.0, all dry, to 1.0.
    mix: f64,
    /// 0.0 for peak detection, 1.0 for RMS.
    rms: f64,
}

impl Settings {
    /// As the parameters are on frame `i` of a span.
    fn at(values: &Values, i: usize, sample_rate: f64) -> Self {
        let value = |p: usize| f64::from(values.frames(p)[i]);
        Self {
            threshold: value(THRESHOLD),
            ratio: value(RATIO),
            knee: value(KNEE),
            attack: one_pole(value(ATTACK), sample_rate),
            release: one_pole(value(RELEASE), sample_rate),
            output: value(OUTPUT),
            mix: value(MIX) / 100.0,
            rms: value(DETECTOR),
        }
    }
}

/// The coefficient of a one-pole filter that goes `1 - 1/e` of the way
/// to where it's heading in `ms`.
fn one_pole(ms: f64, sample_rate: f64) -> f64 {
    (-1000.0 / (ms * sample_rate)).exp()
}

/// A sample's level for detection: its size, or nothing if it isn't a
/// finite number, so a stray NaN or infinity can't stick in the detector.
fn level(sample: f32) -> f64 {
    if sample.is_finite() {
        f64::from(sample.abs())
    } else {
        0.0
    }
}

/// The linear gain of `db` decibels: exactly 1.0 for 0 dB.
fn linear(db: f64) -> f64 {
    (db * (std::f64::consts::LN_10 / 20.0)).exp()
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use musicable_model::native::NativeKind;
    use musicable_model::native::compressor::DETECTORS;

    use super::*;
    use crate::devices::testing::{Run, modulation, param};

    /// The sample rate [`Run`] renders at.
    const FS: f64 = 48_000.0;

    /// A compressor set as `settings`, the rest at their defaults.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut device = NativeDevice::new(NativeKind::Compressor);
        for &(param, value) in settings {
            device.set(param, value);
        }
        device
    }

    fn run(device: &NativeDevice) -> Run {
        Run::new(Box::new(Compressor::new(device)))
    }

    /// Each of `levels`, in dB, held for `frames` frames: a signal that's
    /// that size on every frame, flipping sign, which both detectors read
    /// as exactly that level.
    fn steps(levels: &[f64], frames: usize) -> Vec<f32> {
        levels
            .iter()
            .flat_map(|&db| {
                let size = 10_f64.powf(db / 20.0) as f32;
                (0..frames).map(move |i| if i % 2 == 0 { size } else { -size })
            })
            .collect()
    }

    /// The gain on each frame, in dB: the output against the input.
    fn gains(input: &[f32], output: &[f32]) -> Vec<f64> {
        input
            .iter()
            .zip(output)
            .map(|(&i, &o)| 20.0 * (f64::from(o) / f64::from(i)).log10())
            .collect()
    }

    /// A sine at `freq` Hz, peaking at `size`.
    fn sine(freq: f64, size: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (size * (TAU * freq * i as f64 / FS).sin()) as f32)
            .collect()
    }

    /// The reference test: held at a level, the compressor settles on its
    /// static curve, to within a thousandth of a dB, below, across and
    /// above the knee, with either detector.
    #[test]
    fn held_levels_settle_on_the_designed_curve() {
        let levels: Vec<f64> = (0..=24).map(|i| -54.0 + 2.25 * f64::from(i)).collect();
        let hold = 4800;
        let input = steps(&levels, hold);
        let designs = [
            (-20.0, 4.0, 10.0, 0.0),
            (-36.0, 2.0, 0.0, 6.0),
            (-12.0, 20.0, 24.0, -3.0),
            (-50.0, 1.5, 6.0, 0.0),
        ];
        for detector in [0.0, 1.0] {
            for (threshold, ratio, knee, output) in designs {
                let compressor = device(&[
                    (THRESHOLD, threshold),
                    (RATIO, ratio),
                    (KNEE, knee),
                    (OUTPUT, output),
                    (ATTACK, 1.0),
                    (RELEASE, 5.0),
                    (DETECTOR, detector),
                ]);
                let rendered = run(&compressor).render(&[input.clone(), input.clone()], &[]);
                assert_eq!(rendered[0], rendered[1]);
                let gains = gains(&input, &rendered[0]);
                for (step, &level) in levels.iter().enumerate() {
                    let measured = gains[(step + 1) * hold - 1];
                    let design = output - gain_reduction(level, threshold, ratio, knee);
                    assert!(
                        (measured - design).abs() < 1e-3,
                        "{} at {level} dB through {threshold} dB {ratio}:1, knee {knee}: \
                         {measured} dB, {design} dB designed",
                        DETECTORS[detector as usize],
                    );
                }
            }
        }
    }

    /// The attack and the release are time constants: the reduction goes
    /// `1 - 1/e` of the way to where it's heading in each, on an
    /// exponential, however far it has to go.
    #[test]
    fn attack_and_release_are_time_constants() {
        for (attack, release, loud) in
            [(10.0, 100.0, 0.0), (0.5, 1000.0, -12.0), (50.0, 5.0, -24.0)]
        {
            let compressor = device(&[
                (THRESHOLD, -40.0),
                (RATIO, 4.0),
                (KNEE, 0.0),
                (ATTACK, attack),
                (RELEASE, release),
            ]);
            let attack_frames = attack * FS / 1000.0;
            let release_frames = release * FS / 1000.0;
            // Quiet, under the threshold; loud, until the attack is done;
            // then quiet again for the release.
            let quiet = 4800;
            let held = (attack_frames * 20.0) as usize;
            let after = (release_frames * 5.0) as usize;
            let input = [
                steps(&[-50.0], quiet),
                steps(&[loud], held),
                steps(&[-50.0], after),
            ]
            .concat();
            let rendered = run(&compressor).render(&[input.clone(), input.clone()], &[]);
            assert_eq!(rendered[0][..quiet], input[..quiet]);
            let reduction: Vec<f64> = gains(&input, &rendered[0]).iter().map(|g| -g).collect();
            let full = 0.75 * (loud + 40.0);
            for n in 0..held {
                let expected = full * (1.0 - (-(n as f64 + 1.0) / attack_frames).exp());
                let measured = reduction[quiet + n];
                assert!(
                    (measured - expected).abs() < 1e-3,
                    "attack {attack} ms, frame {n}: {measured} dB, {expected} expected"
                );
            }
            let start = reduction[quiet + held - 1];
            assert!((start - full).abs() < 1e-6);
            for n in 0..after {
                let expected = start * (-(n as f64 + 1.0) / release_frames).exp();
                let measured = reduction[quiet + held + n];
                assert!(
                    (measured - expected).abs() < 1e-3,
                    "release {release} ms, frame {n}: {measured} dB, {expected} expected"
                );
            }
            // One time constant in, each has gone 63% of the way.
            let most = 1.0 - (-1.0_f64).exp();
            let one = |frames: f64| frames.round() as usize - 1;
            let attacked = reduction[quiet + one(attack_frames)];
            assert!((attacked / full - most).abs() < 1e-4, "{attacked}");
            let released = reduction[quiet + held + one(release_frames)];
            assert!((1.0 - released / full - most).abs() < 1e-4, "{released}");
        }
    }

    /// Peak detection reads a sine at its peak, RMS 3 dB under it.
    #[test]
    fn rms_reads_a_sine_3_db_under_its_peak() {
        let input = sine(1000.0, 0.5, 48_000);
        let peak = 20.0 * 0.5_f64.log10();
        let rms = peak - 10.0 * 2.0_f64.log10();
        // Peaks are caught at once and held; RMS averages over 50 ms.
        let detectors = [(0.0, 0.1, 3000.0, peak), (1.0, 50.0, 50.0, rms)];
        for (detector, attack, release, level) in detectors {
            let compressor = device(&[
                (THRESHOLD, -30.0),
                (RATIO, 4.0),
                (KNEE, 0.0),
                (ATTACK, attack),
                (RELEASE, release),
                (DETECTOR, detector),
            ]);
            let rendered = run(&compressor).render(&[input.clone(), input.clone()], &[]);
            let design = -0.75 * (level + 30.0);
            let gains = gains(&input, &rendered[0]);
            for i in 43_200..48_000 {
                if input[i].abs() > 0.1 {
                    assert!(
                        (gains[i] - design).abs() < 0.01,
                        "{}: {} dB, {design} dB designed",
                        DETECTORS[detector as usize],
                        gains[i]
                    );
                }
            }
        }
    }

    #[test]
    fn the_mix_blends_the_compressed_signal_with_what_came_in() {
        // 20 dB over a 5:1 threshold is 16 dB down, and +6 dB of output
        // makes it 10; a quarter of that with three quarters of the dry.
        let settings = [
            (THRESHOLD, -20.0),
            (RATIO, 5.0),
            (KNEE, 0.0),
            (OUTPUT, 6.0),
            (ATTACK, 1.0),
            (RELEASE, 5.0),
        ];
        let input = steps(&[0.0], 4800);
        let mut compressor = device(&settings);
        compressor.set(MIX, 25.0);
        let rendered = run(&compressor).render(&[input.clone(), input.clone()], &[]);
        let wet = 10_f64.powf(-10.0 / 20.0);
        let expected = 0.25 * wet + 0.75;
        let measured = f64::from(rendered[0][4799] / input[4799]);
        assert!((measured - expected).abs() < 1e-6, "{measured} {expected}");
        // All dry, it's what came in, exactly.
        compressor.set(MIX, 0.0);
        let rendered = run(&compressor).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(rendered[0], input);
        // And all wet with nothing to do, the output gain alone.
        let quiet = steps(&[-40.0], 4800);
        compressor.set(MIX, 100.0);
        let rendered = run(&compressor).render(&[quiet.clone(), quiet.clone()], &[]);
        let gain = 10_f64.powf(6.0 / 20.0);
        for (out, input) in rendered[0].iter().zip(&quiet) {
            assert!((f64::from(*out) - f64::from(*input) * gain).abs() < 1e-9);
        }
    }

    #[test]
    fn both_channels_move_together() {
        // The left is loud and the right quiet, but the right is turned
        // down just as far.
        let left = steps(&[0.0], 9600);
        let right: Vec<f32> = steps(&[-40.0], 9600).iter().map(|s| -s * 0.5).collect();
        let compressor = device(&[(THRESHOLD, -20.0), (KNEE, 0.0)]);
        let rendered = run(&compressor).render(&[left.clone(), right.clone()], &[]);
        let left_gains = gains(&left, &rendered[0]);
        let right_gains = gains(&right, &rendered[1]);
        for (l, r) in left_gains.iter().zip(&right_gains) {
            assert!((l - r).abs() < 1e-5, "{l} {r}");
        }
        assert!((left_gains[9599] + 15.0).abs() < 1e-3);
    }

    #[test]
    fn a_new_compressor_leaves_a_signal_under_its_knee_alone() {
        // The default threshold is -18 dB and the knee 6 dB wide, so a
        // sine peaking at -22 dB passes exactly as it is.
        let input = sine(440.0, 10_f64.powf(-22.0 / 20.0), 9600);
        let rendered = run(&device(&[])).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(rendered[0], input);
        assert_eq!(rendered[1], input);
    }

    #[test]
    fn a_stray_nan_does_not_stick() {
        let mut input = sine(440.0, 0.9, 9600);
        let mut clean = input.clone();
        input[3000] = f32::NAN;
        clean[3000] = 0.0;
        for detector in [0.0, 1.0] {
            let compressor = device(&[(DETECTOR, detector)]);
            let with_nan = run(&compressor).render(&[input.clone(), input.clone()], &[]);
            let without = run(&compressor).render(&[clean.clone(), clean.clone()], &[]);
            assert!(with_nan[0][3000].is_nan());
            assert_eq!(with_nan[0][3001..], without[0][3001..]);
            assert_eq!(with_nan[1][3001..], without[1][3001..]);
        }
    }

    fn noise(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect()
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let compressor = device(&[(THRESHOLD, -24.0), (ATTACK, 2.0), (RELEASE, 40.0)]);
        let input = noise(9000);
        let events = [
            param(700, THRESHOLD, -40.0),
            modulation(2300, THRESHOLD, 12.0),
            param(3000, DETECTOR, 1.0),
            param(4100, RATIO, 12.0),
            param(5000, MIX, 40.0),
            param(6001, OUTPUT, -6.0),
            param(7000, ATTACK, 30.0),
        ];
        let render = |block| {
            run(&compressor)
                .with_block(block)
                .render(&[input.clone(), input.clone()], &events)
        };
        let reference = render(512);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let compressor = device(&[(THRESHOLD, -30.0), (DETECTOR, 1.0)]);
        let input = noise(4000);
        let new = || Box::new(Compressor::new(&compressor));
        let whole = Run::new(new()).render(&[input.clone(), input.clone()], &[]);
        let mut run = Run::new(new());
        let (first, second) = input.split_at(1500);
        let mut swapped = run.render(&[first.to_vec(), first.to_vec()], &[]);
        run.swap(new());
        let rest = run.render(&[second.to_vec(), second.to_vec()], &[]);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }

    #[test]
    fn a_switch_to_rms_finds_the_average_ready() {
        // The power is averaged during peak detection too, so once the
        // switch has faded and the reduction followed, it's as if it had
        // been on RMS all along.
        let input = sine(200.0, 0.8, 24_000);
        let settings = [(THRESHOLD, -30.0), (ATTACK, 1.0), (RELEASE, 5.0)];
        let switch = [param(9000, DETECTOR, 1.0)];
        let switched = run(&device(&settings)).render(&[input.clone(), input.clone()], &switch);
        let mut rms = device(&settings);
        rms.set(DETECTOR, 1.0);
        let all_along = run(&rms).render(&[input.clone(), input.clone()], &[]);
        assert_ne!(switched[0][..9000], all_along[0][..9000]);
        for i in 16_000..24_000 {
            assert!((switched[0][i] - all_along[0][i]).abs() < 1e-6, "{i}");
        }
    }
}
