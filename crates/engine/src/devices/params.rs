//! Built-in devices' parameters, worked out frame by frame.

use std::ops::Range;

use musicable_model::native::{NativeDevice, ParamInfo};

use crate::event::{NodeEvent, NodeEventKind};
use crate::nodes::Smoothed;
use crate::{EngineConfig, ProcessContext};

/// One of a node's parameters as the node works it out each frame: the
/// value set, ramping to each new one over 10 ms as [`Smoothed`] does, plus
/// the modulation for every voice, which applies at once.
///
/// A frame's value is the two added and kept in the range, which stretches
/// to take in a value set outside it, so modulation never moves such a
/// value, only keeps the sum in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Param {
    base: Smoothed,
    modulation: f32,
    min: f32,
    max: f32,
}

impl Param {
    /// Set to `value`, kept within `min..=max` once modulated.
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            base: Smoothed::new(value),
            modulation: 0.0,
            min,
            max,
        }
    }

    /// From [`Node::prepare`](crate::Node::prepare).
    pub fn prepare(&mut self, sample_rate: u32) {
        self.base.prepare(sample_rate);
    }

    /// From [`Node::adopt`](crate::Node::adopt): carries on from the
    /// previous node's parameter, its value and its modulation, ramping on
    /// to this one's value.
    pub fn take_over(&mut self, previous: &Param) {
        self.base.take_over(&previous.base);
        self.modulation = previous.modulation;
    }

    /// A [`Param`](NodeEventKind::Param) event: the value set, ramping to
    /// it over 10 ms.
    pub fn set(&mut self, value: f32) {
        self.base.set(value);
    }

    /// A [`Modulation`](NodeEventKind::Modulation) event without a note:
    /// replaces the last amount.
    pub fn modulate(&mut self, amount: f32) {
        self.modulation = amount;
    }

    /// Writes the parameter's value for each frame in `frames` of the block
    /// into `values`, which is as long, and returns whether they're all
    /// the same, for a fast path. Call it for the block's frames in order.
    pub fn render(
        &mut self,
        _ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
    ) -> bool {
        debug_assert_eq!(frames.len(), values.len());
        if self.base.is_settled() {
            values.fill(self.modulated(self.base.target()));
            self.base.skip(frames.len());
            return true;
        }
        for value in values.iter_mut() {
            let base = self.base.next_value();
            *value = self.modulated(base);
        }
        false
    }

    /// The value on the last frame rendered.
    pub fn value(&self) -> f32 {
        self.modulated(self.base.value())
    }

    /// The value set, without its ramp or its modulation: where it's
    /// heading.
    pub fn target(&self) -> f32 {
        self.base.target()
    }

    /// The modulation for every voice: 0.0 for none.
    pub fn modulation(&self) -> f32 {
        self.modulation
    }

    fn modulated(&self, base: f32) -> f32 {
        if self.modulation == 0.0 {
            return base;
        }
        (base + self.modulation).clamp(self.min.min(base), self.max.max(base))
    }
}

/// A built-in device's parameters, each worked out for every frame. A
/// device's node renders through [`run`](Self::run), which splits the
/// block where events fall and applies them in between.
#[derive(Debug, Clone)]
pub struct Params {
    info: &'static [ParamInfo],
    params: Vec<Param>,
    values: Values,
}

impl Params {
    /// `device`'s parameters, at the values the project has.
    pub fn new(device: &NativeDevice) -> Self {
        let info = device.params();
        let params = info
            .iter()
            .zip(&device.values)
            .map(|(info, &value)| Param::new(value as f32, info.min as f32, info.max as f32))
            .collect();
        Self {
            info,
            params,
            values: Values::default(),
        }
    }

    /// From [`Node::prepare`](crate::Node::prepare): sets the ramps' length
    /// and makes room for a block's values.
    pub fn prepare(&mut self, config: &EngineConfig) {
        for param in &mut self.params {
            param.prepare(config.sample_rate);
        }
        self.values = Values {
            data: vec![0.0; self.params.len() * config.max_block],
            steady: vec![true; self.params.len()],
            targets: vec![0.0; self.params.len()],
            modulations: vec![0.0; self.params.len()],
            stride: config.max_block,
            len: 0,
        };
    }

    /// From [`Node::adopt`](crate::Node::adopt), with the parameters of the
    /// node this one replaces, a device of the same kind.
    pub fn take_over(&mut self, previous: &Params) {
        for (param, previous) in self.params.iter_mut().zip(&previous.params) {
            param.take_over(previous);
        }
    }

    /// Parameter `param`'s value on the last frame rendered.
    pub fn value(&self, param: usize) -> f32 {
        self.params[param].value()
    }

    /// Renders the block through `render`, a span at a time: the frames
    /// between one event and the next. Each event applies before the span
    /// it starts, which gets it too, to handle a note or a modulation for
    /// one note. Parameter events and modulation for every voice are
    /// already applied. Spans with no frames and no event are left out.
    pub fn run(&mut self, ctx: &ProcessContext<'_>, mut render: impl FnMut(Span<'_>)) {
        let mut start = 0;
        let mut at = None;
        for event in ctx.events {
            self.span(ctx, at, start..event.offset, &mut render);
            self.apply(event);
            at = Some(event);
            start = event.offset;
        }
        self.span(ctx, at, start..ctx.frames, &mut render);
    }

    fn span(
        &mut self,
        ctx: &ProcessContext<'_>,
        event: Option<&NodeEvent>,
        frames: Range<usize>,
        render: &mut impl FnMut(Span<'_>),
    ) {
        if frames.is_empty() && event.is_none() {
            return;
        }
        let values = &mut self.values;
        values.len = frames.len();
        for (i, param) in self.params.iter_mut().enumerate() {
            let slot = &mut values.data[i * values.stride..][..frames.len()];
            values.steady[i] = param.render(ctx, frames.clone(), slot);
            values.targets[i] = param.target();
            values.modulations[i] = param.modulation();
        }
        render(Span {
            event,
            frames,
            values: &self.values,
        });
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param { param, value } => {
                if let Some(p) = self.params.get_mut(param.0 as usize) {
                    p.set(value as f32);
                }
            }
            NodeEventKind::Modulation {
                param,
                amount,
                note: None,
            } => {
                let index = param.0 as usize;
                if let (Some(p), Some(info)) = (self.params.get_mut(index), self.info.get(index))
                    && info.modulated
                {
                    p.modulate(amount as f32);
                }
            }
            _ => {}
        }
    }
}

/// Part of a block that a device renders: the frames up to the next event.
#[derive(Debug, Clone)]
pub struct Span<'a> {
    /// The event on the span's first frame, if it starts with one.
    pub event: Option<&'a NodeEvent>,
    /// The span's frames of the block.
    pub frames: Range<usize>,
    /// The parameters' values over the span.
    pub values: &'a Values,
}

/// Each parameter's value on each frame of a [`Span`].
#[derive(Debug, Clone, Default)]
pub struct Values {
    /// `stride` frames for each parameter, the first `len` of them this
    /// span's.
    data: Vec<f32>,
    steady: Vec<bool>,
    /// Each parameter's value set and its modulation, which hold over a
    /// span: only an event changes them.
    targets: Vec<f32>,
    modulations: Vec<f32>,
    stride: usize,
    len: usize,
}

impl Values {
    /// Parameter `param`'s value on each of the span's frames.
    pub fn frames(&self, param: usize) -> &[f32] {
        &self.data[param * self.stride..][..self.len]
    }

    /// Its value on the span's first frame. The span has a frame.
    pub fn first(&self, param: usize) -> f32 {
        self.data[param * self.stride]
    }

    /// Whether it holds one value over the span.
    pub fn steady(&self, param: usize) -> bool {
        self.steady[param]
    }

    /// The value it's set to, without its ramp or its modulation: where it
    /// is heading. For what a device can't take halfway, such as a choice,
    /// or would rather move its own way, such as a delay's time.
    pub fn target(&self, param: usize) -> f32 {
        self.targets[param]
    }

    /// Its modulation for every voice over the span, in its units: 0.0 for
    /// none. Its frames' values have it added already.
    pub fn modulation(&self, param: usize) -> f32 {
        self.modulations[param]
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::native::{NativeKind, utility};

    use super::*;
    use crate::devices::testing::context;
    use crate::event::{NoteEvent, NoteId, ParamId};

    fn param_event(offset: usize, param: usize, value: f64) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Param {
                param: ParamId(param as u32),
                value,
            },
        }
    }

    fn modulation(offset: usize, param: usize, amount: f64) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Modulation {
                param: ParamId(param as u32),
                amount,
                note: None,
            },
        }
    }

    fn prepared(kind: NativeKind) -> Params {
        let mut params = Params::new(&NativeDevice::new(kind));
        params.prepare(&EngineConfig {
            sample_rate: 1000,
            max_block: 64,
        });
        params
    }

    /// Each span's frames, its event's offset, and parameter `param`'s
    /// values over it.
    fn spans(
        params: &mut Params,
        events: &[NodeEvent],
        param: usize,
    ) -> Vec<(Range<usize>, Option<usize>, Vec<f32>)> {
        let mut spans = Vec::new();
        params.run(&context(32, events), |span| {
            let values = span.values.frames(param).to_vec();
            assert_eq!(values.len(), span.frames.len());
            if span.values.steady(param) {
                assert!(values.iter().all(|&v| v == values[0]));
            }
            spans.push((span.frames, span.event.map(|e| e.offset), values));
        });
        spans
    }

    #[test]
    fn blocks_split_where_events_fall() {
        let mut params = prepared(NativeKind::Utility);
        let gain = utility::GAIN;
        let events = [param_event(0, gain, 0.5), param_event(10, gain, 0.25)];
        let spans = spans(&mut params, &events, gain);
        let ranges: Vec<_> = spans.iter().map(|(r, e, _)| (r.clone(), *e)).collect();
        assert_eq!(ranges, [(0..10, Some(0)), (10..32, Some(10))]);
        // Before the node has rendered a frame, a value applies at once.
        assert!(spans[0].2.iter().all(|&v| v == 0.5));
        // Then it ramps over 10 ms: ten frames at 1 kHz.
        let ramp = &spans[1].2;
        assert_eq!(ramp[0], 0.475);
        assert_eq!(ramp[9], 0.25);
        assert!(ramp[10..].iter().all(|&v| v == 0.25));
        assert_eq!(params.value(gain), 0.25);
    }

    #[test]
    fn events_on_one_frame_each_get_a_span() {
        let mut params = prepared(NativeKind::Utility);
        let note = NodeEvent {
            offset: 5,
            kind: NodeEventKind::Note(NoteEvent::On {
                note: NoteId(1),
                key: 60,
                velocity: 1.0,
            }),
        };
        let events = [param_event(5, utility::PAN, 0.5), note];
        let spans = spans(&mut params, &events, utility::PAN);
        let ranges: Vec<_> = spans.iter().map(|(r, e, _)| (r.clone(), *e)).collect();
        assert_eq!(ranges, [(0..5, None), (5..5, Some(5)), (5..32, Some(5))]);
    }

    #[test]
    fn modulation_adds_at_once_within_the_range() {
        let mut params = prepared(NativeKind::Utility);
        let pan = utility::PAN;
        let events = [modulation(4, pan, 0.25), modulation(8, pan, 2.0)];
        let first = spans(&mut params, &events, pan);
        assert_eq!(first[0].2, [0.0; 4]);
        assert_eq!(first[1].2, [0.25; 4]);
        assert!(first[2].2.iter().all(|&v| v == 1.0));
        // The next modulation replaces the last; a set value ramps under it.
        let events = [modulation(0, pan, -0.5), param_event(0, pan, 0.5)];
        let next = spans(&mut params, &events, pan);
        let values = &next.last().unwrap().2;
        assert!((values[0] - (-0.5 + 0.05)).abs() < 1e-6, "{}", values[0]);
        assert!((values[20] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn spans_know_the_value_set_and_the_modulation() {
        let mut params = prepared(NativeKind::Utility);
        let pan = utility::PAN;
        spans(&mut params, &[param_event(0, pan, 0.5)], pan);
        let events = [param_event(0, pan, -0.5), modulation(4, pan, 0.25)];
        let mut seen = Vec::new();
        params.run(&context(32, &events), |span| {
            let values = span.values;
            seen.push((
                values.target(pan),
                values.modulation(pan),
                values.steady(pan),
            ));
        });
        // Ramping to -0.5 all the while, modulated by 0.25 from frame 4.
        assert_eq!(seen, [(-0.5, 0.0, false), (-0.5, 0.25, false)]);
    }

    #[test]
    fn switches_and_choices_are_not_modulated() {
        let mut params = prepared(NativeKind::Utility);
        let events = [modulation(0, utility::MUTE, 1.0)];
        let spans = spans(&mut params, &events, utility::MUTE);
        assert!(
            spans
                .iter()
                .all(|(_, _, values)| values.iter().all(|&v| v == 0.0))
        );
    }

    #[test]
    fn values_set_outside_the_range_stay_put() {
        let mut param = Param::new(5.0, 0.0, 1.0);
        param.prepare(1000);
        let mut values = [0.0; 4];
        param.modulate(-1.0);
        param.render(&context(4, &[]), 0..4, &mut values);
        assert_eq!(values, [4.0; 4]);
        param.modulate(1.0);
        param.render(&context(4, &[]), 0..4, &mut values);
        assert_eq!(values, [5.0; 4]);
    }

    #[test]
    fn a_new_node_carries_on_from_the_one_it_replaces() {
        let mut old = prepared(NativeKind::Utility);
        let pan = utility::PAN;
        spans(&mut old, &[param_event(0, pan, 0.5)], pan);
        spans(
            &mut old,
            &[param_event(0, pan, -0.5), modulation(0, pan, 0.25)],
            pan,
        );
        let mut new = prepared(NativeKind::Utility);
        new.take_over(&old);
        let values = spans(&mut new, &[], pan).remove(0).2;
        // It ramps on from where the old one was, -0.5, to its own value,
        // 0.0, with the old one's modulation.
        assert_eq!(old.value(pan), -0.25);
        assert!((values[0] - (-0.45 + 0.25)).abs() < 1e-6, "{}", values[0]);
        assert_eq!(values[31], 0.25);
    }
}
