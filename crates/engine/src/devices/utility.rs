//! The utility: see [`musicable_model::native::utility`].

use std::f64::consts::PI;

use musicable_model::mixer::pan_gains;
use musicable_model::native::NativeDevice;
use musicable_model::native::utility::{
    DC, DC_CUTOFF, GAIN, INVERT_LEFT, INVERT_RIGHT, MUTE, PAN, WIDTH,
};

use super::{Params, Span};
use crate::{AudioMut, Automation, EngineConfig, Inputs, Node, ParamId, ProcessContext};

/// The utility's node.
#[derive(Debug, Clone)]
pub struct Utility {
    params: Params,
    /// The DC filter's memory for each channel: the last input and output.
    dc: [(f64, f64); 2],
    /// The DC filter's pole.
    pole: f64,
}

impl Utility {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            dc: [(0.0, 0.0); 2],
            pole: 0.0,
        }
    }
}

impl Node for Utility {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.pole = (-2.0 * PI * DC_CUTOFF / f64::from(config.sample_rate)).exp();
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        self.params.automate(param, automation)
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            self.dc = previous.dc;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self {
            params, dc, pole, ..
        } = self;
        params.run(ctx, |span| render(&span, output, dc, *pole));
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        self.params.flush(ctx.events);
    }

    fn reset(&mut self) {
        self.params.reset();
        self.dc = [(0.0, 0.0); 2];
    }
}

/// Renders one span of the output, in place.
fn render(span: &Span<'_>, output: &mut AudioMut<'_>, dc: &mut [(f64, f64); 2], pole: f64) {
    let values = span.values;
    let mut out = output.range_mut(span.frames.clone());
    let filtering = !(values.steady(DC) && values.first(DC) == 0.0);
    if !filtering {
        // Off, it starts afresh next time, from the signal as it is.
        *dc = [(0.0, 0.0); 2];
    }
    if out.channels() < 2 {
        let channel = out.channel_mut(0);
        for (i, sample) in channel.iter_mut().enumerate() {
            let level = values.frames(GAIN)[i] * (1.0 - values.frames(MUTE)[i]);
            *sample *= level;
        }
        return;
    }
    let (gain, mute, pan, width) = (
        values.frames(GAIN),
        values.frames(MUTE),
        values.frames(PAN),
        values.frames(WIDTH),
    );
    let (invert_left, invert_right, filter) = (
        values.frames(INVERT_LEFT),
        values.frames(INVERT_RIGHT),
        values.frames(DC),
    );
    for i in 0..span.frames.len() {
        let mut frame = [out.channel(0)[i], out.channel(1)[i]];
        if filtering && filter[i] == 0.0 {
            // Off on this frame, it starts afresh, as when it's off for a
            // whole span: so it sounds the same however the blocks fall.
            *dc = [(0.0, 0.0); 2];
        } else if filtering {
            for (sample, (last_in, last_out)) in frame.iter_mut().zip(dc.iter_mut()) {
                let x = f64::from(*sample);
                let y = x - *last_in + pole * *last_out;
                (*last_in, *last_out) = (x, y);
                *sample += filter[i] * (y as f32 - *sample);
            }
        }
        // Each switch fades through silence to the other polarity.
        frame[0] *= 1.0 - 2.0 * invert_left[i];
        frame[1] *= 1.0 - 2.0 * invert_right[i];
        // Mid and side, as gains on each channel, which are exactly 1 and
        // 0 at 100%.
        let w = width[i] / 100.0;
        let (same, other) = ((1.0 + w) / 2.0, (1.0 - w) / 2.0);
        let [left, right] = frame;
        let frame = [left * same + right * other, right * same + left * other];
        let level = gain[i] * (1.0 - mute[i]);
        let [pan_left, pan_right] = pan_gains(pan[i]);
        out.channel_mut(0)[i] = frame[0] * pan_left * level;
        out.channel_mut(1)[i] = frame[1] * pan_right * level;
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::native::NativeKind;

    use super::*;
    use crate::devices::testing::{Run, param};

    fn utility() -> Run {
        Run::new(Box::new(Utility::new(&NativeDevice::new(
            NativeKind::Utility,
        ))))
    }

    /// Left a sine, right a different one, so every stage shows.
    fn input(frames: usize) -> [Vec<f32>; 2] {
        let wave = |freq: f32| -> Vec<f32> {
            (0..frames)
                .map(|i| (i as f32 * freq * std::f32::consts::TAU / 48_000.0).sin() * 0.5)
                .collect()
        };
        [wave(440.0), wave(1100.0)]
    }

    #[test]
    fn a_new_utility_passes_the_signal_unchanged() {
        let input = input(4800);
        let output = utility().render(&input, &[]);
        assert_eq!(output, input);
    }

    #[test]
    fn gain_width_pan_and_polarity_land_exactly() {
        let input = input(4800);
        let [l, r] = &input;
        // Settings sent before the first frame apply at once.
        let set = |events: &[(usize, f64)]| {
            let events: Vec<_> = events.iter().map(|&(p, v)| param(0, p, v)).collect();
            utility().render(&input, &events)
        };
        let half = set(&[(GAIN, 0.5)]);
        assert!(half[0].iter().zip(l).all(|(o, i)| *o == i * 0.5));
        let mono = set(&[(WIDTH, 0.0)]);
        for i in 0..l.len() {
            let mid = (l[i] + r[i]) * 0.5;
            assert!((mono[0][i] - mid).abs() < 1e-7 && (mono[1][i] - mid).abs() < 1e-7);
        }
        let inverted = set(&[(INVERT_LEFT, 1.0)]);
        assert!(inverted[0].iter().zip(l).all(|(o, i)| *o == -i));
        assert_eq!(inverted[1], *r);
        let panned = set(&[(PAN, 0.5)]);
        assert!(panned[0].iter().zip(l).all(|(o, i)| *o == i * 0.5));
        assert_eq!(panned[1], *r);
        let muted = set(&[(MUTE, 1.0)]);
        assert!(muted.iter().flatten().all(|&s| s == 0.0));
    }

    #[test]
    fn switches_fade_rather_than_click() {
        let input = [vec![0.5; 4800], vec![0.5; 4800]];
        let output = utility().render(&input, &[param(1000, MUTE, 1.0)]);
        // 10 ms at 48 kHz.
        assert_eq!(output[0][999], 0.5);
        assert!((output[0][1000] - 0.5 * (1.0 - 1.0 / 480.0)).abs() < 1e-6);
        assert!(output[0][1240] > 0.2 && output[0][1240] < 0.3);
        assert_eq!(output[0][1479], 0.0);
        let steps = output[0].windows(2).map(|w| (w[1] - w[0]).abs());
        assert!(steps.fold(0.0, f32::max) < 0.002);
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let mut device = NativeDevice::new(NativeKind::Utility);
        device.values[DC] = 1.0;
        let input = input(4000);
        let new = || Box::new(Utility::new(&device));
        let whole = Run::new(new()).render(&input, &[param(800, GAIN, 0.25)]);
        let mut run = Run::new(new());
        let first = input.each_ref().map(|c| c[..1000].to_vec());
        let second = input.each_ref().map(|c| c[1000..].to_vec());
        let mut swapped = run.render(&first, &[param(800, GAIN, 0.25)]);
        // Halfway through the gain's ramp, a node made from the project,
        // which has the new gain.
        let mut changed = device.clone();
        changed.values[GAIN] = 0.25;
        run.swap(Box::new(Utility::new(&changed)));
        let rest = run.render(&second, &[]);
        for (channel, rest) in swapped.iter_mut().zip(rest) {
            channel.extend(rest);
        }
        assert_eq!(swapped, whole);
    }

    #[test]
    fn the_dc_filter_takes_out_an_offset() {
        let input = [vec![0.25; 48_000], vec![-0.25; 48_000]];
        let output = utility().render(&input, &[param(0, DC, 1.0)]);
        // A 5 Hz pole: down by e in 32 ms, and to nothing in a second.
        assert!(output[0][0] > 0.24);
        assert!(output[0][47_999].abs() < 1e-6 && output[1][47_999].abs() < 1e-6);
    }
}
