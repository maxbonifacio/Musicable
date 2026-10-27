//! Modulators: controls and shapes that move other parameters as the song
//! plays, without changing their values.
//!
//! Each track has a [`Modulation`], and so does the master: its
//! [`Modulator`]s, in the order the modulator strip shows them, and the
//! [`Routing`]s from their outputs to the parameters they move. There are
//! three kinds ([`ModulatorKind`]): a Macro, whose controls are played by
//! hand; an [LFO](lfo), a shape that repeats; and an [Envelope](envelope),
//! a shape that runs once each time it's started.
//!
//! A modulator's outputs ([`Output`]) are numbers from 0 to 1, or from -1
//! to 1 for an LFO that isn't unipolar. A routing takes one, bends it with
//! its [`Curve`], scales it by its amount and by another output if it has
//! one, and smooths it over its lag. The engine adds the routings to each
//! parameter on top of its value or its automation, and keeps the sum in
//! the parameter's range, so modulation never changes the project: neither
//! its values nor the undo history.
//!
//! A modulator reaches the parameters of the channel it's on: its devices,
//! its mixer strip and the other modulators there. A group's modulators
//! also reach the tracks that feed the group, and the master's reach every
//! track ([`Project::in_reach`]). A routing to a parameter out of reach,
//! after its track leaves the group, say, waits unplayed until it's back.
//!
//! A modulator's parameters are numbered as a built-in device's are: an
//! LFO's and an Envelope's by their place in their kind's table, which only
//! ever grows at the end, and a Macro's by its controls' ids.
//! [`ParamAddress::Modulator`] names one anywhere in the project, so
//! automation and other modulators can move it too.

pub mod envelope;
pub mod lfo;

use crate::native::Scale::Linear;
use crate::native::Unit::{Fraction, Switch};
use crate::native::{ParamInfo, param};
use crate::param::ParamAddress;
use crate::{Chain, Project, TrackId, next_id};

/// Stable identifier for a modulator, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModulatorId(pub u32);

/// Stable identifier for a routing, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoutingId(pub u32);

/// How notes start a modulator that restarts, as Phase Plant's trigger
/// modes do, in order: see [`NoteTrigger`].
pub const TRIGGERS: [&str; 4] = ["Auto", "Always", "Legato", "Never"];

/// One of [`TRIGGERS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteTrigger {
    /// As Always, unless something moves the modulator's own trigger or
    /// gate: then as Never, so the routing that does has it to itself.
    Auto,
    /// Every note starts it.
    Always,
    /// The first of notes that overlap starts it, and the next notes don't
    /// until they've all ended.
    Legato,
    /// No note starts it.
    Never,
}

impl NoteTrigger {
    pub const ALL: [NoteTrigger; 4] = [Self::Auto, Self::Always, Self::Legato, Self::Never];

    /// The mode a Note Trigger parameter picks, rounded and kept in range.
    pub fn from_value(value: f64) -> NoteTrigger {
        Self::ALL[step(value, Self::ALL.len())]
    }
}

/// The index a choice's `value` picks among `count` choices, rounded and
/// kept in range: 0 for not a number.
fn step(value: f64, count: usize) -> usize {
    if value.is_nan() {
        return 0;
    }
    value.round().clamp(0.0, (count - 1) as f64) as usize
}

/// A channel's modulators and their routings.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Modulation {
    /// In the order the modulator strip shows them.
    pub modulators: Vec<Modulator>,
    /// In any order, each from one of `modulators`' outputs, and at most
    /// one from each output to each parameter.
    pub routings: Vec<Routing>,
}

impl Modulation {
    pub fn is_empty(&self) -> bool {
        self.modulators.is_empty() && self.routings.is_empty()
    }

    pub fn modulator(&self, id: ModulatorId) -> Option<&Modulator> {
        self.modulators.iter().find(|m| m.id == id)
    }

    pub fn modulator_mut(&mut self, id: ModulatorId) -> Option<&mut Modulator> {
        self.modulators.iter_mut().find(|m| m.id == id)
    }

    /// Where the modulator with `id` is in the strip.
    pub fn modulator_index(&self, id: ModulatorId) -> Option<usize> {
        self.modulators.iter().position(|m| m.id == id)
    }

    pub fn routing(&self, id: RoutingId) -> Option<&Routing> {
        self.routings.iter().find(|r| r.id == id)
    }

    pub fn routing_mut(&mut self, id: RoutingId) -> Option<&mut Routing> {
        self.routings.iter_mut().find(|r| r.id == id)
    }

    /// Where the routing with `id` is in the list.
    pub fn routing_index(&self, id: RoutingId) -> Option<usize> {
        self.routings.iter().position(|r| r.id == id)
    }

    /// The routing from `from` to `to`, if there's one.
    pub fn routing_between(&self, from: Output, to: ParamAddress) -> Option<&Routing> {
        self.routings.iter().find(|r| r.from == from && r.to == to)
    }

    /// Whether one of its modulators has `output`.
    pub fn has_output(&self, output: Output) -> bool {
        self.modulator(output.modulator)
            .is_some_and(|m| m.has_output(output.n))
    }

    /// A name for a new modulator of `kind` in the strip: its kind's name
    /// and the first number from 1 that no modulator there has with it,
    /// "LFO 2".
    pub fn new_name(&self, kind: ModulatorKind) -> String {
        let taken = |name: &str| self.modulators.iter().any(|m| m.name == name);
        (1..)
            .map(|n| format!("{} {n}", kind.name()))
            .find(|name| !taken(name))
            .expect("a free number")
    }
}

/// Which kind of modulator a modulator is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModulatorKind {
    /// Knobs and buttons played by hand, each an output.
    Macro,
    /// A shape that repeats: see [`lfo`].
    Lfo,
    /// A shape that runs once each time it's started: see [`envelope`].
    Envelope,
}

impl ModulatorKind {
    /// Every kind, in the order the strip's menu lists them.
    pub const ALL: [ModulatorKind; 3] = [Self::Macro, Self::Lfo, Self::Envelope];

    /// What the strip calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Macro => "Macro",
            Self::Lfo => "LFO",
            Self::Envelope => "Envelope",
        }
    }

    /// What project files and commands call it: "macro", "lfo".
    pub fn key(self) -> &'static str {
        match self {
            Self::Macro => "macro",
            Self::Lfo => "lfo",
            Self::Envelope => "envelope",
        }
    }

    /// The kind whose [`key`](Self::key) is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// Its table of parameters, in order of number: none for a Macro,
    /// whose controls are its parameters.
    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            Self::Macro => &[],
            Self::Lfo => &lfo::PARAMS,
            Self::Envelope => &envelope::PARAMS,
        }
    }
}

/// A modulator on a track or the master.
#[derive(Debug, Clone, PartialEq)]
pub struct Modulator {
    pub id: ModulatorId,
    pub kind: ModulatorKind,
    /// What the strip calls it: "LFO 1".
    pub name: String,
    /// An LFO's or an Envelope's value for each parameter in its kind's
    /// table, in order of number, in the parameter's units. None for a
    /// Macro, whose controls have theirs.
    pub values: Vec<f64>,
    /// A Macro's controls, in the order it shows them. None for the other
    /// kinds.
    pub controls: Vec<Control>,
    /// The id the Macro's next control gets. Like the project's counters,
    /// it stays above every control's id, so ids aren't used again.
    pub next_control: u32,
}

/// One of a Macro's controls: a knob or a button, its parameter and its
/// output.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    /// The number of its parameter and of its output, unique in its Macro.
    pub id: u32,
    pub name: String,
    pub kind: ControlKind,
    /// From 0.0 to 1.0: a button's is 0.0 while it's off and 1.0 while
    /// it's on.
    pub value: f64,
}

/// Whether a Macro's control is a knob or a button, and how the button
/// works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlKind {
    /// Turns from 0 to 1.
    Knob,
    /// A button that each press turns on or off.
    Toggle,
    /// A button that's on while it's held.
    Momentary,
}

impl ControlKind {
    pub const ALL: [ControlKind; 3] = [Self::Knob, Self::Toggle, Self::Momentary];

    /// What project files and commands call it: "knob", "toggle".
    pub fn key(self) -> &'static str {
        match self {
            Self::Knob => "knob",
            Self::Toggle => "toggle",
            Self::Momentary => "momentary",
        }
    }

    /// The kind whose [`key`](Self::key) is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// What a control of the kind is as a parameter.
    pub fn param(self) -> &'static ParamInfo {
        match self {
            Self::Knob => &KNOB,
            Self::Toggle | Self::Momentary => &BUTTON,
        }
    }

    /// Whether it's a button, on or off.
    pub fn is_button(self) -> bool {
        self != Self::Knob
    }
}

/// A Macro's knob as a parameter.
const KNOB: ParamInfo = param("knob", "Knob", "", [0.0, 1.0, 0.0], Fraction, Linear);

/// A Macro's button as a parameter: a switch.
const BUTTON: ParamInfo = ParamInfo {
    stepped: true,
    modulated: false,
    unit: Switch,
    ..param("button", "Button", "", [0.0, 1.0, 0.0], Fraction, Linear)
};

impl Modulator {
    /// A modulator of `kind` with id `id`: every parameter at its default,
    /// and for a Macro, one knob.
    pub fn new(id: ModulatorId, kind: ModulatorKind, name: impl Into<String>) -> Self {
        let mut modulator = Self {
            id,
            kind,
            name: name.into(),
            values: kind.params().iter().map(|p| p.default).collect(),
            controls: Vec::new(),
            next_control: 0,
        };
        if kind == ModulatorKind::Macro {
            let knob = modulator.new_control(ControlKind::Knob);
            modulator.controls.push(knob);
        }
        modulator
    }

    /// A new control of `kind` for the Macro, with the next control id and
    /// a name that no control has, "Knob 2", for the caller to add. Taking
    /// an id moves the Macro's counter on.
    pub fn new_control(&mut self, kind: ControlKind) -> Control {
        let id = next_id(&mut self.next_control, "control");
        let word = if kind.is_button() { "Button" } else { "Knob" };
        let taken = |name: &str| self.controls.iter().any(|c| c.name == name);
        let name = (1..)
            .map(|n| format!("{word} {n}"))
            .find(|name| !taken(name))
            .expect("a free number");
        Control {
            id,
            name,
            kind,
            value: 0.0,
        }
    }

    pub fn control(&self, id: u32) -> Option<&Control> {
        self.controls.iter().find(|c| c.id == id)
    }

    pub fn control_mut(&mut self, id: u32) -> Option<&mut Control> {
        self.controls.iter_mut().find(|c| c.id == id)
    }

    /// Where the control with `id` is among the Macro's controls.
    pub fn control_index(&self, id: u32) -> Option<usize> {
        self.controls.iter().position(|c| c.id == id)
    }

    /// The numbers of its parameters, in order: its kind's table, or a
    /// Macro's controls.
    pub fn params(&self) -> Vec<u32> {
        match self.kind {
            ModulatorKind::Macro => self.controls.iter().map(|c| c.id).collect(),
            kind => (0..kind.params().len() as u32).collect(),
        }
    }

    /// What parameter `n` is, if the modulator has it.
    pub fn param(&self, n: u32) -> Option<&'static ParamInfo> {
        match self.kind {
            ModulatorKind::Macro => self.control(n).map(|c| c.kind.param()),
            kind => kind.params().get(n as usize),
        }
    }

    /// What parameter `n` is called: its control's name for a Macro, and
    /// with its group's otherwise, "Rate Note".
    pub fn param_name(&self, n: u32) -> Option<String> {
        match self.kind {
            ModulatorKind::Macro => self.control(n).map(|c| c.name.clone()),
            _ => self.param(n).map(ParamInfo::full_name),
        }
    }

    /// Parameter `n`'s value, if the modulator has it.
    pub fn value(&self, n: u32) -> Option<f64> {
        match self.kind {
            ModulatorKind::Macro => self.control(n).map(|c| c.value),
            _ => self.values.get(n as usize).copied(),
        }
    }

    /// Sets parameter `n` to `value`, kept in its range and to its steps
    /// ([`ParamInfo::clamp`]), and returns what it was: `None`, changing
    /// nothing, if the modulator hasn't the parameter.
    pub fn set(&mut self, n: u32, value: f64) -> Option<f64> {
        let info = self.param(n)?;
        let slot = match self.kind {
            ModulatorKind::Macro => &mut self.control_mut(n)?.value,
            _ => self.values.get_mut(n as usize)?,
        };
        Some(std::mem::replace(slot, info.clamp(value)))
    }

    /// The numbers of its outputs, in order: one for each of a Macro's
    /// controls, its control's id, and output 0 for an LFO or an Envelope.
    pub fn outputs(&self) -> Vec<u32> {
        match self.kind {
            ModulatorKind::Macro => self.controls.iter().map(|c| c.id).collect(),
            ModulatorKind::Lfo | ModulatorKind::Envelope => vec![0],
        }
    }

    pub fn has_output(&self, n: u32) -> bool {
        match self.kind {
            ModulatorKind::Macro => self.control(n).is_some(),
            ModulatorKind::Lfo | ModulatorKind::Envelope => n == 0,
        }
    }

    /// What output `n` is called: its control's name for a Macro, and the
    /// modulator's own otherwise.
    pub fn output_name(&self, n: u32) -> Option<String> {
        match self.kind {
            ModulatorKind::Macro => self.control(n).map(|c| c.name.clone()),
            _ => self.has_output(n).then(|| self.name.clone()),
        }
    }

    /// Whether output `n` goes from -1 to 1 rather than from 0 to 1: an
    /// LFO's, unless it's unipolar.
    pub fn bipolar(&self, n: u32) -> bool {
        self.kind == ModulatorKind::Lfo
            && n == 0
            && self.values.get(lfo::UNIPOLAR).is_some_and(|&v| v < 0.5)
    }

    /// Whether a project file can hold it: a name, a valid value for each
    /// of its kind's parameters, and for a Macro, controls with names and
    /// valid values, and ids that are unique and below its counter.
    pub fn is_valid(&self) -> bool {
        let params = self.kind.params();
        let values = self.values.len() == params.len()
            && params.iter().zip(&self.values).all(|(p, &v)| p.is_valid(v));
        let controls = match self.kind {
            ModulatorKind::Macro => self.controls.iter().enumerate().all(|(i, c)| {
                !c.name.is_empty()
                    && c.id < self.next_control
                    && c.kind.param().is_valid(c.value)
                    && self.controls[..i].iter().all(|other| other.id != c.id)
            }),
            _ => self.controls.is_empty(),
        };
        !self.name.is_empty() && values && controls
    }
}

/// One of a modulator's outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Output {
    pub modulator: ModulatorId,
    /// Which of its outputs: see [`Modulator::outputs`].
    pub n: u32,
}

impl Output {
    pub fn new(modulator: ModulatorId, n: u32) -> Self {
        Self { modulator, n }
    }
}

/// A modulator's output moving a parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Routing {
    pub id: RoutingId,
    /// The output it takes, from a modulator on the routing's channel.
    pub from: Output,
    /// The parameter it moves.
    pub to: ParamAddress,
    /// How far it moves the parameter while the output is at 1, before the
    /// curve. For a parameter the project knows the range of, that's a
    /// share of the range, from -1.0 to 1.0, along the parameter's control:
    /// at 0.25 it moves it a quarter of the way up, evenly along its scale,
    /// so in octaves on a cutoff. For a plugin's parameter it's in the
    /// plugin's own units, as CLAP advises hosts to keep modulation, so the
    /// sound holds if a new version of the plugin widens the range. Below
    /// 0, it moves the parameter the other way.
    pub amount: f64,
    pub curve: Curve,
    /// Another output on the same channel that multiplies the amount, as
    /// velocity would: `None` for none.
    pub scale: Option<Output>,
    /// How long it takes to follow its output, in ms, as a gentle low pass
    /// does: about two thirds of the way in that time. 0 for at once.
    pub lag: f64,
    /// Whether it's bypassed: it moves nothing, and keeps its settings.
    pub bypassed: bool,
}

impl Routing {
    /// The longest lag: 10 s.
    pub const LAG_MAX: f64 = 10_000.0;

    /// A routing from `from` to `to` by `amount`, straight, with no scale,
    /// no lag, and not bypassed.
    pub fn new(id: RoutingId, from: Output, to: ParamAddress, amount: f64) -> Self {
        Self {
            id,
            from,
            to,
            amount,
            curve: Curve::Linear,
            scale: None,
            lag: 0.0,
            bypassed: false,
        }
    }

    /// Whether a project file can hold its settings: a finite amount, a
    /// curve with a good number of steps, and a lag from 0 to
    /// [`LAG_MAX`](Self::LAG_MAX). Whether its output and its parameter
    /// are there is the project's to say.
    pub fn is_valid(&self) -> bool {
        self.amount.is_finite()
            && self.curve.is_valid()
            && (0.0..=Self::LAG_MAX).contains(&self.lag)
    }
}

/// How a routing bends its output before scaling it, as Bitwig's
/// transfer curves do. A bipolar output bends evenly about 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Curve {
    /// Straight.
    #[default]
    Linear,
    /// Slowly at first, then fast: the output squared, keeping its sign.
    Exponential,
    /// Fast at first, then slowly: the output's square root, keeping its
    /// sign.
    Logarithmic,
    /// Slowly at both ends and fast in the middle.
    SCurve,
    /// In this many even steps, from 2 to [`STEPS_MAX`](Curve::STEPS_MAX).
    Stepped(u32),
    /// The output's size without its sign, so a bipolar output only moves
    /// the parameter one way.
    Absolute,
    /// Only where the output is above 0.
    Positive,
    /// Only where the output is below 0.
    Negative,
}

impl Curve {
    /// The most steps a stepped curve can have.
    pub const STEPS_MAX: u32 = 128;

    /// The keys of the curves, as [`key`](Self::key) gives them.
    pub const KEYS: [&str; 8] = [
        "linear",
        "exponential",
        "logarithmic",
        "s_curve",
        "stepped",
        "absolute",
        "positive",
        "negative",
    ];

    /// What project files and commands call it: "linear", "s_curve".
    pub fn key(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Exponential => "exponential",
            Self::Logarithmic => "logarithmic",
            Self::SCurve => "s_curve",
            Self::Stepped(_) => "stepped",
            Self::Absolute => "absolute",
            Self::Positive => "positive",
            Self::Negative => "negative",
        }
    }

    /// The curve whose key is `key`, with `steps` steps if it's stepped.
    pub fn from_key(key: &str, steps: u32) -> Option<Self> {
        Some(match key {
            "linear" => Self::Linear,
            "exponential" => Self::Exponential,
            "logarithmic" => Self::Logarithmic,
            "s_curve" => Self::SCurve,
            "stepped" => Self::Stepped(steps),
            "absolute" => Self::Absolute,
            "positive" => Self::Positive,
            "negative" => Self::Negative,
            _ => return None,
        })
    }

    pub fn is_valid(self) -> bool {
        match self {
            Self::Stepped(steps) => (2..=Self::STEPS_MAX).contains(&steps),
            _ => true,
        }
    }

    /// `x`, an output from 0 to 1, or from -1 to 1 if it's `bipolar`,
    /// bent by the curve.
    pub fn apply(self, x: f64, bipolar: bool) -> f64 {
        // Bends that keep the sign work on the size.
        let signed = |f: fn(f64) -> f64| f(x.abs()).copysign(x);
        match self {
            Self::Linear => x,
            Self::Exponential => signed(|a| a * a),
            Self::Logarithmic => signed(f64::sqrt),
            Self::SCurve | Self::Stepped(_) => {
                // From 0 to 1 across the output's range, and back.
                let t = if bipolar { 0.5 * (x + 1.0) } else { x };
                let t = t.clamp(0.0, 1.0);
                let bent = match self {
                    Self::Stepped(steps) => {
                        let steps = f64::from(steps.max(2));
                        ((t * steps).floor()).min(steps - 1.0) / (steps - 1.0)
                    }
                    _ => t * t * (3.0 - 2.0 * t),
                };
                if bipolar { 2.0 * bent - 1.0 } else { bent }
            }
            Self::Absolute => x.abs(),
            Self::Positive => x.max(0.0),
            Self::Negative => x.min(0.0),
        }
    }
}

impl Project {
    /// The modulation of a channel: `None` for a track the project doesn't
    /// have.
    pub fn modulation(&self, chain: Chain) -> Option<&Modulation> {
        match chain {
            Chain::Track(id) => self.track(id).map(|t| &t.modulation),
            Chain::Master => Some(&self.master.modulation),
        }
    }

    pub fn modulation_mut(&mut self, chain: Chain) -> Option<&mut Modulation> {
        match chain {
            Chain::Track(id) => self.track_mut(id).map(|t| &mut t.modulation),
            Chain::Master => Some(&mut self.master.modulation),
        }
    }

    /// Every channel's modulation, the tracks' in order and then the
    /// master's.
    pub fn modulations(&self) -> impl Iterator<Item = (Chain, &Modulation)> {
        let tracks = (self.tracks.iter()).map(|t| (Chain::Track(t.id), &t.modulation));
        tracks.chain([(Chain::Master, &self.master.modulation)])
    }

    /// The modulator with `id`, the channel it's on, and where it is in the
    /// strip.
    pub fn find_modulator(&self, id: ModulatorId) -> Option<(Chain, usize, &Modulator)> {
        self.modulations().find_map(|(chain, modulation)| {
            let index = modulation.modulator_index(id)?;
            Some((chain, index, &modulation.modulators[index]))
        })
    }

    pub fn modulator_mut(&mut self, id: ModulatorId) -> Option<&mut Modulator> {
        let tracks = self.tracks.iter_mut().map(|t| &mut t.modulation);
        let mut all = tracks.chain([&mut self.master.modulation]);
        all.find_map(|m| m.modulator_mut(id))
    }

    /// The routing with `id`, the channel it's on, and where it is in the
    /// channel's list.
    pub fn find_routing(&self, id: RoutingId) -> Option<(Chain, usize, &Routing)> {
        self.modulations().find_map(|(chain, modulation)| {
            let index = modulation.routing_index(id)?;
            Some((chain, index, &modulation.routings[index]))
        })
    }

    pub fn routing_mut(&mut self, id: RoutingId) -> Option<&mut Routing> {
        let tracks = self.tracks.iter_mut().map(|t| &mut t.modulation);
        let mut all = tracks.chain([&mut self.master.modulation]);
        all.find_map(|m| m.routing_mut(id))
    }

    /// A modulator of `kind` with the next modulator id, named `name`, for
    /// the caller to add: see [`Modulator::new`].
    pub fn new_modulator(&mut self, kind: ModulatorKind, name: impl Into<String>) -> Modulator {
        let id = ModulatorId(next_id(&mut self.next_modulator_id, "modulator"));
        Modulator::new(id, kind, name)
    }

    /// A routing with the next routing id, for the caller to add: see
    /// [`Routing::new`].
    pub fn new_routing(&mut self, from: Output, to: ParamAddress, amount: f64) -> Routing {
        let id = RoutingId(next_id(&mut self.next_routing_id, "routing"));
        Routing::new(id, from, to, amount)
    }

    /// Whether modulators can move the parameter at `param`, wherever they
    /// are: one the project has that changes smoothly. The mixer's faders,
    /// pans and sends can, and so can wires' gains, the test devices'
    /// parameters and any plugin parameter: a plugin ignores modulation of
    /// parameters it doesn't take it for. Built-in devices' and modulators' parameters
    /// can if their tables say so. Bypasses and clips' parameters can't.
    pub fn can_modulate(&self, param: ParamAddress) -> bool {
        if !self.has_param(param) {
            return false;
        }
        match param {
            ParamAddress::Volume(_)
            | ParamAddress::Pan(_)
            | ParamAddress::Send(_)
            | ParamAddress::Wire(_) => true,
            ParamAddress::Device(..) => self.native_param(param).is_none_or(|p| p.modulated),
            ParamAddress::Modulator(modulator, n) => {
                let (_, _, modulator) = self.find_modulator(modulator).expect("has the param");
                modulator.param(n).is_some_and(|p| p.modulated)
            }
            ParamAddress::Bypass(_) | ParamAddress::Clip(..) => false,
        }
    }

    /// Whether a modulator on `owner` reaches the parameter at `param`:
    /// one on the same channel, or on a track that feeds `owner` if it's a
    /// group, through other groups too, or anywhere if `owner` is the
    /// master. A parameter the project hasn't is out of reach.
    pub fn in_reach(&self, owner: Chain, param: ParamAddress) -> bool {
        match (owner, self.param_chain(param)) {
            (_, None) => false,
            (Chain::Master, Some(_)) => true,
            (Chain::Track(_), Some(Chain::Master)) => false,
            (Chain::Track(owner), Some(Chain::Track(track))) => {
                owner == track || self.feeds_group(track, owner)
            }
        }
    }

    /// Whether the track with id `track` feeds the group track with id
    /// `group`, as the signal really flows ([`Project::output_of`]),
    /// directly or through other groups.
    fn feeds_group(&self, track: TrackId, group: TrackId) -> bool {
        let mut at = track;
        // Outputs never loop, so this ends within as many steps as there
        // are tracks.
        for _ in 0..self.tracks.len() {
            let Some(track) = self.track(at) else {
                return false;
            };
            match self.output_of(track) {
                crate::mixer::Bus::Group(next) if next == group => return true,
                crate::mixer::Bus::Group(next) => at = next,
                crate::mixer::Bus::Master => return false,
            }
        }
        false
    }

    /// Whether `routing`, on `owner`, plays: its output is on `owner`, and
    /// its scale's too if it has one, and its parameter is one modulators
    /// can move, in reach of `owner`.
    pub fn routing_plays(&self, owner: Chain, routing: &Routing) -> bool {
        let Some(modulation) = self.modulation(owner) else {
            return false;
        };
        modulation.has_output(routing.from)
            && routing.scale.is_none_or(|s| modulation.has_output(s))
            && self.can_modulate(routing.to)
            && self.in_reach(owner, routing.to)
    }

    /// The routings to the parameter at `param`, from any channel, with
    /// the channel each is on.
    pub fn routings_to(&self, param: ParamAddress) -> impl Iterator<Item = (Chain, &Routing)> {
        self.modulations().flat_map(move |(chain, modulation)| {
            (modulation.routings.iter())
                .filter(move |r| r.to == param)
                .map(move |r| (chain, r))
        })
    }

    /// Whether `amount` suits a routing to the parameter at `param`: a
    /// finite number, from -1 to 1 for a parameter the project knows the
    /// range of ([`Routing::amount`]).
    pub fn amount_fits(&self, param: ParamAddress, amount: f64) -> bool {
        let plugin = matches!(param, ParamAddress::Device(..)) && self.param_spec(param).is_none();
        amount.is_finite() && (plugin || (-1.0..=1.0).contains(&amount))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::tests::assert_well_formed;
    use crate::{DeviceKind, TrackKind};

    #[test]
    fn tables_are_well_formed() {
        for kind in ModulatorKind::ALL {
            assert_eq!(ModulatorKind::from_key(kind.key()), Some(kind));
            assert_well_formed(kind.name(), kind.params());
        }
        assert_well_formed("Macro controls", &[KNOB, BUTTON]);
        for kind in ControlKind::ALL {
            assert_eq!(ControlKind::from_key(kind.key()), Some(kind));
        }
        for key in Curve::KEYS {
            assert_eq!(Curve::from_key(key, 4).unwrap().key(), key);
        }
    }

    #[test]
    fn new_modulators_start_at_their_defaults() {
        let mut p = Project::new();
        let lfo = p.new_modulator(ModulatorKind::Lfo, "LFO 1");
        assert!(lfo.is_valid());
        assert_eq!(lfo.values.len(), lfo::PARAMS.len());
        assert_eq!(lfo.value(lfo::RATE as u32), Some(1.0));
        assert_eq!(lfo.outputs(), [0]);
        assert!(lfo.bipolar(0));
        let mut macros = p.new_modulator(ModulatorKind::Macro, "Macro 1");
        assert_ne!(macros.id, lfo.id);
        assert!(macros.is_valid());
        assert_eq!(macros.controls.len(), 1);
        assert_eq!(macros.controls[0].name, "Knob 1");
        let button = macros.new_control(ControlKind::Toggle);
        assert_eq!((button.id, button.name.as_str()), (1, "Button 1"));
        macros.controls.push(button);
        assert_eq!(macros.outputs(), [0, 1]);
        assert!(!macros.bipolar(0));
        assert_eq!(macros.param(1).map(|p| p.stepped), Some(true));
        assert_eq!(macros.set(0, 2.0), Some(0.0));
        assert_eq!(macros.value(0), Some(1.0));
        assert_eq!(macros.set(1, 0.7), Some(0.0));
        assert_eq!(macros.value(1), Some(1.0));
        assert_eq!(macros.set(7, 0.5), None);
        let mut envelope = p.new_modulator(ModulatorKind::Envelope, "Envelope 1");
        assert!(!envelope.bipolar(0));
        envelope.values.pop();
        assert!(!envelope.is_valid());
    }

    #[test]
    fn names_take_the_first_free_number() {
        let mut p = Project::new();
        let mut modulation = Modulation::default();
        assert_eq!(modulation.new_name(ModulatorKind::Lfo), "LFO 1");
        modulation
            .modulators
            .push(p.new_modulator(ModulatorKind::Lfo, "LFO 1"));
        assert_eq!(modulation.new_name(ModulatorKind::Lfo), "LFO 2");
        assert_eq!(modulation.new_name(ModulatorKind::Envelope), "Envelope 1");
    }

    #[test]
    fn curves_bend_outputs_about_their_middle() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-12;
        assert_eq!(Curve::Linear.apply(-0.5, true), -0.5);
        assert!(close(Curve::Exponential.apply(-0.5, true), -0.25));
        assert!(close(Curve::Logarithmic.apply(0.25, false), 0.5));
        for bipolar in [false, true] {
            let low = if bipolar { -1.0 } else { 0.0 };
            for curve in [Curve::SCurve, Curve::Stepped(4), Curve::Exponential] {
                assert!(close(curve.apply(low, bipolar), low), "{curve:?}");
                assert!(close(curve.apply(1.0, bipolar), 1.0), "{curve:?}");
            }
        }
        assert!(close(Curve::SCurve.apply(0.0, true), 0.0));
        assert!(Curve::SCurve.apply(0.25, false) < 0.25);
        let steps: Vec<f64> = [0.0, 0.3, 0.5, 0.8, 1.0]
            .map(|x| Curve::Stepped(3).apply(x, false))
            .to_vec();
        assert_eq!(steps, [0.0, 0.0, 0.5, 1.0, 1.0]);
        assert_eq!(Curve::Absolute.apply(-0.5, true), 0.5);
        assert_eq!(Curve::Positive.apply(-0.5, true), 0.0);
        assert_eq!(Curve::Negative.apply(-0.5, true), -0.5);
        assert!(!Curve::Stepped(1).is_valid());
        assert!(!Curve::Stepped(Curve::STEPS_MAX + 1).is_valid());
    }

    #[test]
    fn modulators_reach_their_channel_and_what_feeds_it() {
        let mut p = Project::new();
        let group = p.add_track("Drums", TrackKind::Group);
        let kick = p.add_track("Kick", TrackKind::Audio);
        let bass = p.add_track("Bass", TrackKind::Audio);
        p.track_mut(kick).unwrap().output = crate::mixer::Bus::Group(group);
        let gain = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        let gain_id = gain.id;
        p.track_mut(kick).unwrap().devices.push(gain);
        let lfo = p.new_modulator(ModulatorKind::Lfo, "LFO 1");
        let lfo_id = lfo.id;
        p.track_mut(bass).unwrap().modulation.modulators.push(lfo);

        let on = |track| Chain::Track(track);
        let kick_gain = ParamAddress::Device(gain_id, 0);
        assert!(p.in_reach(on(kick), kick_gain));
        assert!(p.in_reach(on(group), kick_gain));
        assert!(p.in_reach(Chain::Master, kick_gain));
        assert!(!p.in_reach(on(bass), kick_gain));
        assert!(!p.in_reach(on(kick), ParamAddress::Volume(Chain::Master)));
        assert!(!p.in_reach(on(kick), ParamAddress::Volume(on(group))));
        let rate = ParamAddress::Modulator(lfo_id, lfo::RATE as u32);
        assert!(p.in_reach(on(bass), rate));
        assert!(!p.in_reach(on(group), rate));
        assert!(p.can_modulate(rate));
        assert!(!p.can_modulate(ParamAddress::Modulator(lfo_id, lfo::SHAPE as u32)));
        assert!(!p.can_modulate(ParamAddress::Modulator(lfo_id, 99)));
        assert!(!p.can_modulate(ParamAddress::Bypass(gain_id)));
        assert!(p.can_modulate(kick_gain));
        assert!(p.amount_fits(kick_gain, -1.0));
        assert!(!p.amount_fits(kick_gain, 1.5));

        let mut routing = p.new_routing(Output::new(lfo_id, 0), rate, 0.5);
        assert!(routing.is_valid());
        assert!(p.routing_plays(on(bass), &routing));
        assert!(!p.routing_plays(on(kick), &routing));
        routing.lag = -1.0;
        assert!(!routing.is_valid());
    }
}
