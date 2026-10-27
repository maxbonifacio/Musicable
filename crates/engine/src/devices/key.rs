//! The key that the compressor's and the gate's levels follow: see
//! [`musicable_model::native::compressor`].
//!
//! It's the device's own input, or with Sidechain on and something wired to
//! the side input, the side input, crossfading as the switch ramps. A
//! high-pass and a low-pass shape it, each a Butterworth section of the
//! EQ's [state variable filter](super::svf), 12 dB per octave. At the ends
//! of their ranges they cut nothing, so they're left out: one fades out
//! over 10 ms as it gets there, and in again as it leaves, so the key
//! doesn't jump. Listen plays the key in place of the output, crossfading
//! as it ramps.

use std::f64::consts::FRAC_1_SQRT_2;
use std::ops::Range;

use musicable_model::native::NativeDevice;
use musicable_model::native::eq::{HIGHEST, Section, SectionKind};

use super::Values;
use super::svf::{Coefficients, State};
use crate::{AudioBuffer, EngineConfig, Inputs};

/// Where a device's key parameters are in its table.
#[derive(Debug, Clone, Copy)]
pub struct KeyParams {
    pub sidechain: usize,
    pub high_pass: usize,
    pub low_pass: usize,
    pub listen: usize,
}

/// A device's key: its side input over a block, and its filters.
#[derive(Debug, Clone)]
pub struct Key {
    params: KeyParams,
    /// What comes into the side input over the block, summed.
    side: AudioBuffer,
    /// Whether anything is wired to the side input this block.
    wired: bool,
    high_pass: Filter,
    low_pass: Filter,
    sample_rate: f64,
    /// How far a filter fades in or out on each frame.
    fade: f32,
}

impl Key {
    /// The key of `device`, whose key parameters are at `params`.
    pub fn new(device: &NativeDevice, params: KeyParams) -> Self {
        let info = device.params();
        Self {
            params,
            side: AudioBuffer::default(),
            wired: false,
            high_pass: Filter::new(SectionKind::HighPass, info[params.high_pass].min),
            low_pass: Filter::new(SectionKind::LowPass, info[params.low_pass].max),
            sample_rate: 48_000.0,
            fade: 1.0 / 480.0,
        }
    }

    /// From [`Node::prepare`](crate::Node::prepare): makes room for a
    /// block of the side input.
    pub fn prepare(&mut self, config: &EngineConfig) {
        self.side = AudioBuffer::new(2, config.max_block);
        self.sample_rate = f64::from(config.sample_rate);
        self.fade = 100.0 / config.sample_rate as f32;
        self.high_pass.tuned = None;
        self.low_pass.tuned = None;
    }

    /// From [`Node::adopt`](crate::Node::adopt), with the key of the node
    /// this one replaces: its filters carry on where they were.
    pub fn take_over(&mut self, previous: &Key) {
        for (filter, previous) in [
            (&mut self.high_pass, &previous.high_pass),
            (&mut self.low_pass, &previous.low_pass),
        ] {
            filter.mix = previous.mix;
            filter.state = previous.state;
        }
    }

    /// From [`Node::reset`](crate::Node::reset): the filters start afresh.
    pub fn reset(&mut self) {
        for filter in [&mut self.high_pass, &mut self.low_pass] {
            filter.mix = None;
            filter.state = SILENT;
        }
    }

    /// Takes in what comes into the side input over the block, `frames`
    /// long, from [`Node::process`](crate::Node::process).
    pub fn take(&mut self, inputs: &Inputs<'_>, frames: usize) {
        let mut side = inputs.side(0).peekable();
        self.wired = side.peek().is_some();
        if self.wired {
            let mut sum = self.side.view_mut(frames);
            sum.clear();
            for input in side {
                sum.add_from(input);
            }
        }
    }

    /// The key over a span with `values`, the block's `frames`: `None`
    /// where it's the device's own input, untouched, and Listen is off, so
    /// the device can go on as it would without a key.
    pub fn span<'a>(&'a mut self, values: &'a Values, frames: Range<usize>) -> Option<KeySpan<'a>> {
        let Self {
            params: p,
            side,
            wired,
            high_pass,
            low_pass,
            sample_rate,
            fade,
        } = self;
        let off = |param: usize| values.steady(param) && values.first(param) == 0.0;
        let side: &AudioBuffer = side;
        let side = (*wired && !off(p.sidechain)).then(move || {
            let side = side.view(side.capacity());
            [0, 1].map(|c| &side.channel(c)[frames.clone()])
        });
        let mut filters_out = true;
        for (filter, param) in [(&mut *high_pass, p.high_pass), (&mut *low_pass, p.low_pass)] {
            if values.steady(param) {
                filter.tune(values.first(param), *sample_rate);
            }
            filters_out &= values.steady(param) && !filter.on && filter.mix == Some(0.0);
        }
        if side.is_none() && filters_out && off(p.listen) {
            return None;
        }
        Some(KeySpan {
            values,
            params: *p,
            side,
            filters: [high_pass, low_pass],
            sample_rate: *sample_rate,
            fade: *fade,
        })
    }
}

/// A device's key over a span: see [`Key::span`].
pub struct KeySpan<'a> {
    values: &'a Values,
    params: KeyParams,
    /// The side input over the span, if it's wired and Sidechain is on.
    side: Option<[&'a [f32]; 2]>,
    filters: [&'a mut Filter; 2],
    sample_rate: f64,
    fade: f32,
}

impl KeySpan<'_> {
    /// The key on the span's frame `i`, where the device's own input is
    /// `own`, a sample for each channel, with a mono input on both.
    pub fn at(&mut self, i: usize, own: [f32; 2]) -> [f32; 2] {
        let p = self.params;
        let mut key = own;
        if let Some(side) = self.side {
            let on = self.values.frames(p.sidechain)[i];
            let side = [side[0][i], side[1][i]];
            key = if on == 1.0 {
                side
            } else {
                [0, 1].map(|c| own[c] + on * (side[c] - own[c]))
            };
        }
        for (filter, param) in self.filters.iter_mut().zip([p.high_pass, p.low_pass]) {
            if !self.values.steady(param) {
                filter.tune(self.values.frames(param)[i], self.sample_rate);
            }
            key = filter.tick(key, self.fade);
        }
        key
    }

    /// Plays `key`, the key on the span's frame `i` on a channel, in place
    /// of `out`, the output there, as far as Listen says.
    pub fn listen(&self, i: usize, key: f32, out: &mut f32) {
        let listen = self.values.frames(self.params.listen)[i];
        if listen == 1.0 {
            *out = key;
        } else if listen != 0.0 {
            *out += listen * (key - *out);
        }
    }
}

/// One of the key's filters.
#[derive(Debug, Clone, Copy)]
struct Filter {
    kind: SectionKind,
    /// The end of its range, where it cuts nothing.
    end: f32,
    /// Whether it's off the end of its range, so in.
    on: bool,
    /// How far in it is, from 0.0, left out, to 1.0, fading to where `on`
    /// says. `None` until it's first tuned, when it's in or out at once.
    mix: Option<f32>,
    /// The frequency the coefficients are for, if they're for any yet.
    tuned: Option<f32>,
    coefficients: Coefficients,
    /// Each channel's memory.
    state: [State; 2],
}

/// A filter's memory of silence, on both channels.
const SILENT: [State; 2] = [State::SILENT; 2];

impl Filter {
    fn new(kind: SectionKind, end: f64) -> Self {
        Self {
            kind,
            end: end as f32,
            on: false,
            mix: None,
            tuned: None,
            coefficients: Coefficients::new(&Self::section(kind), 1000.0, 48_000.0),
            state: SILENT,
        }
    }

    fn section(kind: SectionKind) -> Section {
        Section {
            kind,
            q: FRAC_1_SQRT_2,
            a: 1.0,
        }
    }

    /// Tunes it to `freq` Hz, unless it's tuned there already, and has it
    /// head out if that's the end of its range, or in if not.
    fn tune(&mut self, freq: f32, sample_rate: f64) {
        self.on = match self.kind {
            SectionKind::HighPass => freq > self.end,
            _ => freq < self.end,
        };
        if self.mix.is_none() {
            self.mix = Some(if self.on { 1.0 } else { 0.0 });
        }
        if self.tuned != Some(freq) {
            let corner = f64::from(freq).min(HIGHEST * sample_rate);
            self.coefficients = Coefficients::new(&Self::section(self.kind), corner, sample_rate);
            self.tuned = Some(freq);
        }
    }

    /// Filters a frame of the key, as far as it's in, and fades it `fade`
    /// further in or out. Once it's all the way out, it comes back in from
    /// silence.
    fn tick(&mut self, key: [f32; 2], fade: f32) -> [f32; 2] {
        let mix = self.mix.unwrap_or(0.0);
        let mix = if self.on {
            (mix + fade).min(1.0)
        } else {
            (mix - fade).max(0.0)
        };
        self.mix = Some(mix);
        if mix == 0.0 {
            self.state = SILENT;
            return key;
        }
        let c = &self.coefficients;
        let state = &mut self.state;
        [0, 1].map(|n| {
            let wet = state[n].tick(c, f64::from(key[n])) as f32;
            if mix == 1.0 {
                wet
            } else {
                key[n] + mix * (wet - key[n])
            }
        })
    }
}
