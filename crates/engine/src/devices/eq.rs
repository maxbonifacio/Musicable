//! The EQ: see [`musicable_model::native::eq`].

use musicable_model::native::eq::{BANDS, Band, FREQ, GAIN, ON, OUTPUT, PER_BAND, Q, SHAPE, Shape};
use musicable_model::native::{NativeDevice, decibels_to_gain};

use super::svf::{Coefficients, State};
use super::{Params, Span, Values};
use crate::{AudioMut, EngineConfig, Inputs, Node, ProcessContext};

/// The most sections a band has: a 48 dB cut's four.
const SECTIONS: usize = 4;

/// The EQ's node.
#[derive(Debug, Clone)]
pub struct Equalizer {
    params: Params,
    bands: [BandFilter; BANDS],
    /// The signal into a band that's fading in or out, for each channel,
    /// `max_block` frames each.
    dry: Vec<f32>,
    sample_rate: f64,
}

impl Equalizer {
    pub fn new(device: &NativeDevice) -> Self {
        Self {
            params: Params::new(device),
            bands: [BandFilter::default(); BANDS],
            dry: Vec::new(),
            sample_rate: 48_000.0,
        }
    }
}

impl Node for Equalizer {
    fn prepare(&mut self, config: &EngineConfig) {
        self.params.prepare(config);
        self.dry = vec![0.0; 2 * config.max_block];
        self.sample_rate = f64::from(config.sample_rate);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Self>() {
            self.params.take_over(&previous.params);
            // Its filters carry on as they were, tuned as they were.
            self.bands = previous.bands;
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let Self {
            params,
            bands,
            dry,
            sample_rate,
        } = self;
        params.run(ctx, |span| {
            let mut out = output.range_mut(span.frames.clone());
            for (b, band) in bands.iter_mut().enumerate() {
                band.render(b, &span, &mut out, dry, *sample_rate);
            }
            let gain = span.values.frames(OUTPUT);
            if span.values.steady(OUTPUT) {
                if gain[0] != 0.0 {
                    let gain = decibels_to_gain(f64::from(gain[0])) as f32;
                    for channel in out.channels_mut() {
                        channel.iter_mut().for_each(|s| *s *= gain);
                    }
                }
            } else {
                for channel in out.channels_mut() {
                    for (sample, &db) in channel.iter_mut().zip(gain) {
                        *sample *= decibels_to_gain(f64::from(db)) as f32;
                    }
                }
            }
        });
    }
}

/// One band's filters: its sections' coefficients, and each channel's
/// memory of them.
#[derive(Debug, Clone, Copy)]
struct BandFilter {
    sections: [Coefficients; SECTIONS],
    count: usize,
    /// The shape, frequency, gain and Q the coefficients are for, if
    /// they're for any yet.
    tuned: Option<[f32; 4]>,
    state: [[State; SECTIONS]; 2],
}

impl Default for BandFilter {
    fn default() -> Self {
        Self {
            sections: [Coefficients::new(&FLAT, 1000.0, 48_000.0); SECTIONS],
            count: 0,
            tuned: None,
            state: [[State::default(); SECTIONS]; 2],
        }
    }
}

/// A section that changes nothing, to fill unused slots.
const FLAT: musicable_model::native::eq::Section = musicable_model::native::eq::Section {
    kind: musicable_model::native::eq::SectionKind::Bell,
    q: 1.0,
    a: 1.0,
};

impl BandFilter {
    /// Filters one span of the output in place as band `b`.
    fn render(
        &mut self,
        b: usize,
        span: &Span<'_>,
        out: &mut AudioMut<'_>,
        dry: &mut [f32],
        sample_rate: f64,
    ) {
        let values = span.values;
        let param = |p: usize| b * PER_BAND + p;
        let frames = span.frames.len();
        if frames == 0 {
            return;
        }
        let on = values.frames(param(ON));
        if values.steady(param(ON)) && on[0] == 0.0 {
            // Off, it starts afresh when it's next turned on.
            self.state = [[State::default(); SECTIONS]; 2];
            return;
        }
        let fading = !(values.steady(param(ON)) && on[0] == 1.0);
        let channels = out.channels().min(2);
        if fading {
            for c in 0..channels {
                dry[c * frames..][..frames].copy_from_slice(out.channel(c));
            }
        }
        let tuning = [SHAPE, FREQ, GAIN, Q].map(param);
        if tuning.iter().all(|&p| values.steady(p)) {
            self.tune(tuning.map(|p| values.first(p)), sample_rate);
            for c in 0..channels {
                let channel = out.channel_mut(c);
                for (coefficients, state) in
                    self.sections[..self.count].iter().zip(&mut self.state[c])
                {
                    state.run(coefficients, channel);
                }
            }
        } else {
            // Tuned anew each frame, as it moves.
            for i in 0..frames {
                self.tune(at(values, tuning, i), sample_rate);
                for c in 0..channels {
                    let mut x = f64::from(out.channel(c)[i]);
                    for (coefficients, state) in
                        self.sections[..self.count].iter().zip(&mut self.state[c])
                    {
                        x = state.tick(coefficients, x);
                    }
                    out.channel_mut(c)[i] = x as f32;
                }
            }
        }
        if fading {
            for c in 0..channels {
                let dry = &dry[c * frames..][..frames];
                for ((wet, &dry), &on) in out.channel_mut(c).iter_mut().zip(dry).zip(on) {
                    *wet = dry + on * (*wet - dry);
                }
            }
        }
    }

    /// Works out the coefficients for the band set as `tuning`, unless
    /// they're for it already.
    fn tune(&mut self, tuning: [f32; 4], sample_rate: f64) {
        if self.tuned == Some(tuning) {
            return;
        }
        let [shape, freq, gain, q] = tuning.map(f64::from);
        let band = Band {
            on: true,
            shape: Shape::from_value(shape),
            freq,
            gain,
            q,
        };
        let corner = band.corner(sample_rate);
        let before = self.count;
        self.count = 0;
        for section in band.sections() {
            self.sections[self.count] = Coefficients::new(&section, corner, sample_rate);
            self.count += 1;
        }
        // Sections a steeper shape takes up start from silence, not from
        // whatever they held when they were last used.
        for state in &mut self.state {
            for section in state.iter_mut().take(self.count).skip(before) {
                *section = State::default();
            }
        }
        self.tuned = Some(tuning);
    }
}

/// The values of `params` on frame `i` of the span.
fn at(values: &Values, params: [usize; 4], i: usize) -> [f32; 4] {
    params.map(|p| values.frames(p)[i])
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_1_SQRT_2, PI};

    use musicable_model::native::NativeKind;
    use musicable_model::native::eq::response;

    use super::*;
    use crate::devices::testing::{Run, modulation, param};

    fn shape(shape: Shape) -> f64 {
        Shape::ALL.iter().position(|&s| s == shape).unwrap() as f64
    }

    /// An EQ with `bands` on, set as given, and the others off.
    fn device(bands: &[(usize, Shape, f64, f64, f64)]) -> NativeDevice {
        let mut eq = NativeDevice::new(NativeKind::Eq);
        for b in 0..BANDS {
            eq.values[b * PER_BAND + ON] = 0.0;
        }
        for &(b, s, freq, gain, q) in bands {
            let at = b * PER_BAND;
            eq.values[at + ON] = 1.0;
            eq.values[at + SHAPE] = shape(s);
            eq.values[at + FREQ] = freq;
            eq.values[at + GAIN] = gain;
            eq.values[at + Q] = q;
        }
        eq
    }

    /// The node's impulse response, `frames` long, on the left channel.
    fn impulse_response(device: &NativeDevice, sample_rate: u32, frames: usize) -> Vec<f32> {
        let mut input = vec![0.0; frames];
        input[0] = 1.0;
        let mut run = Run::new(Box::new(Equalizer::new(device))).with_sample_rate(sample_rate);
        run.render(&[input.clone(), input], &[]).swap_remove(0)
    }

    /// The gain of `impulse` at `freq` Hz, as a linear magnitude: its
    /// discrete-time Fourier transform there.
    fn gain_at(impulse: &[f32], freq: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * freq / sample_rate;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &h) in impulse.iter().enumerate() {
            let phase = w * n as f64;
            re += f64::from(h) * phase.cos();
            im -= f64::from(h) * phase.sin();
        }
        re.hypot(im)
    }

    fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    /// The reference test: the EQ's measured response matches its design
    /// to within a thousandth of a dB, from 20 Hz to 20 kHz, for every
    /// shape, at two sample rates.
    #[test]
    fn the_measured_response_matches_the_design() {
        let settings = [
            (0, Shape::LowCut12, 40.0, 0.0, FRAC_1_SQRT_2),
            (1, Shape::LowShelf, 150.0, -4.0, 0.9),
            (2, Shape::Bell, 400.0, 6.0, 1.4),
            (3, Shape::Notch, 1000.0, 0.0, 4.0),
            (4, Shape::Bell, 2500.0, -9.0, 0.5),
            (5, Shape::HighShelf, 6000.0, 3.0, FRAC_1_SQRT_2),
            (6, Shape::HighCut24, 15_000.0, 0.0, 1.2),
        ];
        for sample_rate in [44_100, 96_000] {
            let mut eq = device(&settings);
            eq.values[OUTPUT] = -1.5;
            let impulse = impulse_response(&eq, sample_rate, 1 << 16);
            let fs = f64::from(sample_rate);
            let mut freq = 20.0;
            while freq < 20_000.0 {
                let measured = db(gain_at(&impulse, freq, fs));
                let design = db(response(&eq.values, freq, fs));
                // The notch's own frequency is a hole, -inf in theory.
                if (freq - 1000.0).abs() > 30.0 {
                    let error = (measured - design).abs();
                    assert!(
                        error < 1e-3,
                        "{sample_rate} Hz, {freq:.0} Hz: {measured} {design}"
                    );
                }
                freq *= 1.03;
            }
        }
    }

    #[test]
    fn steep_cuts_match_the_design_too() {
        for s in [
            Shape::LowCut24,
            Shape::LowCut48,
            Shape::HighCut12,
            Shape::HighCut48,
        ] {
            let eq = device(&[(0, s, 1000.0, 0.0, 2.0)]);
            let impulse = impulse_response(&eq, 48_000, 1 << 15);
            for freq in [50.0, 300.0, 900.0, 1000.0, 1100.0, 3000.0, 15_000.0] {
                let measured = gain_at(&impulse, freq, 48_000.0);
                let design = response(&eq.values, freq, 48_000.0);
                // Far down the slope, the impulse's own rounding to f32
                // shows: there it's within a millionth of full scale.
                let close = if design > 1e-3 {
                    (db(measured) - db(design)).abs() < 1e-3
                } else {
                    (measured - design).abs() < 1e-6
                };
                assert!(close, "{s:?} at {freq}: {measured} {design}");
            }
        }
    }

    #[test]
    fn a_new_eq_passes_the_signal_unchanged() {
        let input: Vec<f32> = (0..4800)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let eq = NativeDevice::new(NativeKind::Eq);
        let output =
            Run::new(Box::new(Equalizer::new(&eq))).render(&[input.clone(), input.clone()], &[]);
        assert_eq!(output[0], input);
        assert_eq!(output[1], input);
    }

    #[test]
    fn a_new_node_carries_on_where_the_old_one_was() {
        let eq = device(&[
            (0, Shape::LowCut48, 80.0, 0.0, FRAC_1_SQRT_2),
            (2, Shape::Bell, 800.0, 6.0, 2.0),
        ]);
        let input: Vec<f32> = (0..4000)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let new = || Box::new(Equalizer::new(&eq));
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
    fn blocks_of_any_length_render_the_same() {
        let eq = device(&[
            (2, Shape::Bell, 800.0, 6.0, 2.0),
            (7, Shape::HighCut48, 5000.0, 0.0, 0.7),
        ]);
        let input: Vec<f32> = (0..9000)
            .map(|i| ((i * 7919) % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        let events = [
            param(700, 2 * PER_BAND + FREQ, 2000.0),
            modulation(2300, 2 * PER_BAND + GAIN, -12.0),
            param(5000, 4 * PER_BAND + ON, 1.0),
            param(6001, OUTPUT, -6.0),
        ];
        let render = |block| {
            Run::new(Box::new(Equalizer::new(&eq)))
                .with_block(block)
                .render(&[input.clone(), input.clone()], &events)
        };
        let reference = render(512);
        for block in [1, 7, 64, 333] {
            assert_eq!(render(block), reference, "blocks of {block}");
        }
    }

    #[test]
    fn moving_a_band_changes_it_smoothly() {
        // A sine at 1 kHz under a bell that sweeps onto it: no jumps.
        let eq = device(&[(0, Shape::Bell, 200.0, 12.0, 2.0)]);
        let input: Vec<f32> = (0..24_000)
            .map(|i| (2.0 * PI * 1000.0 * i as f64 / 48_000.0).sin() as f32 * 0.25)
            .collect();
        let events = [param(4000, FREQ, 1000.0), param(12_000, ON, 0.0)];
        let output = Run::new(Box::new(Equalizer::new(&eq)))
            .render(&[input.clone(), input.clone()], &events)
            .swap_remove(0);
        let steps = output
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        // A full-scale 1 kHz sine moves by 0.13 a frame at most; 12 dB up,
        // a quarter-scale one by about as much.
        assert!(steps < 0.15, "{steps}");
        // Up 12 dB on the bell's frequency, once it has got there.
        let peak = output[8000..10_000]
            .iter()
            .fold(0.0, |m: f32, s| m.max(s.abs()));
        assert!((db(f64::from(peak) / 0.25) - 12.0).abs() < 0.1, "{peak}");
        // And back to the signal as it was once the band is off.
        assert_eq!(output[20_000..], input[20_000..]);
    }
}
