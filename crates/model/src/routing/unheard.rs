//! Notes a channel's modulators can't hear: see [`Project::unheard_notes`].

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Flow, Point, Port, Tap, WireId};
use crate::modulation::{Modulator, ModulatorKind, NoteTrigger, envelope, lfo};
use crate::param::ParamAddress;
use crate::{Chain, DeviceId, Project, TrackKind};

/// Notes that would reach a channel's modulators, as everything that
/// reaches its first instrument does, but can't: they're made as
/// something renders that comes after something the modulators move, so
/// the modulators would have to move it before it made them. The engine
/// leaves out the whole way they'd come, so the modulators hear none of
/// what comes that way, not even the notes on it made by nothing they
/// move. See [`Project::unheard_notes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnheardNotes {
    /// The channel whose modulators don't hear them.
    pub chain: Chain,
    /// The note wire that brings them to the channel's first instrument,
    /// or `None` for the track's own notes: those of a MIDI track whose
    /// input brings them.
    pub wire: Option<WireId>,
    /// The plugin that plays them out as it renders
    /// ([`DeviceKind::makes_notes`](crate::DeviceKind::makes_notes)): the
    /// modulators move it, or something before it.
    pub maker: DeviceId,
}

/// A place notes, sound or modulation pass through, as the engine has
/// them: a point on a channel, or a channel's modulators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Node {
    At(Point),
    Modulators(Chain),
}

impl Project {
    /// The ways notes would reach a channel's modulators that the engine
    /// leaves out, as they'd close a loop, for the channels with a
    /// modulator that hears notes: an LFO that notes start, or an Envelope
    /// that notes gate. A channel's modulators hear what reaches its first
    /// instrument: a MIDI track's own notes, and those note wires bring.
    /// Notes a plugin plays out as it renders
    /// ([`DeviceKind::makes_notes`](crate::DeviceKind::makes_notes)) can't
    /// reach the modulators that move it, or anything before it, as they'd
    /// have to move it before it made them. In channel order, then wire
    /// order.
    ///
    /// It follows the engine's graph point by point, as the loop checks
    /// do: the modulators reach what they move, and what that reaches. It
    /// can't know which plugins load, so it takes every active device to
    /// be there.
    pub fn unheard_notes(&self) -> Vec<UnheardNotes> {
        self.unheard(None)
    }

    /// The ways notes would reach `chain`'s modulators that the engine
    /// leaves out, as [`unheard_notes`](Self::unheard_notes) finds them:
    /// none at once, without following anything, if no modulator of its
    /// hears notes.
    pub fn unheard_notes_of(&self, chain: Chain) -> Vec<UnheardNotes> {
        self.unheard(Some(chain))
    }

    /// [`unheard_notes`](Self::unheard_notes), for `only` channel if it's
    /// given.
    fn unheard(&self, only: Option<Chain>) -> Vec<UnheardNotes> {
        let hearing: Vec<Chain> = (self.modulations())
            .filter(|&(chain, _)| only.is_none_or(|only| only == chain))
            .filter(|(_, modulation)| modulation.modulators.iter().any(|m| self.hears_notes(m)))
            .map(|(chain, _)| chain)
            .collect();
        if hearing.is_empty() {
            return Vec::new();
        }
        let live = self.live(None);
        let made_at = |tap: Tap| match tap {
            Tap::Device(device) if self.makes_notes(device) => Some(device),
            _ => None,
        };
        // Each way on, and the plugin whose made notes take it, if they do.
        let mut next: HashMap<Node, Vec<(Node, Option<DeviceId>)>> = HashMap::new();
        let mut add = |from: Node, to: Node, made: Option<DeviceId>| {
            next.entry(from).or_default().push((to, made));
        };
        for (&from, tos) in &Flow::new(self).next {
            for &to in tos {
                add(Node::At(from), Node::At(to), None);
            }
        }
        for &(track, tap) in &live.inputs {
            if crate::Input::Tap(tap).is_resampling() {
                continue;
            }
            let midi = self.track(track).is_some_and(|t| t.kind == TrackKind::Midi);
            let made = made_at(tap).filter(|_| midi);
            let to = Node::At(Point::Head(Chain::Track(track)));
            add(Node::At(self.input_point(track, tap)), to, made);
        }
        for wire in &live.wires {
            let made = made_at(wire.from).filter(|_| wire.to.takes_notes());
            let to = Node::At(Point::port(wire.to));
            add(Node::At(self.wire_point(wire.from, wire.to)), to, made);
        }
        for (owner, modulation) in self.modulations() {
            let plays = modulation.routings.iter();
            for routing in plays.filter(|r| !r.bypassed && self.routing_plays(owner, r)) {
                let Some(chain) = self.param_chain(routing.to) else {
                    continue;
                };
                // Through the modulators of the parameter's channel, as a
                // relay of theirs.
                if chain != owner {
                    add(Node::Modulators(owner), Node::Modulators(chain), None);
                }
                if let Some(point) = self.moved_point(routing.to, &live.wires) {
                    add(Node::Modulators(chain), Node::At(point), None);
                }
            }
        }
        let mut unheard = Vec::new();
        for chain in hearing {
            for (wire, from, made) in self.notes_heard(chain, &live.wires) {
                let maker = made_on_way(&next, Node::Modulators(chain), Node::At(from));
                if let Some(maker) = maker.or(made.filter(|_| reaches(&next, chain, from))) {
                    unheard.push(UnheardNotes { chain, wire, maker });
                }
            }
        }
        unheard
    }

    /// The ways notes reach `chain`'s modulators, as they reach its first
    /// instrument: a MIDI track's own, from the point that plays them to
    /// its instrument, and each note wire's into the instrument, from the
    /// point it takes them at, with the plugin whose made notes it takes,
    /// if they are.
    fn notes_heard(
        &self,
        chain: Chain,
        wires: &[&super::Wire],
    ) -> Vec<(Option<WireId>, Point, Option<DeviceId>)> {
        let mut heard = Vec::new();
        if let Chain::Track(track) = chain
            && self.track(track).is_some_and(|t| t.kind == TrackKind::Midi)
        {
            heard.push((None, self.notes_point(Tap::PostFx(chain)), None));
        }
        let devices = self.devices(chain).unwrap_or_default();
        let Some(instrument) = devices.iter().find(|d| d.kind.is_instrument()) else {
            return heard;
        };
        if !instrument.active {
            return heard;
        }
        for wire in wires.iter().filter(|w| w.to == Port::Notes(instrument.id)) {
            let made = match wire.from {
                Tap::Device(device) if self.makes_notes(device) => Some(device),
                _ => None,
            };
            heard.push((Some(wire.id), self.wire_point(wire.from, wire.to), made));
        }
        heard
    }

    /// The point whose signal the parameter at `param` moves, as the
    /// engine's node for it sits: none for one whose node is out of the
    /// signal, or that has none, such as a modulator's, which its own
    /// channel's modulators move.
    fn moved_point(&self, param: ParamAddress, wires: &[&super::Wire]) -> Option<Point> {
        let plays = |device: DeviceId| {
            self.find_device(device).is_some_and(|(_, _, found)| {
                found.active && (!found.kind.is_midi_effect() || self.is_midi_effect_tap(device))
            })
        };
        match param {
            ParamAddress::Volume(chain) | ParamAddress::Pan(chain) => Some(Point::PostFader(chain)),
            ParamAddress::Send(send) => (self.tracks.iter())
                .flat_map(|track| self.sends_of(track))
                .find(|s| s.id == send)
                .map(|s| Point::Head(Chain::Track(s.to))),
            // A switch's own id, which no plugin has.
            ParamAddress::Device(_, u32::MAX) => None,
            ParamAddress::Device(device, _) | ParamAddress::Bypass(device) => {
                plays(device).then_some(Point::Device(device))
            }
            ParamAddress::Wire(wire) => wires
                .iter()
                .find(|w| w.id == wire && plays(w.to.device()))
                .map(|w| Point::port(w.to)),
            ParamAddress::Clip(..) | ParamAddress::Modulator(..) => None,
        }
    }

    /// Whether `modulator` hears notes: an LFO in the Note start, or an
    /// Envelope, whose Note Trigger isn't Never, nor Auto while something
    /// moves its own trigger or gate.
    fn hears_notes(&self, modulator: &Modulator) -> bool {
        let (trigger, gate) = match modulator.kind {
            ModulatorKind::Macro => return false,
            ModulatorKind::Lfo => {
                let start = modulator.value(lfo::START as u32).unwrap_or(0.0);
                if lfo::Start::from_value(start) != lfo::Start::Note {
                    return false;
                }
                (lfo::NOTE_TRIGGER, lfo::TRIGGER)
            }
            ModulatorKind::Envelope => (envelope::NOTE_TRIGGER, envelope::GATE),
        };
        let trigger = modulator.value(trigger as u32).unwrap_or(0.0);
        match NoteTrigger::from_value(trigger) {
            NoteTrigger::Never => false,
            NoteTrigger::Always | NoteTrigger::Legato => true,
            NoteTrigger::Auto => {
                let gate = ParamAddress::Modulator(modulator.id, gate as u32);
                let routed = (self.routings_to(gate))
                    .any(|(chain, r)| !r.bypassed && self.routing_plays(chain, r));
                let automated = (self.automation.iter()).any(|l| !l.overridden && l.param == gate);
                !(routed || automated)
            }
        }
    }
}

/// The plugin whose made notes lie on a way from `start` to `target`, if
/// one does: the first such way found.
fn made_on_way(
    next: &HashMap<Node, Vec<(Node, Option<DeviceId>)>>,
    start: Node,
    target: Node,
) -> Option<DeviceId> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([(start, None)]);
    while let Some((node, made)) = queue.pop_front() {
        if node == target && made.is_some() {
            return made;
        }
        if !seen.insert((node, made.is_some())) {
            continue;
        }
        for &(to, on) in next.get(&node).into_iter().flatten() {
            queue.push_back((to, made.or(on)));
        }
    }
    None
}

/// Whether anything passes from `chain`'s modulators to `point`.
fn reaches(
    next: &HashMap<Node, Vec<(Node, Option<DeviceId>)>>,
    chain: Chain,
    point: Point,
) -> bool {
    let mut seen = HashSet::new();
    let mut stack = vec![Node::Modulators(chain)];
    while let Some(node) = stack.pop() {
        if node == Node::At(point) {
            return true;
        }
        if seen.insert(node) {
            stack.extend(next.get(&node).into_iter().flatten().map(|&(to, _)| to));
        }
    }
    false
}
