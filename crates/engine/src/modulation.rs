//! The modulators as the engine plays them: see [`Modulation`].
//!
//! [`compile`](crate::compile()) gives each channel that has modulators,
//! or parameters that modulators move, a [`Modulation`] node. It runs the
//! channel's modulators and their routings, and it's the one node that
//! modulates the channel's parameters, through the ports
//! [`Graph::modulate`](crate::Graph::modulate) gives it. A routing from
//! another channel, such as the master's to a track's device, goes through
//! the node of the parameter's channel: the routing's own node relays what
//! it adds to the parameter there, and the node there adds it to the rest.
//! So each node runs after the nodes of the channels whose modulators reach
//! it: the master's first, then the groups', outer ones first, then the
//! tracks'.
//!
//! A node works in ticks: on every 32nd frame of engine time, as a lane
//! sends values ([`Node::automate`]), on the frames of its events, up to 16
//! more a block, and on the first frame it plays. At each tick it takes the
//! events up to there, moves its modulators on and works out each
//! routing, and sends each parameter whose sum has changed its new one,
//! which the parameter ramps to over the next 32 frames
//! ([`automation::MODULATION_RAMP`](crate::automation::MODULATION_RAMP)).
//! What moves a modulator's own parameters applies from the tick after, so
//! modulators can move each other without a loop.
//!
//! Each sum is an offset along the parameter's scale
//! ([`Scale::position`](musicable_model::param::Scale::position)), which is
//! what [`Modulation`](crate::NodeEventKind::Modulation) events carry. A
//! routing's amount is a share of its parameter's range
//! ([`Routing::amount`]), so the node scales it by the range's length along
//! the scale; a plugin's parameter takes it in the plugin's own units.

use musicable_model::modulation::{
    self as model, ModulatorId, ModulatorKind, NoteTrigger, Routing, RoutingId, envelope, lfo,
};
use musicable_model::native::ParamInfo;
use musicable_model::time::{Beats, TempoMap};

use crate::automation::Feed;
use crate::{
    AudioMut, Controls, EngineConfig, Inputs, Node, NodeEvent, NodeEventKind, NoteEvent, NoteId,
    ParamId, ProcessContext,
};

/// The id parameter `n` of the modulator with id `modulator` has on its
/// channel's node, for parameter events and lanes: the modulator's id above
/// the parameter's number.
pub fn param_id(modulator: ModulatorId, n: u32) -> ParamId {
    ParamId(u64::from(modulator.0) << 32 | u64::from(n))
}

/// The id the amount of the routing with id `routing` has on its channel's
/// node, so a parameter event can change it without a new graph: the
/// routing's id below 32 bits of ones, which no modulator's id is.
pub fn amount_id(routing: RoutingId) -> ParamId {
    ParamId(u64::from(u32::MAX) << 32 | u64::from(routing.0))
}

/// How often a node ticks between its events, in frames of engine time.
const GRID: usize = Feed::GRID;

/// The most ticks a block has on its events' frames, besides the grid.
/// Events beyond them in one block wait for the grid.
const EXTRA_TICKS: usize = 16;

/// The most notes a node keeps track of at once. Notes beyond them still
/// start its modulators, but don't hold their gates open.
const HELD: usize = 64;

/// A channel's modulators and routings, running: see the [module](self).
///
/// Its parameters are its modulators' ([`param_id`]) and its routings'
/// amounts ([`amount_id`]). [`Modulation`](NodeEventKind::Modulation)
/// events relay other channels' routings: their parameter is the relay's
/// number, from [`add_relay`](Self::add_relay). It plays notes sent to it
/// ([`Graph::send_notes`](crate::Graph::send_notes)) to its modulators. It
/// outputs silence, on one channel, and connects to nothing.
pub struct Modulation {
    modulators: Vec<Modulator>,
    routings: Vec<Route>,
    targets: Vec<Target>,
    relays: Vec<Relay>,
    sample_rate: f64,
    /// Engine time of the last tick, if there's been one.
    ticked: Option<u64>,
    /// Whether the next tick is the first since the node was prepared, on
    /// the first frame of the block, sending every port.
    fresh: bool,
    /// The notes held, first `held_count` of them.
    held: [NoteId; HELD],
    held_count: usize,
    /// Whether a note has started since the last tick.
    note_started: bool,
    /// Whether a note has started since the last tick with no other held.
    first_started: bool,
}

/// A modulator, running.
struct Modulator {
    id: ModulatorId,
    kind: Kind,
    /// Its parameters: its kind's table, or a Macro's controls.
    params: Vec<Param>,
    /// Its outputs' values as of the last tick: one for each control of a
    /// Macro, and one for the other kinds.
    outputs: Vec<f64>,
    /// Whether something besides notes moves its trigger or gate, so Auto
    /// acts as Never.
    trigger_moved: bool,
}

struct Param {
    n: u32,
    info: &'static ParamInfo,
    value: f64,
    /// What moves it along its scale, as of the last tick.
    modulation: f64,
    /// Whether one of the node's targets moves it.
    targeted: bool,
}

impl Param {
    /// Its value, moved by its modulation along its scale and kept within
    /// its range, stretched to take in the value.
    fn value(&self) -> f64 {
        if self.modulation == 0.0 {
            return self.value;
        }
        let Self { info, value, .. } = *self;
        let moved = info
            .scale
            .value_at(info.scale.position(value) + self.modulation);
        moved.clamp(info.min.min(value), info.max.max(value))
    }

    fn is_on(&self) -> bool {
        self.value() >= 0.5
    }
}

enum Kind {
    Macro,
    Lfo(Lfo),
    Envelope(Envelope),
}

#[derive(Debug, Clone, Copy, Default)]
struct Lfo {
    /// How far through its cycle it is, from 0 to 1, before its Phase.
    phase: f64,
    /// Seconds since it last started, for the Note start's delay and
    /// fade-in.
    since: f64,
    /// Whether its Trigger was at half way or above at the last tick.
    trigger: bool,
}

#[derive(Debug, Clone, Copy)]
struct Envelope {
    stage: Stage,
    /// How far through the stage it is, from 0 to 1.
    progress: f64,
    /// Its level where the stage started.
    start: f64,
    level: f64,
    /// Whether its gate was open at the last tick.
    gate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
}

/// What notes did since the last tick.
#[derive(Debug, Clone, Copy)]
struct Notes {
    /// Whether any are held.
    held: bool,
    /// Whether one started.
    started: bool,
    /// Whether one started with none held.
    first: bool,
}

/// Where the song is at a tick.
struct Song<'a> {
    tempo: &'a TempoMap,
    sample_rate: u32,
    /// In samples.
    position: u64,
    playing: bool,
}

impl Song<'_> {
    fn beats(&self) -> f64 {
        self.tempo.beats_at(self.position as f64, self.sample_rate)
    }

    fn seconds(&self) -> f64 {
        self.position as f64 / f64::from(self.sample_rate)
    }

    fn bpm(&self) -> f64 {
        self.tempo.bpm_at(Beats::from_f64(self.beats()))
    }
}

/// A routing, running.
struct Route {
    id: RoutingId,
    /// The modulator and which of its outputs, by index.
    from: (usize, usize),
    scale: Option<(usize, usize)>,
    curve: model::Curve,
    amount: f64,
    /// The length of its parameter's range along the parameter's scale,
    /// which the amount is a share of: 1.0 for a plugin's parameter.
    span: f64,
    /// Its lag, in seconds.
    lag: f64,
    /// What it adds to its parameter, as of the last tick.
    value: f64,
    /// Whether it has worked out a value yet: until it has, it follows its
    /// output at once, lag or not.
    started: bool,
    /// Which of the node's targets it moves.
    target: usize,
}

/// A parameter the node moves.
struct Target {
    to: To,
    /// What moves it, as of the last tick.
    sum: f64,
    /// The sum sent last, for a port's.
    sent: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
enum To {
    /// What the node sends through this port.
    Port(usize),
    /// One of its own modulators' parameters, by the modulator's index and
    /// the parameter's.
    Param(usize, usize),
}

/// What another channel's node adds to one of the node's targets.
struct Relay {
    target: usize,
    value: f64,
}

impl Modulation {
    /// A node running `modulators`, the modulators of a channel in the
    /// strip's order, with no routings.
    pub fn new(modulators: &[model::Modulator]) -> Self {
        Self {
            modulators: modulators.iter().map(Modulator::new).collect(),
            routings: Vec::new(),
            targets: Vec::new(),
            relays: Vec::new(),
            sample_rate: 48_000.0,
            ticked: None,
            fresh: true,
            held: [NoteId::default(); HELD],
            held_count: 0,
            note_started: false,
            first_started: false,
        }
    }

    /// Has the node send what moves a parameter through its port `port`
    /// ([`Graph::modulate`](crate::Graph::modulate)), and returns the
    /// target's number.
    pub fn add_port(&mut self, port: usize) -> usize {
        self.add_target(To::Port(port))
    }

    /// Has the node move parameter `n` of its modulator with id `modulator`
    /// itself, and returns the target's number: `None` if it hasn't the
    /// parameter.
    pub fn add_param(&mut self, modulator: ModulatorId, n: u32) -> Option<usize> {
        let m = self.modulators.iter().position(|m| m.id == modulator)?;
        let p = self.modulators[m].params.iter().position(|p| p.n == n)?;
        self.modulators[m].params[p].targeted = true;
        Some(self.add_target(To::Param(m, p)))
    }

    fn add_target(&mut self, to: To) -> usize {
        self.targets.push(Target {
            to,
            sum: 0.0,
            sent: None,
        });
        self.targets.len() - 1
    }

    /// Has what another node sends with the returned parameter add to
    /// `target`, for that node's routings: the relay's number.
    pub fn add_relay(&mut self, target: usize) -> ParamId {
        assert!(target < self.targets.len(), "the node has the target");
        self.relays.push(Relay { target, value: 0.0 });
        ParamId(self.relays.len() as u64 - 1)
    }

    /// Has `routing`, from one of the node's modulators, move `target`,
    /// with its amount a share of `span`, the length of its parameter's
    /// range along the parameter's scale: 1.0 for a plugin's parameter.
    /// Returns whether the node has the outputs it takes.
    pub fn add_routing(&mut self, routing: &Routing, span: f64, target: usize) -> bool {
        assert!(target < self.targets.len(), "the node has the target");
        let Some(from) = self.output(routing.from) else {
            return false;
        };
        let scale = match routing.scale {
            Some(scale) => match self.output(scale) {
                Some(scale) => Some(scale),
                None => return false,
            },
            None => None,
        };
        self.routings.push(Route {
            id: routing.id,
            from,
            scale,
            curve: routing.curve,
            amount: routing.amount,
            span,
            lag: routing.lag / 1000.0,
            value: 0.0,
            started: false,
            target,
        });
        true
    }

    /// Notes that something besides notes moves the trigger or the gate of
    /// the modulator with id `modulator`, so its Auto note trigger acts as
    /// Never.
    pub fn move_trigger(&mut self, modulator: ModulatorId) {
        if let Some(m) = self.modulators.iter_mut().find(|m| m.id == modulator) {
            m.trigger_moved = true;
        }
    }

    /// The modulator and output index of `output`, if the node has it.
    fn output(&self, output: model::Output) -> Option<(usize, usize)> {
        let m = self
            .modulators
            .iter()
            .position(|m| m.id == output.modulator)?;
        let o = match self.modulators[m].kind {
            Kind::Macro => self.modulators[m]
                .params
                .iter()
                .position(|p| p.n == output.n)?,
            Kind::Lfo(_) | Kind::Envelope(_) => (output.n == 0).then_some(0)?,
        };
        Some((m, o))
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param { param, value } => self.set(param, value),
            NodeEventKind::Modulation {
                param,
                amount,
                note: None,
            } if amount.is_finite() => {
                let relay = usize::try_from(param.0).ok();
                if let Some(relay) = relay.and_then(|r| self.relays.get_mut(r)) {
                    relay.value = amount;
                }
            }
            NodeEventKind::Note(NoteEvent::On { note, .. }) => {
                self.first_started |= self.held_count == 0;
                self.note_started = true;
                if self.held_count < HELD {
                    self.held[self.held_count] = note;
                    self.held_count += 1;
                }
            }
            NodeEventKind::Note(NoteEvent::Off { note, .. }) => {
                let held = &mut self.held[..self.held_count];
                if let Some(i) = held.iter().position(|&n| n == note) {
                    held.copy_within(i + 1.., i);
                    self.held_count -= 1;
                }
            }
            _ => {}
        }
    }

    /// Sets a modulator's parameter or a routing's amount.
    fn set(&mut self, param: ParamId, value: f64) {
        let (high, low) = ((param.0 >> 32) as u32, param.0 as u32);
        if high == u32::MAX {
            let route = self.routings.iter_mut().find(|r| r.id.0 == low);
            if let Some(route) = route
                && value.is_finite()
            {
                route.amount = value;
            }
            return;
        }
        let modulator = self.modulators.iter_mut().find(|m| m.id.0 == high);
        let param = modulator.and_then(|m| m.params.iter_mut().find(|p| p.n == low));
        if let Some(param) = param {
            param.value = param.info.clamp(value);
        }
    }

    /// Moves everything on to frame `offset` of the block, and sends what
    /// has changed.
    fn tick(&mut self, ctx: &ProcessContext<'_>, offset: usize, controls: &mut Controls<'_>) {
        let time = ctx.time + offset as u64;
        let dt = self
            .ticked
            .map_or(0.0, |t| time.saturating_sub(t) as f64 / self.sample_rate);
        self.ticked = Some(time);
        let transport = ctx.transport;
        let song = Song {
            tempo: ctx.tempo,
            sample_rate: ctx.sample_rate,
            position: transport.position + if transport.playing { offset as u64 } else { 0 },
            playing: transport.playing,
        };
        let notes = Notes {
            held: self.held_count > 0,
            started: std::mem::take(&mut self.note_started),
            first: std::mem::take(&mut self.first_started),
        };
        for modulator in &mut self.modulators {
            modulator.advance(dt, &song, notes);
        }

        let Self {
            modulators,
            routings,
            targets,
            relays,
            fresh,
            ..
        } = self;
        for target in targets.iter_mut() {
            target.sum = 0.0;
        }
        for route in routings.iter_mut() {
            let from = &modulators[route.from.0];
            let bipolar = from.bipolar(route.from.1);
            let mut moves = route.curve.apply(from.outputs[route.from.1], bipolar);
            if let Some((m, o)) = route.scale {
                moves *= modulators[m].outputs[o];
            }
            moves *= route.amount * route.span;
            route.value = if route.lag > 0.0 && route.started {
                let follow = 1.0 - (-dt / route.lag).exp();
                route.value + (moves - route.value) * follow
            } else {
                moves
            };
            route.started = true;
            targets[route.target].sum += route.value;
        }
        for relay in relays.iter() {
            targets[relay.target].sum += relay.value;
        }
        for target in targets.iter_mut() {
            match target.to {
                To::Port(port) => {
                    if (*fresh || target.sent != Some(target.sum))
                        && controls.send(port, offset, target.sum)
                    {
                        target.sent = Some(target.sum);
                    }
                }
                // For the next tick.
                To::Param(m, p) => modulators[m].params[p].modulation = target.sum,
            }
        }
        *fresh = false;
    }
}

impl Modulator {
    fn new(modulator: &model::Modulator) -> Self {
        let params: Vec<Param> = (modulator.params().into_iter())
            .map(|n| Param {
                n,
                info: modulator
                    .param(n)
                    .expect("the modulator has its parameters"),
                value: modulator
                    .value(n)
                    .expect("the modulator has its parameters"),
                modulation: 0.0,
                targeted: false,
            })
            .collect();
        let kind = match modulator.kind {
            ModulatorKind::Macro => Kind::Macro,
            ModulatorKind::Lfo => Kind::Lfo(Lfo::default()),
            ModulatorKind::Envelope => Kind::Envelope(Envelope {
                stage: Stage::Idle,
                progress: 0.0,
                start: 0.0,
                level: 0.0,
                gate: false,
            }),
        };
        Self {
            id: modulator.id,
            kind,
            outputs: vec![0.0; modulator.outputs().len()],
            params,
            trigger_moved: false,
        }
    }

    /// Carries on from `previous`, the same modulator in the node this one
    /// replaces: where its shape has got to, and the modulation of the
    /// parameters something still moves.
    fn take_over(&mut self, previous: &Modulator) {
        match (&mut self.kind, &previous.kind) {
            (Kind::Lfo(lfo), Kind::Lfo(old)) => *lfo = *old,
            (Kind::Envelope(envelope), Kind::Envelope(old)) => *envelope = *old,
            _ => {}
        }
        for param in self.params.iter_mut().filter(|p| p.targeted) {
            if let Some(old) = previous.params.iter().find(|p| p.n == param.n) {
                param.modulation = old.modulation;
            }
        }
    }

    /// Whether its output with index `output` goes from -1 to 1: an LFO's,
    /// unless it's unipolar.
    fn bipolar(&self, output: usize) -> bool {
        matches!(self.kind, Kind::Lfo(_)) && output == 0 && !self.params[lfo::UNIPOLAR].is_on()
    }

    /// Moves on `dt` seconds, to where `song` is, and works out its
    /// outputs.
    fn advance(&mut self, dt: f64, song: &Song<'_>, notes: Notes) {
        let Self {
            kind,
            params,
            outputs,
            trigger_moved,
            ..
        } = self;
        let triggers = |n: usize| match NoteTrigger::from_value(params[n].value) {
            NoteTrigger::Auto if *trigger_moved => NoteTrigger::Never,
            NoteTrigger::Auto => NoteTrigger::Always,
            trigger => trigger,
        };
        match kind {
            Kind::Macro => {
                for (output, param) in outputs.iter_mut().zip(params.iter()) {
                    *output = param.value();
                }
            }
            Kind::Lfo(state) => {
                let trigger = triggers(lfo::NOTE_TRIGGER);
                outputs[0] = state.advance(params, dt, song, notes, trigger);
            }
            Kind::Envelope(state) => {
                let trigger = triggers(envelope::NOTE_TRIGGER);
                outputs[0] = state.advance(params, dt, song, notes, trigger);
            }
        }
    }
}

impl Lfo {
    /// Moves on `dt` seconds, to where `song` is, starting again as `notes`
    /// and its Trigger say with the Note start, and returns its output.
    fn advance(
        &mut self,
        params: &[Param],
        dt: f64,
        song: &Song<'_>,
        notes: Notes,
        trigger: NoteTrigger,
    ) -> f64 {
        let value = |n: usize| params[n].value();
        let synced = params[lfo::SYNC].is_on();
        let note = lfo::NOTE_BEATS[(value(lfo::NOTE).round() as usize).min(lfo::NOTES.len() - 1)];
        let cycles = |seconds: f64| match synced {
            true => seconds * song.bpm() / 60.0 / note,
            false => seconds * value(lfo::RATE),
        };
        let triggered = params[lfo::TRIGGER].is_on();
        let rose = triggered && !self.trigger;
        self.trigger = triggered;
        let mut fade = 1.0;
        match lfo::Start::from_value(value(lfo::START)) {
            lfo::Start::Free => self.phase = (self.phase + cycles(dt)).rem_euclid(1.0),
            lfo::Start::Song => {
                let cycles = match synced {
                    true => song.beats() / note,
                    false => cycles(song.seconds()),
                };
                self.phase = cycles.rem_euclid(1.0);
            }
            lfo::Start::Note => {
                self.phase = (self.phase + cycles(dt)).rem_euclid(1.0);
                self.since += dt;
                let by_notes = match trigger {
                    NoteTrigger::Always => notes.started,
                    NoteTrigger::Legato => notes.first,
                    NoteTrigger::Auto | NoteTrigger::Never => false,
                };
                if rose || by_notes {
                    self.phase = 0.0;
                    self.since = 0.0;
                }
                let delay = value(lfo::DELAY) / 1000.0;
                let fade_in = value(lfo::FADE_IN) / 1000.0;
                fade = if self.since < delay {
                    0.0
                } else if self.since - delay < fade_in {
                    (self.since - delay) / fade_in
                } else {
                    1.0
                };
            }
        }
        let shape = lfo::Shape::from_value(value(lfo::SHAPE));
        let phase = self.phase + value(lfo::PHASE) / 360.0;
        let output = shape.value(phase, value(lfo::SKEW) / 100.0);
        let output = match params[lfo::UNIPOLAR].is_on() {
            true => 0.5 * (output + 1.0),
            false => output,
        };
        fade * output
    }
}

/// An envelope's settings at a tick.
struct Stages {
    /// In seconds.
    delay: f64,
    attack: f64,
    hold: f64,
    decay: f64,
    /// From 0 to 1.
    sustain: f64,
    release: f64,
    one_shot: bool,
    /// From -1 to 1.
    attack_curve: f64,
    decay_curve: f64,
    release_curve: f64,
}

impl Envelope {
    /// Moves on `dt` seconds, gated as `notes`, its gate and `song` say,
    /// and returns its level.
    fn advance(
        &mut self,
        params: &[Param],
        dt: f64,
        song: &Song<'_>,
        notes: Notes,
        trigger: NoteTrigger,
    ) -> f64 {
        let value = |n: usize| params[n].value();
        let stages = Stages {
            delay: value(envelope::DELAY) / 1000.0,
            attack: value(envelope::ATTACK) / 1000.0,
            hold: value(envelope::HOLD) / 1000.0,
            decay: value(envelope::DECAY) / 1000.0,
            sustain: value(envelope::SUSTAIN) / 100.0,
            release: value(envelope::RELEASE) / 1000.0,
            one_shot: params[envelope::ONE_SHOT].is_on(),
            attack_curve: value(envelope::ATTACK_CURVE) / 100.0,
            decay_curve: value(envelope::DECAY_CURVE) / 100.0,
            release_curve: value(envelope::RELEASE_CURVE) / 100.0,
        };
        self.run(dt, &stages);
        let by_notes = trigger != NoteTrigger::Never && notes.held;
        let transport = params[envelope::TRANSPORT].is_on() && song.playing;
        let gate = by_notes || params[envelope::GATE].is_on() || transport;
        let starts = match trigger {
            NoteTrigger::Always => notes.started,
            NoteTrigger::Legato => notes.first,
            NoteTrigger::Auto | NoteTrigger::Never => false,
        };
        if (gate && !self.gate) || starts {
            self.enter(Stage::Delay);
            self.run(0.0, &stages);
        }
        self.gate = gate;
        let gated = matches!(
            self.stage,
            Stage::Delay | Stage::Attack | Stage::Hold | Stage::Decay | Stage::Sustain
        );
        if !gate && !stages.one_shot && gated {
            self.enter(Stage::Release);
            self.run(0.0, &stages);
        }
        self.level
    }

    fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        self.progress = 0.0;
        self.start = self.level;
    }

    /// Moves `dt` seconds on through the stages, leaving out any of 0 s.
    fn run(&mut self, mut dt: f64, stages: &Stages) {
        loop {
            let (length, end, curve, next) = match self.stage {
                Stage::Idle => return,
                Stage::Sustain => {
                    self.level = stages.sustain;
                    return;
                }
                Stage::Delay => (stages.delay, self.start, 0.0, Stage::Attack),
                Stage::Attack => (stages.attack, 1.0, stages.attack_curve, Stage::Hold),
                Stage::Hold => {
                    let next = match stages.one_shot && stages.decay == 0.0 {
                        true => Stage::Release,
                        false => Stage::Decay,
                    };
                    (stages.hold, 1.0, 0.0, next)
                }
                Stage::Decay if stages.one_shot => {
                    (stages.decay, 0.0, stages.decay_curve, Stage::Idle)
                }
                Stage::Decay => (
                    stages.decay,
                    stages.sustain,
                    stages.decay_curve,
                    Stage::Sustain,
                ),
                Stage::Release => (stages.release, 0.0, stages.release_curve, Stage::Idle),
            };
            let left = (1.0 - self.progress) * length;
            if length > 0.0 && dt < left {
                self.progress += dt / length;
                self.level = self.start + (end - self.start) * envelope::bend(self.progress, curve);
                return;
            }
            dt -= left;
            self.level = end;
            self.enter(next);
        }
    }
}

impl Node for Modulation {
    fn channels(&self) -> usize {
        1
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.sample_rate = f64::from(config.sample_rate);
        self.fresh = true;
    }

    /// Carries on from the node it replaces: its modulators' shapes, its
    /// routings' lags and the notes held, by modulator and routing id.
    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<Modulation>() else {
            return;
        };
        self.ticked = previous.ticked;
        self.held = previous.held;
        self.held_count = previous.held_count;
        self.note_started = previous.note_started;
        self.first_started = previous.first_started;
        for modulator in &mut self.modulators {
            if let Some(old) = previous.modulators.iter().find(|m| m.id == modulator.id) {
                modulator.take_over(old);
            }
        }
        for route in &mut self.routings {
            if let Some(old) = previous.routings.iter().find(|r| r.id == route.id) {
                route.value = old.value;
                route.started = old.started;
            }
        }
    }

    fn control(&mut self, ctx: &ProcessContext<'_>, controls: &mut Controls<'_>) {
        let grid = |offset: usize| {
            let late = ((ctx.time + offset as u64) % GRID as u64) as usize;
            offset + (GRID - late) % GRID
        };
        let (mut next, mut extra, mut from) = (0, 0, 0);
        loop {
            let mut tick = if self.fresh { 0 } else { grid(from) };
            if let Some(event) = ctx.events.get(next)
                && extra < EXTRA_TICKS
                && event.offset < tick
            {
                tick = event.offset.max(from);
                extra += 1;
            }
            if tick >= ctx.frames {
                break;
            }
            while let Some(event) = ctx.events.get(next)
                && event.offset <= tick
            {
                self.apply(event);
                next += 1;
            }
            self.tick(ctx, tick, controls);
            from = tick + 1;
        }
        // For the next block's first tick.
        for event in &ctx.events[next..] {
            self.apply(event);
        }
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.clear();
    }
}

#[cfg(test)]
mod tests;
