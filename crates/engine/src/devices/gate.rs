//! The gate: see [`musicable_model::native::gate`]. The peak it opens on
//! is the [key](super::key)'s, which is the input unless Sidechain or the
//! key filters say otherwise.

use musicable_model::native::gate::{
    ATTACK, FLOOR, HOLD, KEY_HIGH_PASS, KEY_LOW_PASS, LISTEN, RELEASE, RETURN, SIDECHAIN, THRESHOLD,
};
use musicable_model::native::{NativeDevice, decibels_to_gain};

use super::key::{Key, KeyParams};
use super::{Params, Span, Values};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The parameters the DSP reads.
const PARAMS: [usize; 6] = [THRESHOLD, RETURN, ATTACK, HOLD, RELEASE, FLOOR];

/// Where the key's parameters are.
const KEY: KeyParams = KeyParams {
    sidechain: SIDECHAIN,
    high_pass: KEY_HIGH_PASS,
    low_pass: KEY_LOW_PASS,
    listen: LISTEN,
};

/// The gate's node.
#[derive(Debug, Clone)]
pub struct Gate {
    params: Params,
    key: Key,
    state: State,
    sample_rate: f64,
}

impl Gate {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            key: Key::new(device, KEY),
            state: State::default(),
            sample_rate: 48_000.0,
        }
    }
}

impl Node for Gate {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.key.prepare(config);
        self.sample_rate = f64::from(config.sample_rate);
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
            self.key.take_over(&previous.key);
            self.state = previous.state;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        self.key.take(&inputs, output.frames());
        let Self {
            params,
            key,
            state,
            sample_rate,
        } = self;
        params.run(ctx, |span| state.render(&span, output, key, *sample_rate));
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.key.reset();
        self.state = State::default();
    }
}

/// Where the gate has got to.
#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    /// Whether it's open, or opening.
    open: bool,
    /// How many frames in a row the peak has been below the closing level.
    below: u32,
    /// How far open it is, from 0.0, closed, to 1.0, open: the fade.
    opening: f64,
}

impl Default for State {
    /// Open, so a new gate lets the signal through until it's quiet.
    fn default() -> Self {
        Self {
            open: true,
            below: 0,
            opening: 1.0,
        }
    }
}

impl State {
    /// Gates one span of the output, in place, by the peak of `key`.
    fn render(
        &mut self,
        span: &Span<'_>,
        output: &mut AudioMut<'_>,
        key: &mut Key,
        sample_rate: f64,
    ) {
        let frames = span.frames.len();
        if frames == 0 {
            return;
        }
        let values = span.values;
        let mut key = key.span(values, span.frames.clone());
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
            let own = [left[i], right.as_ref().map_or(left[i], |right| right[i])];
            let keyed = key.as_mut().map_or(own, |key| key.at(i, own));
            let peak = match &right {
                Some(_) => keyed[0].abs().max(keyed[1].abs()),
                None => keyed[0].abs(),
            };
            let gain = self.gain(&settings, f64::from(peak));
            if gain != 1.0 {
                left[i] = (f64::from(left[i]) * gain) as f32;
                if let Some(right) = &mut right {
                    right[i] = (f64::from(right[i]) * gain) as f32;
                }
            }
            if let Some(key) = &key {
                key.listen(i, keyed[0], &mut left[i]);
                if let Some(right) = &mut right {
                    key.listen(i, keyed[1], &mut right[i]);
                }
            }
        }
    }

    /// Takes in the peak of a frame of the key, and returns its gain.
    fn gain(&mut self, settings: &Settings, peak: f64) -> f64 {
        if peak >= settings.open {
            self.open = true;
            self.below = 0;
        } else if peak >= settings.close {
            self.below = 0;
        } else {
            self.below = self.below.saturating_add(1);
            if f64::from(self.below) > settings.hold {
                self.open = false;
            }
        }
        self.opening = if self.open {
            (self.opening + settings.attack).min(1.0)
        } else {
            (self.opening - settings.release).max(0.0)
        };
        if self.opening == 1.0 {
            1.0
        } else {
            settings.floor + (1.0 - settings.floor) * self.opening
        }
    }
}

/// The settings on a frame, as the DSP uses them.
#[derive(Debug, Clone, Copy)]
struct Settings {
    /// The peak that opens it, as a linear level.
    open: f64,
    /// The peak it has to stay below to close.
    close: f64,
    /// How much further it opens on each frame while opening, from 0.0 to
    /// 1.0.
    attack: f64,
    /// How many frames it stays open once the peak is below `close`.
    hold: f64,
    /// How much further it closes on each frame while closing.
    release: f64,
    /// The gain when it's closed.
    floor: f64,
}

impl Settings {
    /// As the parameters are on frame `i` of a span.
    fn at(values: &Values, i: usize, sample_rate: f64) -> Self {
        let value = |p: usize| f64::from(values.frames(p)[i]);
        let frames = |ms: f64| ms * sample_rate / 1000.0;
        let threshold = value(THRESHOLD);
        Self {
            open: decibels_to_gain(threshold),
            close: decibels_to_gain(threshold - value(RETURN)),
            attack: 1.0 / frames(value(ATTACK)),
            hold: frames(value(HOLD)),
            release: 1.0 / frames(value(RELEASE)),
            floor: decibels_to_gain(value(FLOOR)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use musicable_model::native::NativeKind;

    use super::*;
    use crate::devices::testing::{Run, modulation, param};

    /// The defaults at 48 kHz: the attack, the hold and the release in
    /// frames, and the floor as a gain.
    const ATTACK_FRAMES: usize = 24;
    const HOLD_FRAMES: usize = 480;
    const RELEASE_FRAMES: usize = 4800;
    const FLOOR_GAIN: f64 = 1e-4;

    /// A gate set as `settings`, the rest at their defaults.
    fn device(settings: &[(usize, f64)]) -> NativeDevice {
        let mut device = NativeDevice::new(NativeKind::Gate);
        for &(param, value) in settings {
            device.set(param, value);
        }
        device
    }

    fn run(device: &NativeDevice) -> Run {
        Run::new(Box::new(Gate::new(device)))
    }

    /// Each `(level, frames)` in turn: a signal that's `level` dB on every
    /// frame, flipping sign, so its peak is that level throughout.
    fn steps(steps: &[(f64, usize)]) -> Vec<f32> {
        steps
            .iter()
            .flat_map(|&(db, frames)| {
                let size = 10_f64.powf(db / 20.0) as f32;
                (0..frames).map(move |i| if i % 2 == 0 { size } else { -size })
            })
            .collect()
    }

    /// Checks that `output` is `input` at `gain` on each of `frames`.
    #[track_caller]
    fn gained(
        input: &[f32],
        output: &[f32],
        frames: std::ops::Range<usize>,
        gain: impl Fn(usize) -> f64,
    ) {
        for i in frames {
            let expected = f64::from(input[i]) * gain(i);
            let error = (f64::from(output[i]) - expected).abs();
            assert!(
                error <= expected.abs() * 1e-6,
                "frame {i}: {} for {expected}",
                output[i]
            );
        }
    }

    /// The reference test: frame by frame, the gate holds, closes, rests
    /// at its floor and opens again as its parameters say.
    #[test]
    fn it_holds_then_fades_closed_and_opens_at_the_threshold() {
        // Loud, then 20 dB under the -40 dB threshold, then loud again.
        let input = steps(&[(-6.0, 1000), (-60.0, 10_000), (-6.0, 1000)]);
        let output = run(&device(&[])).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(output[0], output[1]);
        let output = &output[0];
        // A new gate is open, and stays open for the hold once the peak
        // falls.
        let closing = 1000 + HOLD_FRAMES;
        assert_eq!(output[..closing], input[..closing]);
        // Then it fades to the floor in a straight line over the release.
        let faded = |opening: f64| FLOOR_GAIN + (1.0 - FLOOR_GAIN) * opening;
        let closed = closing + RELEASE_FRAMES;
        gained(&input, output, closing..closed, |i| {
            faded(1.0 - (i + 1 - closing) as f64 / RELEASE_FRAMES as f64)
        });
        gained(&input, output, closed..11_000, |_| FLOOR_GAIN);
        // It opens on the first loud frame, over the attack.
        gained(&input, output, 11_000..11_000 + ATTACK_FRAMES, |i| {
            faded((i + 1 - 11_000) as f64 / ATTACK_FRAMES as f64)
        });
        assert_eq!(output[11_024..], input[11_024..]);
    }

    #[test]
    fn return_keeps_it_open_until_the_peak_falls_well_below() {
        // Between the threshold and 3 dB under it, it stays as it was:
        // open after a loud start, closed after a quiet spell.
        let input = steps(&[
            (-6.0, 1000),
            (-42.0, 20_000),
            (-50.0, 10_000),
            (-42.0, 10_000),
            (-39.0, 1000),
        ]);
        let output = run(&device(&[])).render(&[input.clone(), input.clone()], &[]);
        let output = &output[0];
        assert_eq!(output[..21_000], input[..21_000]);
        assert_eq!(
            output[21_000 + HOLD_FRAMES - 1],
            input[21_000 + HOLD_FRAMES - 1]
        );
        assert!(output[21_000 + HOLD_FRAMES].abs() < input[21_000 + HOLD_FRAMES].abs());
        let closed = 21_000 + HOLD_FRAMES + RELEASE_FRAMES;
        gained(&input, output, closed..41_000, |_| FLOOR_GAIN);
        // Over the threshold, it opens.
        assert_eq!(output[41_100..], input[41_100..]);
    }

    #[test]
    fn hold_attack_release_and_floor_follow_their_settings() {
        let settings = [(HOLD, 0.0), (ATTACK, 0.02), (RELEASE, 1.0), (FLOOR, -20.0)];
        let input = steps(&[(-6.0, 1000), (-60.0, 1000), (-6.0, 1000)]);
        let output = run(&device(&settings)).render(&[input.clone(), input.clone()], &[]);
        let output = &output[0];
        // No hold: it starts to close on the first quiet frame, and takes
        // 48 frames to close.
        assert_eq!(output[..1000], input[..1000]);
        let faded = |opening: f64| 0.1 + 0.9 * opening;
        gained(&input, output, 1000..1048, |i| {
            faded(1.0 - (i + 1 - 1000) as f64 / 48.0)
        });
        gained(&input, output, 1048..2000, |_| 0.1);
        // The shortest attack, under a frame, opens it at once.
        assert_eq!(output[2000..], input[2000..]);
    }

    #[test]
    fn one_loud_channel_opens_both() {
        let left = steps(&[(-6.0, 2000), (-60.0, 8000)]);
        let right = steps(&[(-60.0, 10_000)]);
        let output = run(&device(&[])).render(&[left.clone(), right.clone()], &[]);
        assert_eq!(output[0][..2000], left[..2000]);
        assert_eq!(output[1][..2000 + HOLD_FRAMES], right[..2000 + HOLD_FRAMES]);
        gained(&right, &output[1], 8000..10_000, |_| FLOOR_GAIN);
        gained(&left, &output[0], 8000..10_000, |_| FLOOR_GAIN);
    }

    #[test]
    fn a_new_gate_passes_a_loud_signal_unchanged() {
        let input: Vec<f32> = (0..9600)
            .map(|i| (0.5 * (TAU * 440.0 * i as f64 / 48_000.0).sin()) as f32)
            .collect();
        let output = run(&device(&[])).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(output[0], input);
        assert_eq!(output[1], input);
        // Nor does a gate whose floor is 0 dB, open or closed.
        let quiet = steps(&[(-70.0, 9600)]);
        let output = run(&device(&[(FLOOR, 0.0)])).render(&[quiet.clone(), quiet.clone()], &[]);
        assert_eq!(output[0], quiet);
    }

    /// Bursts of a decaying tone with quiet hiss between: a drum to gate.
    fn hits(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                let t = (i % 6000) as f64 / 48_000.0;
                let hit = (TAU * 180.0 * t).sin() * (-t / 0.02).exp() * 0.8;
                let hiss = ((i * 7919) % 2000) as f64 / 1000.0 - 1.0;
                (hit + hiss * 0.002) as f32
            })
            .collect()
    }

    #[test]
    fn blocks_of_any_length_render_the_same() {
        let gate = device(&[(THRESHOLD, -30.0), (HOLD, 5.0), (RELEASE, 20.0)]);
        let input = hits(20_000);
        let events = [
            param(700, THRESHOLD, -20.0),
            modulation(2300, THRESHOLD, -6.0),
            param(5000, RETURN, 10.0),
            param(8001, HOLD, 0.0),
            param(11_000, FLOOR, -30.0),
            param(15_000, ATTACK, 5.0),
            param(17_000, RELEASE, 2.0),
        ];
        let render = |block| {
            run(&gate)
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
        let gate = device(&[(THRESHOLD, -30.0), (HOLD, 5.0), (RELEASE, 20.0)]);
        let input = hits(20_000);
        let new = || Box::new(Gate::new(&gate));
        let whole = Run::new(new()).render(&[input.clone(), input.clone()], &[]);
        // Swapped while it fades closed after the first hit.
        let mut run = Run::new(new());
        let (first, second) = input.split_at(4000);
        let mut swapped = run.render(&[first.to_vec(), first.to_vec()], &[]);
        run.swap(new());
        let rest = run.render(&[second.to_vec(), second.to_vec()], &[]);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }
    /// With Sidechain on, the side input's peak opens and closes it, frame
    /// by frame as it would on its own: a kick opens a pad on each hit.
    #[test]
    fn sidechain_opens_it_on_the_side_inputs_peak() {
        let kick = steps(&[(-6.0, 1000), (-60.0, 10_000), (-6.0, 1000), (-60.0, 6000)]);
        // Loud enough to hold it open on its own.
        let pad = steps(&[(-20.0, kick.len())]);
        let alone = run(&device(&[])).render(&[kick.clone(), kick.clone()], &[]);
        let keyed = run(&device(&[(SIDECHAIN, 1.0)])).render_keyed(
            &[pad.clone(), pad.clone()],
            Some(&[kick.clone(), kick.clone()]),
            &[],
        );
        assert_eq!(keyed[0], keyed[1]);
        for i in 0..kick.len() {
            let expected = f64::from(alone[0][i]) / f64::from(kick[i]);
            gained(&pad, &keyed[0], i..i + 1, |_| expected);
        }
        // So it's closed between the hits, at its floor.
        gained(&pad, &keyed[0], 7000..11_000, |_| FLOOR_GAIN);
        gained(
            &pad,
            &keyed[0],
            12_000 + HOLD_FRAMES + RELEASE_FRAMES..18_000,
            |_| FLOOR_GAIN,
        );
    }

    #[test]
    fn listen_plays_the_side_input_with_sidechain_on() {
        let pad = steps(&[(-20.0, 4000)]);
        let kick = hits(4000);
        let gate = device(&[(SIDECHAIN, 1.0), (LISTEN, 1.0)]);
        let output = run(&gate).render_keyed(
            &[pad.clone(), pad.clone()],
            Some(&[kick.clone(), kick.clone()]),
            &[],
        );
        assert_eq!(output, [kick.clone(), kick]);
    }

    /// A key low-pass lets a drum's gate open on its thump, not on the
    /// hi-hat in its microphone.
    #[test]
    fn a_key_low_pass_keeps_it_shut_on_highs() {
        let hat: Vec<f32> = (0..20_000)
            .map(|i| (0.3 * (TAU * 9000.0 * i as f64 / 48_000.0).sin()) as f32)
            .collect();
        let open = run(&device(&[])).render(&[hat.clone(), hat.clone()], &[]);
        assert_eq!(open[0], hat);
        let filtered = device(&[(KEY_LOW_PASS, 200.0)]);
        let shut = run(&filtered).render(&[hat.clone(), hat.clone()], &[]);
        gained(
            &hat,
            &shut[0],
            HOLD_FRAMES + RELEASE_FRAMES + 50..20_000,
            |_| FLOOR_GAIN,
        );
    }

    #[test]
    fn blocks_of_any_length_render_the_same_with_a_key() {
        let gate = device(&[(THRESHOLD, -30.0), (HOLD, 5.0), (RELEASE, 20.0)]);
        let input = hits(20_000);
        let side: Vec<f32> = hits(20_000).iter().rev().copied().collect();
        let events = [
            param(900, SIDECHAIN, 1.0),
            param(3000, KEY_HIGH_PASS, 400.0),
            param(6000, KEY_LOW_PASS, 3000.0),
            param(9000, LISTEN, 1.0),
            param(10_500, LISTEN, 0.0),
            param(13_001, SIDECHAIN, 0.0),
            param(16_000, KEY_HIGH_PASS, 20.0),
        ];
        let render = |block| {
            run(&gate).with_block(block).render_keyed(
                &[input.clone(), input.clone()],
                Some(&[side.clone(), side.clone()]),
                &events,
            )
        };
        let reference = render(512);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }
}
