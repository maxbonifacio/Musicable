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
/// move. Or the loop goes through another channel's modulators, which
/// hear notes too: the modulators move what plays the notes those hear,
/// and those move what plays these, so the channel first in order hears
/// its notes and the other doesn't. See [`Project::unheard_notes`].
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
    /// modulators move it, or something before it, or the notes it makes
    /// lie on the loop through `through`.
    pub maker: DeviceId,
    /// The other channel whose modulators the loop goes through, hearing
    /// notes that come to them, if it does.
    pub through: Option<Chain>,
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
    /// have to move it before it made them, and nor can a way close a
    /// loop through other channels' modulators that hear notes: the
    /// channel first in order hears its notes, and the later one doesn't.
    /// In channel order, then wire order.
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
        if only.is_some_and(|only| !self.modulators_hear_notes(only)) {
            return Vec::new();
        }
        // In channel order, as the engine joins them to its graph.
        let mut hearing: Vec<Chain> = (self.modulations())
            .filter(|(_, modulation)| modulation.modulators.iter().any(|m| self.hears_notes(m)))
            .map(|(chain, _)| chain)
            .collect();
        hearing.sort_unstable();
        if hearing.is_empty() {
            return Vec::new();
        }
        let live = self.live(None);
        let made_at = |tap: Tap| match tap {
            Tap::Device(device) if self.makes_notes(device) => Some(device),
            _ => None,
        };
        // Each way on, and the plugin whose made notes take it, if they do.
        let mut next: Next = HashMap::new();
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
        // Each way on its own first, as nothing else hears notes yet, then
        // the ways that pass that, channel by channel, each with the ways
        // the channels before it hear.
        let ways: Vec<_> = (hearing.iter())
            .flat_map(|&chain| {
                let heard = self.notes_heard(chain, &live.wires).into_iter();
                heard.map(move |(wire, from, made)| (chain, wire, from, made))
            })
            .collect();
        let mut alone = Vec::with_capacity(ways.len());
        for &(chain, wire, from, made) in &ways {
            let loops = made_on_way(&next, Node::Modulators(chain), Node::At(from))
                .map(|(maker, _)| maker)
                .or(made.filter(|_| reaches(&next, chain, from)));
            alone.push(loops.map(|maker| UnheardNotes {
                chain,
                wire,
                maker,
                through: None,
            }));
        }
        let mut unheard = Vec::new();
        let mut joined = next.clone();
        for (&(chain, wire, from, made), alone) in ways.iter().zip(alone) {
            let refused = alone.or_else(|| {
                let way = made_on_way(&joined, Node::Modulators(chain), Node::At(from));
                let (maker, through) = way.or_else(|| {
                    let maker = made?;
                    reaches_through(&joined, chain, from).map(|through| (maker, through))
                })?;
                Some(UnheardNotes {
                    chain,
                    wire,
                    maker,
                    through,
                })
            });
            match refused {
                Some(refused) => unheard.push(refused),
                None => (joined.entry(Node::At(from)).or_default())
                    .push((Node::Modulators(chain), made)),
            }
        }
        unheard.retain(|u| only.is_none_or(|only| only == u.chain));
        unheard
    }

    /// Whether any of `chain`'s modulators hears notes, as
    /// [`unheard_notes`](Self::unheard_notes) has it: an LFO that notes
    /// start, or an Envelope that notes gate.
    pub fn modulators_hear_notes(&self, chain: Chain) -> bool {
        (self.modulation(chain)).is_some_and(|m| m.modulators.iter().any(|m| self.hears_notes(m)))
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

/// Each way on from a node, and the plugin whose made notes take it, if
/// they do.
type Next = HashMap<Node, Vec<(Node, Option<DeviceId>)>>;

/// The plugin whose made notes lie on a way from `start` to `target`, if
/// one does: the first such way found, with the first other channel whose
/// modulators it reaches hearing notes, if it does.
fn made_on_way(next: &Next, start: Node, target: Node) -> Option<(DeviceId, Option<Chain>)> {
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([(start, None, None)]);
    while let Some((node, made, through)) = queue.pop_front() {
        if node == target
            && let Some(made) = made
        {
            return Some((made, through));
        }
        if !seen.insert((node, made.is_some())) {
            continue;
        }
        for &(to, on) in next.get(&node).into_iter().flatten() {
            queue.push_back((to, made.or(on), through.or(heard_at(node, to))));
        }
    }
    None
}

/// The channel whose modulators hear notes from `from` on the way to `to`,
/// if they do: no other way goes from a point to modulators.
fn heard_at(from: Node, to: Node) -> Option<Chain> {
    match (from, to) {
        (Node::At(_), Node::Modulators(chain)) => Some(chain),
        _ => None,
    }
}

/// Whether anything passes from `chain`'s modulators to `point`.
fn reaches(next: &Next, chain: Chain, point: Point) -> bool {
    reaches_through(next, chain, point).is_some()
}

/// Whether anything passes from `chain`'s modulators to `point`, with the
/// first other channel whose modulators it reaches hearing notes on the
/// way, if it does.
fn reaches_through(next: &Next, chain: Chain, point: Point) -> Option<Option<Chain>> {
    let mut seen = HashSet::new();
    let mut stack = vec![(Node::Modulators(chain), None)];
    while let Some((node, through)) = stack.pop() {
        if node == Node::At(point) {
            return Some(through);
        }
        if seen.insert(node) {
            for &(to, _) in next.get(&node).into_iter().flatten() {
                stack.push((to, through.or(heard_at(node, to))));
            }
        }
    }
    None
}
