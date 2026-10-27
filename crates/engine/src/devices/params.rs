//! Built-in devices' parameters, worked out frame by frame.

use std::ops::Range;

use musicable_model::native::{NativeDevice, ParamInfo};

use crate::automation::{Automation, Param};
use crate::event::{NodeEvent, NodeEventKind, ParamId};
use crate::{EngineConfig, ProcessContext};

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
            .map(|(info, &value)| {
                Param::new(value as f32, info.min as f32, info.max as f32).with_scale(info.scale)
            })
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
            set: vec![0.0; self.params.len()],
            automated: vec![false; self.params.len()],
            lanes: vec![0.0; self.params.len() * config.max_block],
            modulations: vec![0.0; self.params.len() * config.max_block],
            stride: config.max_block,
            len: 0,
        };
    }

    /// From [`Node::modulated`](crate::Node::modulated): something
    /// modulates parameter `param` in the node's graph, so it keeps its
    /// modulation when the node takes over from another.
    pub fn modulated(&mut self, param: ParamId) {
        let Ok(index) = usize::try_from(param.0) else {
            return;
        };
        if let (Some(p), Some(info)) = (self.params.get_mut(index), self.info.get(index))
            && info.modulated
        {
            p.set_modulated();
        }
    }

    /// From [`Node::adopt`](crate::Node::adopt), with the parameters of the
    /// node this one replaces, a device of the same kind.
    pub fn take_over(&mut self, previous: &Params) {
        for (param, previous) in self.params.iter_mut().zip(&previous.params) {
            param.take_over(previous);
        }
    }

    /// From [`Node::flush`](crate::Node::flush): takes the parameter
    /// changes and the modulation in `events`, as a block renders them.
    pub fn flush(&mut self, events: &[NodeEvent]) {
        for event in events {
            self.apply(event);
        }
    }

    /// From [`Node::reset`](crate::Node::reset): every parameter starts
    /// afresh at its value.
    pub fn reset(&mut self) {
        for param in &mut self.params {
            param.reset();
        }
    }

    /// Parameter `param`'s value on the last frame rendered.
    pub fn value(&self, param: usize) -> f32 {
        self.params[param].value()
    }

    /// From [`Node::automate`](crate::Node::automate): has parameter
    /// `param` follow `automation` frame by frame. Hands it back for a
    /// parameter the device hasn't got.
    pub fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        match usize::try_from(param.0)
            .ok()
            .and_then(|i| self.params.get_mut(i))
        {
            Some(p) => {
                p.automate(automation);
                None
            }
            None => Some(automation),
        }
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
            let at = i * values.stride;
            let slot = &mut values.data[at..][..frames.len()];
            let modulations = &mut values.modulations[at..][..frames.len()];
            values.automated[i] = param.is_automated();
            values.steady[i] = if values.automated[i] {
                let lane = &mut values.lanes[at..][..frames.len()];
                param.render_with_targets(ctx, frames.clone(), slot, lane, Some(modulations))
            } else {
                param.render_with_modulation(ctx, frames.clone(), slot, modulations)
            };
            values.set[i] = param.target();
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
                if let Some(p) = usize::try_from(param.0)
                    .ok()
                    .and_then(|i| self.params.get_mut(i))
                {
                    p.set(value as f32);
                }
            }
            NodeEventKind::Modulation {
                param,
                amount,
                note: None,
            } => {
                let Ok(index) = usize::try_from(param.0) else {
                    return;
                };
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
    /// Each parameter's value set, which holds over a span: only an event
    /// changes it.
    set: Vec<f32>,
    /// Each parameter's modulation on each frame, laid out as `data` is.
    modulations: Vec<f32>,
    /// Whether each parameter follows automation, and for those that do,
    /// the automation's value on each frame, laid out as `data` is.
    automated: Vec<bool>,
    lanes: Vec<f32>,
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

    /// Where it's heading on the span's frame `frame`, without its ramp or
    /// its modulation: the value set, or with automation the automation's
    /// value on the frame, without the glide after a jump. For what a
    /// device can't take halfway, such as a choice, or would rather move
    /// its own way, such as a delay's time.
    pub fn target(&self, param: usize, frame: usize) -> f32 {
        if self.automated[param] {
            self.lanes[param * self.stride + frame]
        } else {
            self.set[param]
        }
    }

    /// Its modulation for every voice on each of the span's frames, along
    /// its scale ([`Param`](crate::automation::Param)): 0.0 for none. Its
    /// frames' values have it already.
    pub fn modulations(&self, param: usize) -> &[f32] {
        &self.modulations[param * self.stride..][..self.len]
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
                param: ParamId(param as u64),
                value,
            },
        }
    }

    fn modulation(offset: usize, param: usize, amount: f64) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Modulation {
                param: ParamId(param as u64),
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
    fn modulation_ramps_in_within_the_range() {
        let mut params = prepared(NativeKind::Utility);
        let pan = utility::PAN;
        let events = [modulation(4, pan, 0.25), modulation(8, pan, 2.0)];
        let first = spans(&mut params, &events, pan);
        assert_eq!(first[0].2, [0.0; 4]);
        // A modulation ramps in over 32 frames, a modulator's tick.
        let step = 0.25 / 32.0;
        assert_eq!(first[1].2, [step, 2.0 * step, 3.0 * step, 4.0 * step]);
        // The next ramps on from there, and the sum stays within the range.
        let ramp = &first[2].2;
        assert!(ramp[14] < 1.0, "{}", ramp[14]);
        assert!(ramp[15..].iter().all(|&v| v == 1.0));
        // A set value ramps under the modulation.
        let events = [modulation(0, pan, -0.5), param_event(0, pan, 0.5)];
        let next = spans(&mut params, &events, pan);
        let values = &next.last().unwrap().2;
        assert!(values[..31].iter().all(|&v| v > 0.0));
        assert_eq!(values[31], 0.0);
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
            let modulations = values.modulations(pan);
            assert_eq!(modulations.len(), span.frames.len());
            seen.push((
                values.target(pan, 0),
                modulations[0],
                modulations[modulations.len() - 1],
                values.steady(pan),
            ));
        });
        // Ramping to -0.5 all the while, and modulated from frame 4, the
        // modulation ramping to 0.25 over 32 frames.
        let step = 0.25 / 32.0;
        assert_eq!(
            seen,
            [(-0.5, 0.0, 0.0, false), (-0.5, step, 28.0 * step, false)]
        );
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
        let mut values = [0.0; 32];
        // Before the parameter renders a frame, modulation applies at once.
        param.modulate(-1.0);
        param.render(&context(32, &[]), 0..32, &mut values);
        assert_eq!(values, [4.0; 32]);
        param.modulate(1.0);
        param.render(&context(32, &[]), 0..32, &mut values);
        assert!(values[..15].iter().all(|&v| v < 5.0));
        assert_eq!(values[15..], [5.0; 17]);
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
        assert_eq!(old.value(pan), -0.25);
        // While something modulates the parameter in the new node's graph,
        // it ramps on from where the old one was, -0.5, to its own value,
        // 0.0, keeping the old one's modulation until the next comes.
        let mut new = prepared(NativeKind::Utility);
        new.modulated(ParamId(pan as u64));
        new.take_over(&old);
        let values = spans(&mut new, &[], pan).remove(0).2;
        assert!((values[0] - (-0.45 + 0.25)).abs() < 1e-6, "{}", values[0]);
        assert_eq!(values[31], 0.25);
        // Otherwise the modulation ramps away to nothing.
        let mut new = prepared(NativeKind::Utility);
        new.take_over(&old);
        let values = spans(&mut new, &[], pan).remove(0).2;
        let first = -0.45 + 0.25 - 0.25 / 32.0;
        assert!((values[0] - first).abs() < 1e-6, "{}", values[0]);
        assert_eq!(values[31], 0.0);
    }
}
