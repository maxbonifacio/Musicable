//! Automation: parameters that follow lanes of points as the song plays.
//!
//! An [`Automation`] is a lane compiled for the engine: [`compile`](crate::compile())
//! makes one for each lane that isn't overridden, and
//! [`Graph::automate`](crate::Graph::automate) gives it to the node with the
//! parameter. Its points are in beats. Whoever reads it places them through
//! the block's tempo map ([`ProcessContext::tempo`]), each on the frame
//! [`TempoMap::frame_at`] gives, as clip players place clips, so automation
//! follows tempo changes without a new graph, and loops and locates as clips
//! do. Between two points the value moves in a straight line in the lane's
//! [`Scale`]. Two points on the same frame make a step, and on a step's
//! frame the value is the one after it.
//!
//! A node that reads its own automation works the parameter out frame by
//! frame with a [`Param`]. Other nodes, such as plugins, hand theirs back
//! ([`Node::automate`](crate::Node::automate)), and the graph sends them its
//! values as parameter events instead.

use std::ops::Range;
use std::sync::Arc;

use musicable_model::automation::Point;
use musicable_model::param::Scale;
use musicable_model::time::TempoMap;

use crate::nodes::Smoothed;
use crate::{Block, ProcessContext};

/// A lane of automation, compiled for the engine. Clones share the points,
/// so cloning is cheap.
#[derive(Debug, Clone, PartialEq)]
pub struct Automation {
    /// At least one, in order of time, as a lane's are.
    points: Arc<[Point]>,
    scale: Scale,
}

impl Automation {
    /// Follows `points`, which are in order of time as a
    /// [`Lane`](musicable_model::automation::Lane)'s are, moving evenly in
    /// `scale` between them. `None` without points.
    pub fn new(points: &[Point], scale: Scale) -> Option<Self> {
        (!points.is_empty()).then(|| Self {
            points: points.into(),
            scale,
        })
    }

    pub fn points(&self) -> &[Point] {
        &self.points
    }

    pub fn scale(&self) -> Scale {
        self.scale
    }

    /// The value on the song position `frame`, at `sample_rate`, with
    /// `tempo` placing the points.
    pub fn value_at(&self, tempo: &TempoMap, sample_rate: u32, frame: i64) -> f64 {
        let mut cursor = Cursor::default();
        cursor.seek(self, tempo, sample_rate, frame);
        cursor.value(self, frame)
    }
}

/// Where a reader is along an automation: between which points, and on
/// which frames those fall with the tempo of the block being read.
#[derive(Debug, Clone, Copy)]
struct Cursor {
    /// How many points fall on or before the frame read last.
    after: usize,
    /// The frame the point before falls on, `i64::MIN` before the first.
    start: i64,
    /// The frame the next point falls on, `i64::MAX` after the last.
    end: i64,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            after: 0,
            start: i64::MIN,
            end: i64::MAX,
        }
    }
}

impl Cursor {
    /// Places the cursor on `frame`, with `tempo` placing the points.
    fn seek(&mut self, automation: &Automation, tempo: &TempoMap, sample_rate: u32, frame: i64) {
        let points = &automation.points;
        let at = |i: usize| tempo.frame_at(points[i].at, sample_rate);
        self.after = points.partition_point(|p| tempo.frame_at(p.at, sample_rate) <= frame);
        self.start = match self.after {
            0 => i64::MIN,
            after => at(after - 1),
        };
        self.end = if self.after < points.len() {
            at(self.after)
        } else {
            i64::MAX
        };
    }

    /// Moves on to `frame`, at or after the frame read last with the same
    /// tempo, and returns how many points it passed: two or more on one
    /// frame make a step.
    fn advance(
        &mut self,
        automation: &Automation,
        tempo: &TempoMap,
        sample_rate: u32,
        frame: i64,
    ) -> usize {
        let mut passed = 0;
        while frame >= self.end {
            self.after += 1;
            passed += 1;
            self.start = self.end;
            self.end = automation
                .points
                .get(self.after)
                .map_or(i64::MAX, |p| tempo.frame_at(p.at, sample_rate));
        }
        passed
    }

    /// The value on `frame`, which is where the cursor is.
    fn value(&self, automation: &Automation, frame: i64) -> f64 {
        let points = &automation.points;
        let Some(before) = self.after.checked_sub(1) else {
            return points.first().map_or(0.0, |p| p.value);
        };
        let from = points[before].value;
        match points.get(self.after) {
            // On a point, exactly its value, whatever the scale's rounding.
            Some(next) if frame > self.start => {
                let t = (frame - self.start) as f64 / (self.end - self.start) as f64;
                automation.scale.mix(from, next.value, t)
            }
            _ => from,
        }
    }
}

/// A node's parameter, worked out frame by frame: its base, plus
/// modulation.
///
/// The base is the automation at the song position while the parameter has
/// some, and otherwise the value set, ramping to each new one over 10 ms as
/// [`Smoothed`] does. Wherever the automation jumps (at a step in its lane,
/// where the playhead jumps, and where a new graph takes over), the base
/// glides there over 10 ms, so it doesn't click. Modulation moves the base
/// along the parameter's [`Scale`], after the ramp and the glide: it's an
/// offset on the scale's even axis ([`Scale::position`]), so in the
/// parameter's units on a linear one, and in ratios on a log one. A new
/// modulation ramps there over [`MODULATION_RAMP`] frames, the time between
/// a modulator's ticks, so the modulation moves in straight lines from one
/// tick to the next. The sum stays within the parameter's range, which
/// stretches to take in a base set outside it, so modulation never moves a
/// value set out there, only keeps the sum in.
///
/// Per-note modulation is the node's own business: a `Param` is the part
/// every voice shares.
#[derive(Debug, Clone)]
pub struct Param {
    /// The value set, ramping to each new one.
    set: Smoothed,
    min: f32,
    max: f32,
    scale: Scale,
    automation: Option<Reader>,
    /// The modulation for every voice, ramping to each new one.
    modulation: Smoothed,
    /// Whether something modulates the parameter in the node's graph: see
    /// [`Node::modulated`](crate::Node::modulated).
    modulated: bool,
    /// How far the base is off the automation after a jump, falling to
    /// nothing over a ramp's length.
    glide: Smoothed,
    /// The base to glide from on the next block: where the parameter this
    /// one took over from left it.
    glide_from: Option<f32>,
    /// The base and the value on the frame rendered last.
    base: f32,
    value: f32,
    /// The playhead's jumps as of the block rendered last: `None` until the
    /// parameter renders a frame.
    jumps: Option<u64>,
}

#[derive(Debug, Clone)]
struct Reader {
    automation: Automation,
    cursor: Cursor,
}

impl Param {
    /// Set to `value`, with modulation kept within `min..=max`, on a linear
    /// scale.
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self {
            set: Smoothed::new(value),
            min: min.min(max),
            max: max.max(min),
            scale: Scale::Linear,
            automation: None,
            modulation: Smoothed::new(0.0),
            modulated: false,
            glide: Smoothed::new(0.0),
            glide_from: None,
            base: value,
            value,
            jumps: None,
        }
    }

    /// Modulated along `scale`.
    pub fn with_scale(mut self, scale: Scale) -> Self {
        self.scale = scale;
        self
    }

    /// Sets how long ramps and glides take at `sample_rate`. Call it from
    /// the node's [`Node::prepare`](crate::Node::prepare).
    pub fn prepare(&mut self, sample_rate: u32) {
        self.set.prepare(sample_rate);
        self.glide.prepare(sample_rate);
        self.modulation.prepare_frames(MODULATION_RAMP);
    }

    /// Notes that something modulates the parameter in the node's graph,
    /// so it keeps the modulation of the parameter it takes over from. Call
    /// it from the node's [`Node::modulated`](crate::Node::modulated).
    pub fn set_modulated(&mut self) {
        self.modulated = true;
    }

    /// Follows `automation` in place of the value set. Call it from the
    /// node's [`Node::automate`](crate::Node::automate).
    pub fn automate(&mut self, automation: Automation) {
        self.automation = Some(Reader {
            automation,
            cursor: Cursor::default(),
        });
    }

    pub fn is_automated(&self) -> bool {
        self.automation.is_some()
    }

    /// Carries on from `previous`, the same parameter in the node this one
    /// replaces: from its value and its modulation, ramping or gliding on
    /// to this one's. If nothing modulates it in this node's graph
    /// ([`set_modulated`](Self::set_modulated)), its modulation ramps away
    /// to nothing; otherwise it holds until the next comes. Call it from the
    /// node's [`Node::adopt`](crate::Node::adopt).
    pub fn take_over(&mut self, previous: &Param) {
        if previous.automation.is_some() {
            let mut left = previous.set;
            left.hold(previous.base);
            self.set.take_over(&left);
        } else {
            self.set.take_over(&previous.set);
        }
        self.glide.take_over(&previous.glide);
        if self.automation.is_some() && previous.jumps.is_some() {
            self.glide_from = Some(previous.base);
        }
        self.base = previous.base;
        self.value = previous.value;
        self.jumps = previous.jumps;
        if self.modulated {
            self.modulation = previous.modulation;
        } else {
            self.modulation.take_over(&previous.modulation);
        }
    }

    /// Starts afresh, as if it had rendered nothing yet: at the value set,
    /// or at the automation's value once it renders, with no ramp or glide.
    /// For a node's [`Node::reset`](crate::Node::reset).
    pub fn reset(&mut self) {
        self.set.reset();
        self.glide.hold(0.0);
        self.glide.reset();
        self.glide_from = None;
        self.modulation.reset();
        self.base = self.set.value();
        self.value = self.modulated(self.base, self.modulation.value());
        self.jumps = None;
    }

    /// Sets the value, for a [`Param`](crate::NodeEventKind::Param) event.
    /// It ramps there over 10 ms, and applies once the parameter has no
    /// automation.
    pub fn set(&mut self, value: f32) {
        self.set.set(value);
    }

    /// Offsets the value by `amount` along its scale from here on, in
    /// place of the last amount, ramping there, for a
    /// [`Modulation`](crate::NodeEventKind::Modulation) event without a
    /// note. Amounts that aren't finite are ignored.
    pub fn modulate(&mut self, amount: f32) {
        if amount.is_finite() {
            self.modulation.set(amount);
        }
    }

    /// The value on the frame rendered last.
    pub fn value(&self) -> f32 {
        self.value
    }

    /// The value set, without its ramp or its modulation: where it's
    /// heading while it has no automation.
    pub fn target(&self) -> f32 {
        self.set.target()
    }

    /// The modulation for every voice on the frame rendered last, along
    /// the scale: 0.0 for none.
    pub fn modulation(&self) -> f32 {
        self.modulation.value()
    }

    /// Writes the value for each frame in `frames` of the block into
    /// `values`, which is as long, and returns `true` only if they're all
    /// the same, for a fast path. Call it for every frame of every block,
    /// in order, a range at a time: the ranges between a node's events, say.
    pub fn render(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
    ) -> bool {
        self.render_into(ctx, frames, values, None, None)
    }

    /// Renders as [`render`](Self::render) does, and writes the
    /// modulation on each frame into `modulations`, which is as long: for a
    /// node that applies the modulation its own way, as the delay does to
    /// its time.
    pub fn render_with_modulation(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
        modulations: &mut [f32],
    ) -> bool {
        self.render_into(ctx, frames, values, None, Some(modulations))
    }

    fn render_into(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
        targets: Option<&mut [f32]>,
        modulations: Option<&mut [f32]>,
    ) -> bool {
        debug_assert_eq!(frames.len(), values.len(), "a value for each frame");
        if values.is_empty() {
            return true;
        }
        if self.automation.is_some() {
            self.render_automation(ctx, frames, values, targets, modulations)
        } else {
            if frames.start == 0 {
                self.jumps = Some(ctx.transport.jumps);
            }
            let steady = self.render_set(values, modulations);
            if let Some(targets) = targets {
                targets.fill(self.set.target());
            }
            steady
        }
    }

    /// Renders as [`render`](Self::render) does, and writes where the
    /// value is heading on each frame into `targets`, which is as long: the
    /// value set, without its ramp, or the automation's value on the frame,
    /// without the glide after a jump. Neither has the modulation in it.
    /// For a node that takes a new value its own way, as the delay fades to
    /// a new time.
    pub fn render_with_targets(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
        targets: &mut [f32],
        modulations: Option<&mut [f32]>,
    ) -> bool {
        debug_assert_eq!(values.len(), targets.len(), "a target for each frame");
        self.render_into(ctx, frames, values, Some(targets), modulations)
    }

    fn render_set(&mut self, values: &mut [f32], mut modulations: Option<&mut [f32]>) -> bool {
        let len = values.len();
        self.glide.skip(len);
        if self.set.is_settled() && self.modulation.is_settled() {
            self.set.skip(len);
            self.modulation.skip(len);
            let (base, modulation) = (self.set.value(), self.modulation.value());
            let value = self.modulated(base, modulation);
            values.fill(value);
            if let Some(modulations) = modulations {
                modulations.fill(modulation);
            }
            self.base = base;
            self.value = value;
            return true;
        }
        for (k, value) in values.iter_mut().enumerate() {
            let base = self.set.next_value();
            let modulation = self.modulation.next_value();
            if let Some(modulations) = modulations.as_deref_mut() {
                modulations[k] = modulation;
            }
            *value = self.modulated(base, modulation);
            self.base = base;
        }
        self.value = values[len - 1];
        false
    }

    fn render_automation(
        &mut self,
        ctx: &ProcessContext<'_>,
        frames: Range<usize>,
        values: &mut [f32],
        mut targets: Option<&mut [f32]>,
        mut modulations: Option<&mut [f32]>,
    ) -> bool {
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let transport = ctx.transport;
        let position = transport.position as i64;
        let mut started = self.jumps.is_some();
        self.set.skip(values.len());
        let Reader { automation, cursor } = self.automation.as_mut().expect("automated");
        if frames.start == 0 {
            let jumped = self.jumps.is_some_and(|jumps| jumps != transport.jumps);
            self.jumps = Some(transport.jumps);
            // The tempo can change between blocks: place the points again.
            let before = cursor.after;
            cursor.seek(automation, tempo, rate, position);
            let target = cursor.value(automation, position) as f32;
            // The playhead jumped, or played on to a step on this frame.
            let stepped = started && (jumped || cursor.after >= before + 2);
            let from = self.glide_from.take().or(stepped.then_some(self.base));
            if let Some(from) = from {
                glide(&mut self.glide, from - target);
            }
        }
        let (min, max, scale) = (self.min, self.max, self.scale);
        for (k, (value, i)) in values.iter_mut().zip(frames).enumerate() {
            let frame = if transport.playing {
                position + i as i64
            } else {
                position
            };
            let passed = cursor.advance(automation, tempo, rate, frame);
            let target = cursor.value(automation, frame) as f32;
            if let Some(targets) = targets.as_deref_mut() {
                targets[k] = target;
            }
            if passed >= 2 && started {
                glide(&mut self.glide, self.base - target);
            }
            let base = target + self.glide.next_value();
            let modulation = self.modulation.next_value();
            if let Some(modulations) = modulations.as_deref_mut() {
                modulations[k] = modulation;
            }
            *value = modulated(base, min, max, scale, modulation);
            self.base = base;
            started = true;
        }
        self.value = values[values.len() - 1];
        values.iter().all(|&v| v == values[0])
    }

    fn modulated(&self, base: f32, modulation: f32) -> f32 {
        modulated(base, self.min, self.max, self.scale, modulation)
    }
}

/// How many frames a new modulation takes to ramp in: 32, the time between
/// a modulator's ticks and between the values a lane sends as events.
pub const MODULATION_RAMP: u32 = Feed::GRID as u32;

/// `base` offset by `modulation` along `scale`'s even axis, kept within
/// `min..=max` stretched to take in `base`.
pub fn modulated(base: f32, min: f32, max: f32, scale: Scale, modulation: f32) -> f32 {
    if modulation == 0.0 {
        return base;
    }
    let moved = match scale {
        Scale::Linear => base + modulation,
        scale => scale.value_at(scale.position(f64::from(base)) + f64::from(modulation)) as f32,
    };
    moved.clamp(min.min(base), max.max(base))
}

/// Starts `glide` falling from `offset` to nothing.
fn glide(glide: &mut Smoothed, offset: f32) {
    glide.hold(offset);
    glide.set(0.0);
}

/// Automation the graph sends a node as parameter events, for a node that
/// reads none of its own ([`Node::automate`](crate::Node::automate)).
#[derive(Debug, Clone)]
pub(crate) struct Feed {
    automation: Automation,
    /// The value sent last.
    sent: Option<f64>,
    /// The playhead's jumps as of the block before: `None` before the
    /// first.
    jumps: Option<u64>,
}

impl Feed {
    /// How often a value goes out between points: every 32 frames of
    /// engine time, 0.7 ms at 44.1 kHz.
    pub(crate) const GRID: usize = 32;

    /// The most points a block sends values on, besides the grid. Points
    /// beyond them in one block wait for the grid.
    const POINTS: usize = 16;

    pub(crate) fn new(automation: Automation) -> Self {
        Self {
            automation,
            sent: None,
            jumps: None,
        }
    }

    /// The most events [`block`](Self::block) sends for a block of up to
    /// `max_block` frames.
    pub(crate) fn room(max_block: usize) -> usize {
        max_block.div_ceil(Self::GRID) + 1 + Self::POINTS
    }

    /// Calls `send` with the offset and the value of each event for
    /// `block`, in order of offset. A playing transport gets the value on
    /// the frame of each point, on every frame of the grid, and on the
    /// block's first frame if it's the graph's first or the playhead
    /// jumped; one that stands still, on the first frame. Each goes out
    /// only if it differs from the value sent last, so the events fall on
    /// the same frames however the blocks do.
    pub(crate) fn block(
        &mut self,
        block: &Block<'_>,
        sample_rate: u32,
        mut send: impl FnMut(usize, f64),
    ) {
        let Self {
            automation,
            sent,
            jumps,
        } = self;
        let jumped = *jumps != Some(block.transport.jumps);
        *jumps = Some(block.transport.jumps);
        let position = block.transport.position as i64;
        let mut cursor = Cursor::default();
        let mut emit = |offset: usize, frame: i64, cursor: &Cursor| {
            let value = cursor.value(automation, frame);
            if *sent != Some(value) {
                send(offset, value);
                *sent = Some(value);
            }
        };
        if !block.transport.playing {
            cursor.seek(automation, block.tempo, sample_rate, position);
            emit(0, position, &cursor);
            return;
        }
        // From just before the block, so a point on its first frame counts.
        cursor.seek(automation, block.tempo, sample_rate, position - 1);
        let grid = |offset: usize| {
            let late = (block.time + offset as u64) % Self::GRID as u64;
            offset + (Self::GRID - late as usize) % Self::GRID
        };
        let mut points = 0;
        let mut next = |offset: usize, cursor: &Cursor| {
            let grid = grid(offset);
            let point = cursor.end.saturating_sub(position);
            if points < Self::POINTS && point < grid as i64 {
                points += 1;
                point as usize
            } else {
                grid
            }
        };
        let mut offset = if jumped { 0 } else { next(0, &cursor) };
        while offset < block.frames {
            let frame = position + offset as i64;
            cursor.advance(automation, block.tempo, sample_rate, frame);
            emit(offset, frame, &cursor);
            offset = next(offset + 1, &cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::{Beats, TimeSignature};

    use super::*;
    use crate::{AudioRef, NodeEvent, TransportState};

    /// 500 frames to the beat at 120 BPM, and 10 to a ramp.
    const RATE: u32 = 1000;

    fn automation(points: &[(f64, f64)], scale: Scale) -> Automation {
        let points: Vec<Point> = points
            .iter()
            .map(|&(at, value)| Point::new(Beats::from_f64(at), value))
            .collect();
        Automation::new(&points, scale).unwrap()
    }

    fn context<'a>(
        frames: usize,
        transport: TransportState,
        tempo: &'a TempoMap,
        events: &'a [NodeEvent],
    ) -> ProcessContext<'a> {
        ProcessContext {
            frames,
            sample_rate: RATE,
            time: 0,
            transport,
            tempo,
            events,
            input: AudioRef::empty(frames),
            midi: &[],
        }
    }

    fn playing(position: u64, jumps: u64) -> TransportState {
        TransportState {
            playing: true,
            position,
            jumps,
            ..TransportState::default()
        }
    }

    /// Renders `param` for `frames` frames from `position`, playing, in
    /// blocks of `block`.
    fn render(param: &mut Param, position: u64, frames: usize, block: usize) -> Vec<f32> {
        let tempo = TempoMap::default();
        let mut out = vec![0.0; frames];
        for (n, chunk) in out.chunks_mut(block).enumerate() {
            let start = position + (n * block) as u64;
            let ctx = context(chunk.len(), playing(start, 0), &tempo, &[]);
            param.render(&ctx, 0..chunk.len(), chunk);
        }
        out
    }

    #[track_caller]
    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        let near = actual
            .iter()
            .zip(expected)
            .all(|(a, e)| (a - e).abs() < 1e-6);
        assert!(near, "{actual:?} != {expected:?}");
    }

    #[test]
    fn automation_moves_in_a_straight_line_on_its_scale_between_points() {
        let tempo = TempoMap::default();
        let ramp = automation(&[(1.0, 0.0), (2.0, 1.0)], Scale::Linear);
        let at = |frame| ramp.value_at(&tempo, RATE, frame);
        assert_eq!(
            [at(0), at(500), at(750), at(1000), at(4500)],
            [0.0, 0.0, 0.5, 1.0, 1.0]
        );
        let fader = automation(&[(0.0, 0.0), (1.0, 1.0)], Scale::Fader);
        assert!((fader.value_at(&tempo, RATE, 250) - 0.0625).abs() < 1e-12);
        // Two points on a frame make a step, and the step's frame has the
        // value after it.
        let step = automation(&[(0.0, 0.5), (1.0, 0.5), (1.0, 0.25)], Scale::Fader);
        let at = |frame| step.value_at(&tempo, RATE, frame);
        assert_eq!([at(499), at(500), at(501)], [0.5, 0.25, 0.25]);
    }

    #[test]
    fn a_param_follows_its_automation_from_frame_to_frame() {
        let mut param = Param::new(0.9, 0.0, 1.0);
        param.prepare(RATE);
        param.automate(automation(&[(1.0, 0.0), (1.2, 1.0)], Scale::Linear));
        let out = render(&mut param, 498, 6, 6);
        assert_eq!(out, [0.0, 0.0, 0.0, 0.01, 0.02, 0.03]);
        assert_eq!(param.value(), 0.03);
        // The value set waits for the automation to go.
        param.set(0.5);
        assert_eq!(render(&mut param, 504, 2, 2), [0.04, 0.05]);
    }

    #[test]
    fn steps_and_jumps_glide_over_ten_milliseconds() {
        let step = automation(&[(0.0, 1.0), (1.0, 1.0), (1.0, 0.0)], Scale::Linear);
        let glide = [1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0, 0.0];
        // However the blocks fall, even with one starting on the step.
        for block in [12, 1, 2, 5] {
            let mut param = Param::new(0.5, 0.0, 1.0);
            param.prepare(RATE);
            param.automate(step.clone());
            assert_close(&render(&mut param, 499, 12, block), &glide);
        }
        // A locate glides from where the value was.
        let mut param = Param::new(0.5, 0.0, 1.0);
        param.prepare(RATE);
        param.automate(step);
        render(&mut param, 0, 4, 4);
        let tempo = TempoMap::default();
        let mut out = [0.0; 3];
        let ctx = context(3, playing(2500, 1), &tempo, &[]);
        param.render(&ctx, 0..3, &mut out);
        assert_close(&out, &[0.9, 0.8, 0.7]);
        // The first frame a parameter renders has nothing to glide from.
        let mut fresh = Param::new(1.0, 0.0, 1.0);
        fresh.prepare(RATE);
        fresh.automate(automation(&[(0.0, 0.25)], Scale::Linear));
        assert_eq!(render(&mut fresh, 0, 2, 2), [0.25, 0.25]);
    }

    #[test]
    fn targets_leave_out_ramps_glides_and_modulation() {
        let tempo = TempoMap::default();
        let ctx = context(4, playing(499, 0), &tempo, &[]);
        let (mut values, mut targets) = ([0.0; 4], [0.0; 4]);
        // Over a step, the value glides and the target is the automation's.
        let step = automation(&[(0.0, 1.0), (1.0, 1.0), (1.0, 0.0)], Scale::Linear);
        let mut param = Param::new(0.5, 0.0, 1.0);
        param.prepare(RATE);
        param.automate(step);
        param.modulate(-0.25);
        render(&mut param, 0, 499, 499);
        param.render_with_targets(&ctx, 0..4, &mut values, &mut targets, None);
        assert_close(&values, &[0.75, 0.65, 0.55, 0.45]);
        assert_eq!(targets, [1.0, 0.0, 0.0, 0.0]);
        // Without automation, the value ramps to the value set, the target.
        let mut set = Param::new(0.0, 0.0, 1.0);
        set.prepare(RATE);
        set.modulate(0.25);
        render(&mut set, 0, 1, 1);
        set.set(0.5);
        set.render_with_targets(&ctx, 0..4, &mut values, &mut targets, None);
        assert_close(&values, &[0.3, 0.35, 0.4, 0.45]);
        assert_eq!(targets, [0.5; 4]);
        assert_eq!((set.target(), set.modulation()), (0.5, 0.25));
    }

    #[test]
    fn a_new_param_carries_on_where_the_old_one_was() {
        let mut old = Param::new(0.0, 0.0, 1.0);
        old.prepare(RATE);
        old.automate(automation(&[(0.0, 0.0), (0.2, 1.0)], Scale::Linear));
        render(&mut old, 0, 50, 50);
        assert_eq!(old.value(), 0.49);
        // Automation that goes elsewhere: it glides there from where the
        // old one was.
        let mut new = Param::new(0.0, 0.0, 1.0);
        new.prepare(RATE);
        new.automate(automation(&[(0.0, 0.0)], Scale::Linear));
        new.take_over(&old);
        assert_close(&render(&mut new, 50, 2, 2), &[0.441, 0.392]);
        // No automation: it ramps from there to the value set.
        let mut set = Param::new(1.0, 0.0, 1.0);
        set.prepare(RATE);
        set.take_over(&new);
        let from = new.value();
        let ramp = [from + (1.0 - from) * 0.1, from + (1.0 - from) * 0.2];
        assert_close(&render(&mut set, 52, 2, 2), &ramp);
    }

    #[test]
    fn modulation_adds_to_the_base_within_the_range() {
        let mut param = Param::new(0.5, 0.0, 1.0);
        param.prepare(RATE);
        // Before the parameter renders a frame, modulation applies at once.
        param.modulate(0.25);
        assert_eq!(render(&mut param, 0, 2, 2), [0.75, 0.75]);
        // After, each new amount ramps in over 32 frames, a modulator's
        // tick, and the sum stays within the range.
        param.modulate(0.75);
        let ramp = render(&mut param, 0, 32, 32);
        assert_eq!(ramp[0], 0.75 + 0.5 / 32.0);
        assert!(ramp[14] < 1.0);
        assert!(ramp[15..].iter().all(|&v| v == 1.0));
        param.modulate(f32::NAN);
        assert_eq!(render(&mut param, 0, 1, 1), [1.0]);
        // A base set beyond the range stays where it was set.
        let mut loud = Param::new(2.0, 0.0, 1.0);
        loud.modulate(0.5);
        assert_eq!(render(&mut loud, 0, 1, 1), [2.0]);
        loud.modulate(-1.5);
        assert_eq!(render(&mut loud, 0, 1, 1), [0.5]);
        // It adds to the automation too.
        let mut automated = Param::new(0.0, 0.0, 1.0);
        automated.prepare(RATE);
        automated.automate(automation(&[(0.0, 0.5)], Scale::Linear));
        automated.modulate(-0.25);
        assert_eq!(render(&mut automated, 0, 1, 1), [0.25]);
    }

    #[test]
    fn a_stopped_transport_holds_the_value_at_the_playhead() {
        let mut param = Param::new(0.0, 0.0, 1.0);
        param.prepare(RATE);
        param.automate(automation(&[(0.0, 0.0), (1.0, 1.0)], Scale::Linear));
        let tempo = TempoMap::default();
        let stopped = TransportState {
            position: 125,
            ..TransportState::default()
        };
        let mut out = [0.0; 4];
        assert!(param.render(&context(4, stopped, &tempo, &[]), 0..4, &mut out));
        assert_eq!(out, [0.25; 4]);
        // A new tempo map moves the points' frames.
        let half = TempoMap::new(60.0, TimeSignature::FOUR_FOUR);
        param.render(&context(4, stopped, &half, &[]), 0..4, &mut out);
        assert_eq!(out, [0.125; 4]);
    }

    /// Sends `feed`'s events for blocks of `sizes`, playing from frame 0,
    /// and returns them with their engine times.
    fn feed(feed: &mut Feed, sizes: &[usize]) -> Vec<(u64, f64)> {
        let tempo = TempoMap::default();
        let mut events = Vec::new();
        let mut time = 0;
        for &frames in sizes {
            let block = Block {
                frames,
                time,
                transport: playing(time, 0),
                tempo: &tempo,
                input: AudioRef::empty(frames),
                midi: &[],
            };
            feed.block(&block, RATE, |offset, value| {
                events.push((time + offset as u64, value));
            });
            time += frames as u64;
        }
        events
    }

    #[test]
    fn feeds_send_points_and_the_grid_between_them() {
        let lane = automation(
            &[(0.0, 0.0), (0.25, 0.0), (0.5, 1.0), (0.5, 0.5), (0.75, 0.5)],
            Scale::Linear,
        );
        let events = feed(&mut Feed::new(lane.clone()), &[500]);
        let tempo = TempoMap::default();
        let at = |frame: u64| (frame, lane.value_at(&tempo, RATE, frame as i64));
        // The first frame, then the grid along the ramp, the step, and
        // nothing while the value holds.
        let expected = [at(0), at(128), at(160), at(192), at(224), at(250)];
        assert_eq!(events, expected);
        assert_eq!(at(250).1, 0.5);
        for sizes in [&[1; 500][..], &[7, 31, 62, 400], &[250, 250]] {
            let events = feed(&mut Feed::new(lane.clone()), sizes);
            assert_eq!(events, expected, "{:?}", &sizes[..sizes.len().min(4)]);
        }
        assert!(expected.len() <= Feed::room(500));
    }
}
