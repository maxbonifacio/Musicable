//! Building processing graphs and preparing them to run.

use std::cmp::Ordering;
use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use musicable_model::clip::{Controller, Note};

use crate::automation::Feed;
use crate::buffer::{AudioBuffer, AudioRef, InputPort};
use crate::engine::EVENT_CAPACITY;
use crate::load::{self, LoadKey, NodeLoads};
use crate::nodes::Delay;
use crate::parallel::Schedule;
use crate::queue;
use crate::{
    Automation, Block, EngineConfig, Node, NodeEvent, NodeEventKind, NoteEvent, NoteId, ParamId,
    Renderer,
};

/// A node's identity across graph rebuilds, chosen by whoever builds the
/// graph. [`compile`](crate::compile()) derives keys from model ids.
///
/// When a new graph replaces the running one, each node takes over the state
/// of the old node with the same key, and parameter events find their node by
/// key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey(pub u64);

/// A node's index in one graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    UnknownNode(NodeId),
    /// Connecting these nodes would create a feedback loop.
    Cycle,
    /// Two nodes have the same key.
    DuplicateKey(NodeKey),
    /// No output node was set.
    NoOutput,
    /// The node reports zero channels.
    NoChannels(NodeId),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "no node {}", id.0),
            Self::Cycle => write!(f, "the connection would create a feedback loop"),
            Self::DuplicateKey(key) => write!(f, "two nodes have key {}", key.0),
            Self::NoOutput => write!(f, "the graph has no output node"),
            Self::NoChannels(id) => write!(f, "node {} has no channels", id.0),
        }
    }
}

impl std::error::Error for GraphError {}

/// A processing graph being built: keyed nodes connected into a directed
/// acyclic graph. [`prepare`](Graph::prepare) turns it into something an
/// engine can run. Build graphs off the audio thread.
#[derive(Default)]
pub struct Graph {
    nodes: Vec<Box<dyn Node>>,
    keys: Vec<NodeKey>,
    /// `inputs[n]` lists the nodes feeding node `n`, in connection order.
    inputs: Vec<Vec<NodeId>>,
    /// `ports[n][i]` is the port of node `n` that `inputs[n][i]` feeds.
    ports: Vec<Vec<InputPort>>,
    /// `outputs[n]` lists the nodes that node `n` feeds.
    outputs: Vec<Vec<NodeId>>,
    output: Option<NodeId>,
    cue: Option<NodeId>,
    /// The nodes [`add_tap`](Graph::add_tap) tapped, in order: in a half
    /// of a graph, those in the half.
    taps: Vec<Option<NodeId>>,
    /// The taps [`add_record`](Graph::add_record) added, by their writers'
    /// keys, with their numbers.
    records: Vec<(NodeKey, usize)>,
    /// The nodes whose notes [`add_note_record`](Graph::add_note_record)
    /// records, by their writers' keys.
    note_records: Vec<(NodeKey, NodeId)>,
    /// Which nodes' notes go to which, and whether to its side input for
    /// notes, from [`send_notes`](Graph::send_notes) and
    /// [`send_side_notes`](Graph::send_side_notes).
    note_routes: Vec<(NodeId, NodeId, bool)>,
    /// The same for the notes nodes make as they render, from
    /// [`send_made_notes`](Graph::send_made_notes) and
    /// [`send_side_made_notes`](Graph::send_side_made_notes).
    made_routes: Vec<(NodeId, NodeId, bool)>,
    /// The nodes [`plays_notes`](Graph::plays_notes) has play notes,
    /// wherever they go.
    playing: Vec<NodeId>,
    /// The automation nodes handed back, to send them as events.
    feeds: Vec<(NodeId, ParamId, Feed)>,
    /// The nodes whose [`Node::control`] runs each block, from
    /// [`control`](Graph::control) and [`modulate`](Graph::modulate).
    controllers: Vec<NodeId>,
    /// Which nodes modulate which nodes' parameters, from
    /// [`modulate`](Graph::modulate), in the order they were added.
    modulations: Vec<(NodeId, NodeId, ParamId)>,
    /// The nodes [`set_live`](Graph::set_live) has render live.
    marks: Vec<NodeId>,
    /// The nodes [`render_together`](Graph::render_together) keeps on one
    /// side, in pairs.
    together: Vec<(NodeId, NodeId)>,
    // What `prepare` works out for each node, which the halves of a graph
    // carry over from the whole graph.
    /// Whose each node is: its key's, a delay's, or no one's, for a queue's.
    ids: Vec<Option<LoadKey>>,
    /// Where each node's notes, modulation and made notes come in the
    /// whole graph.
    ranks: Vec<Rank>,
    /// Where each node's notes and made notes go in the whole graph, by
    /// key, and whether to the side input for notes.
    targets_of: Vec<Vec<(NodeKey, bool)>>,
    made_of: Vec<Vec<(NodeKey, bool)>>,
    /// The room a modulator's queue in the live half has, as much as the
    /// modulator had: see [`queue`](crate::queue).
    rooms: Vec<(NodeId, usize)>,
    /// The queues into the live half, for the engine to check they have
    /// room before it renders ahead.
    queue_rooms: Vec<queue::Room>,
}

/// Where a node's notes, modulation and made notes come among the events a
/// node gets on one frame, from 1, in the whole graph: 0 if it plays,
/// modulates or makes none. Events sent before any node plays, such as
/// the engine's, come first.
#[derive(Debug, Clone, Copy, Default)]
struct Rank {
    player: u32,
    controller: u32,
    maker: u32,
}

/// How many notes a queue of notes into the live half holds: as many as
/// four blocks can send.
const NOTE_QUEUE: usize = 4 * Notes::CAPACITY;

impl Graph {
    /// The most frames late a path can be, to compensate for: 262,144, about
    /// 5.5 seconds at 48 kHz. Paths later than that are lined up as if they
    /// were this late, so a node reporting a wild latency can't have the
    /// graph allocate more delay than that.
    pub const MAX_LATENCY: usize = 1 << 18;

    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node. Keys must be unique, which [`prepare`](Graph::prepare)
    /// checks.
    pub fn add(&mut self, key: NodeKey, node: impl Node) -> NodeId {
        self.add_boxed(key, Box::new(node))
    }

    /// [`add`](Graph::add), for a node whose type is chosen at run time.
    pub fn add_boxed(&mut self, key: NodeKey, node: Box<dyn Node>) -> NodeId {
        self.nodes.push(node);
        self.keys.push(key);
        self.inputs.push(Vec::new());
        self.ports.push(Vec::new());
        self.outputs.push(Vec::new());
        NodeId(self.nodes.len() - 1)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node with `key`, if there is one.
    pub fn find(&self, key: NodeKey) -> Option<NodeId> {
        self.keys.iter().position(|&k| k == key).map(NodeId)
    }

    /// Has `node` follow `automation` for its parameter `param`
    /// ([`Node::automate`]). A node that hands the automation back gets its
    /// values as parameter events, on top of the events the engine sends
    /// it: the automation's come after those on the same frame, so they
    /// win.
    pub fn automate(
        &mut self,
        node: NodeId,
        param: ParamId,
        automation: Automation,
    ) -> Result<(), GraphError> {
        self.check(node)?;
        if let Some(automation) = self.nodes[node.0].automate(param, automation) {
            self.feeds.push((node, param, Feed::new(automation)));
        }
        Ok(())
    }

    /// Feeds `from`'s output into `to`'s main input.
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.connect_to(from, to, InputPort::Main)
    }

    /// Feeds `from`'s output into `to`'s input `port`, such as a
    /// compressor's sidechain. Delay compensation lines up what comes into
    /// every port of a node alike, so a side input's signal arrives with
    /// the main input's. A connection that would close a loop is refused,
    /// and so is one that would close a loop with notes made while
    /// rendering ([`send_made_notes`](Graph::send_made_notes)).
    pub fn connect_to(
        &mut self,
        from: NodeId,
        to: NodeId,
        port: InputPort,
    ) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.reaches(to, from) || self.made_way(to, from) {
            return Err(GraphError::Cycle);
        }
        self.inputs[to.0].push(from);
        self.ports[to.0].push(port);
        self.outputs[from.0].push(to);
        Ok(())
    }

    /// Sends the notes `from` plays to `to`, such as a MIDI clip player's to
    /// an instrument. Notes don't need an audio connection, and can go
    /// against the flow of audio: before any node renders a block, the
    /// engine has each node whose notes go somewhere send the block's notes
    /// ([`Node::play_notes`]), after the nodes whose notes it gets. So notes
    /// can pass through a chain of nodes, such as a MIDI effect between a
    /// clip player and an instrument, but not around a loop: a route that
    /// would close one is refused, and so is one that would close a loop
    /// with notes made while rendering ([`send_made_notes`](Graph::send_made_notes)).
    pub fn send_notes(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.route_notes(from, to, false)
    }

    /// Sends the notes `from` plays to `to`'s side input for notes, as
    /// [`send_notes`](Graph::send_notes) sends them to its main one. They
    /// come to it as [`SideNote`](NodeEventKind::SideNote) and
    /// [`SideController`](NodeEventKind::SideController) events, so it can
    /// tell them from the others: a MIDI track's monitor takes another
    /// track's notes as its input this way, beside its clips' notes. They
    /// go through no node on the way, so they keep their ids.
    pub fn send_side_notes(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.route_notes(from, to, true)
    }

    /// Sends the notes `from` makes as it renders a block to `to`, such as
    /// those an instrument plugin plays out as it plays, or a plugin that
    /// hears notes in what comes into it. They're there only once `from` has
    /// rendered, and `to` plays them in the same block, never one late, so
    /// the graph renders in stages: what such notes reach renders in a
    /// stage after `from`'s, once the notes `from` made
    /// ([`Node::made_notes`]) have gone where they go, as the notes of the
    /// stage's players do before it renders ([`send_notes`](Graph::send_notes)).
    /// A route that would close a loop with the audio, the notes or the
    /// modulation is refused, as is anything after that would close one
    /// through it: such a loop has no stage to start in. Tells `from` that
    /// its notes go somewhere ([`Node::makes_notes`]).
    pub fn send_made_notes(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.route_made_notes(from, to, false)
    }

    /// Sends the notes `from` makes as it renders to `to`'s side input for
    /// notes, as [`send_made_notes`](Graph::send_made_notes) sends them to
    /// its main one and [`send_side_notes`](Graph::send_side_notes) sends a
    /// player's.
    pub fn send_side_made_notes(&mut self, from: NodeId, to: NodeId) -> Result<(), GraphError> {
        self.route_made_notes(from, to, true)
    }

    fn route_made_notes(&mut self, from: NodeId, to: NodeId, side: bool) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.leads_to(to, from) {
            return Err(GraphError::Cycle);
        }
        self.nodes[from.0].makes_notes();
        if !self.made_routes.contains(&(from, to, side)) {
            self.made_routes.push((from, to, side));
        }
        Ok(())
    }

    /// Has the engine have `node` play its notes for each block
    /// ([`Node::play_notes`]) whether they go anywhere or not, as a MIDI
    /// effect at the end of a track's chain does, so it plays the same
    /// whatever takes them.
    pub fn plays_notes(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        if !self.playing.contains(&node) {
            self.playing.push(node);
        }
        Ok(())
    }

    fn route_notes(&mut self, from: NodeId, to: NodeId, side: bool) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.notes_reach(to, from) || self.made_way(to, from) {
            return Err(GraphError::Cycle);
        }
        if !self.note_routes.contains(&(from, to, side)) {
            self.note_routes.push((from, to, side));
        }
        Ok(())
    }

    /// Has the engine call `node`'s [`Node::control`] for each block, once
    /// the notes have played and before any node renders, so it can
    /// modulate other nodes' parameters ([`modulate`](Graph::modulate)).
    pub fn control(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        if !self.controllers.contains(&node) {
            self.controllers.push(node);
        }
        Ok(())
    }

    /// Has `from` modulate `to`'s parameter `param`, and returns the port
    /// `from` sends the modulation through ([`Controls::send`]): the ports
    /// of each node count from 0, in the order they're added. The engine
    /// calls `from`'s [`Node::control`] for each block, and `to` gets what
    /// it sends as [`Modulation`](NodeEventKind::Modulation) events among
    /// its other events for the block, so `to` can be any node, even one
    /// that modulates others in turn: each node's `control` runs after the
    /// nodes that modulate it. A route that would close a loop of nodes
    /// modulating each other is refused, and so is one that would close a
    /// loop with notes made while rendering
    /// ([`send_made_notes`](Graph::send_made_notes)). Tells `to` that
    /// `param` is modulated ([`Node::modulated`]).
    pub fn modulate(
        &mut self,
        from: NodeId,
        to: NodeId,
        param: ParamId,
    ) -> Result<usize, GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.modulation_reaches(to, from) || self.made_way(to, from) {
            return Err(GraphError::Cycle);
        }
        self.control(from)?;
        self.nodes[to.0].modulated(param);
        let ports = self.modulations.iter().filter(|m| m.0 == from).count();
        self.modulations.push((from, to, param));
        Ok(ports)
    }

    /// Has `node` render live, as the device asks for each block, whatever
    /// comes into it: a track set to Always Live, or a plugin that must run
    /// in real time. What it reaches renders live too, as with any live
    /// source ([`Node::live`]): see [`prepare`](Graph::prepare).
    pub fn set_live(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        if !self.marks.contains(&node) {
            self.marks.push(node);
        }
        Ok(())
    }

    /// Keeps `a` and `b` on the same side of the graph, live or ahead: if
    /// either renders live, so does the other, and what it reaches. For
    /// nodes that share state outside the graph, which they must both
    /// read at the same engine time, such as a track's monitor and the cut
    /// after its devices that follows the monitor's arming.
    pub fn render_together(&mut self, a: NodeId, b: NodeId) -> Result<(), GraphError> {
        self.check(a)?;
        self.check(b)?;
        if a != b && !self.together.contains(&(a, b)) {
            self.together.push((a, b));
        }
        Ok(())
    }

    /// Sets the node whose output the engine plays.
    pub fn set_output(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        self.output = Some(node);
        Ok(())
    }

    /// Sets the cue output: a node the engine plays alongside the output,
    /// not through it, such as the metronome. The engine adds it to the
    /// device's buffer after the output, so it's in what you hear but not
    /// in the mix.
    pub fn set_cue(&mut self, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        self.cue = Some(node);
        Ok(())
    }

    /// Taps `node`'s output, for an offline render to keep alongside the
    /// output, such as an export's stems: see
    /// [`Engine::process_with_taps`](crate::Engine::process_with_taps).
    /// Taps line up with the output the way the cue output does. Returns the
    /// tap's number: they count from 0, in the order they're added.
    pub fn add_tap(&mut self, node: NodeId) -> Result<usize, GraphError> {
        self.check(node)?;
        self.taps.push(Some(node));
        Ok(self.taps.len() - 1)
    }

    /// Taps `node`'s output, as [`add_tap`](Graph::add_tap) does, for the
    /// engine to hand to the [`TapWriter`](crate::TapWriter) with `key` as
    /// it renders, block by block: what a track records from another's tap.
    /// A tap whose key is taken already isn't added. Returns the tap's
    /// number.
    pub fn add_record(&mut self, key: NodeKey, node: NodeId) -> Result<usize, GraphError> {
        if let Some(&(_, n)) = self.records.iter().find(|(k, _)| *k == key) {
            return Ok(n);
        }
        let n = self.add_tap(node)?;
        self.records.push((key, n));
        Ok(n)
    }

    /// Records the notes `node` plays, block by block, for the engine to
    /// hand to the [`TapWriter`](crate::TapWriter) with `key`, as
    /// [`add_record`](Graph::add_record) records a node's output: what a
    /// MIDI track records from another's notes, and then the notes it makes
    /// as it renders. A node plays and makes only notes that go somewhere
    /// ([`send_notes`](Graph::send_notes),
    /// [`send_made_notes`](Graph::send_made_notes)). Each writer
    /// gets one node's notes, and writers with different keys can get the
    /// same node's: a record with a key that has one already isn't added.
    pub fn add_note_record(&mut self, key: NodeKey, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        if !self.note_records.iter().any(|&(k, _)| k == key) {
            self.note_records.push((key, node));
        }
        Ok(())
    }

    /// Calls [`Node::prepare`] on each node, compensates for their latency,
    /// orders them and allocates their buffers.
    ///
    /// **Delay compensation.** Each node's output is as late as the latest
    /// path into it, plus the node's own [latency](Node::latency). Wherever
    /// paths of different lateness meet at a node, the earlier ones go
    /// through a [`Delay`] that lines them up with the latest. The cue
    /// output gets one to line it up with the output, or the output with it,
    /// so the metronome clicks in time with what plays, and so do taps.
    /// [`PreparedGraph::latency`] says how late the output is in the end,
    /// at most [`MAX_LATENCY`](Graph::MAX_LATENCY). Notes sent with
    /// [`send_notes`](Graph::send_notes) aren't delayed, so an instrument's
    /// sound lines up as long as nothing latent comes before it.
    ///
    /// **Two halves.** A graph with a live source ([`Node::live`]), or a
    /// node set live ([`set_live`](Graph::set_live)), comes in two halves:
    /// the live half, those nodes and everything they reach through their
    /// outputs, their notes, the notes they make and their modulation, and
    /// the half that renders ahead, the rest. The engine renders the live
    /// half as the device asks, and the rest ahead of it when it can
    /// ([`EngineHandle::set_lead`](crate::EngineHandle::set_lead)). What the
    /// half rendering ahead sends the live half goes through queues, one
    /// for each node's sound, notes and modulation that cross, at its own
    /// engine time, so it comes in on the frame it would have with the
    /// graph whole, and the events a live node gets on a frame come in the
    /// same order. Delay compensation lines up the whole graph first, so the
    /// halves have the same latency. A graph without live sources is one
    /// half, which renders ahead.
    ///
    /// # Panics
    ///
    /// If the sample rate or the block size in `config` is zero.
    pub fn prepare(self, config: &EngineConfig) -> Result<PreparedGraph, GraphError> {
        self.prepare_split(config, true)
    }

    /// [`prepare`](Graph::prepare), keeping the graph whole if `split` is
    /// false: for a test's reference, which renders every node as the
    /// device asks.
    pub(crate) fn prepare_split(
        mut self,
        config: &EngineConfig,
        split: bool,
    ) -> Result<PreparedGraph, GraphError> {
        assert!(
            config.sample_rate > 0 && config.max_block > 0,
            "invalid engine config {config:?}"
        );
        let mut output = self.output.ok_or(GraphError::NoOutput)?;
        let mut by_key: Vec<NodeKey> = self.keys.clone();
        by_key.sort_unstable();
        if let Some(pair) = by_key.windows(2).find(|pair| pair[0] == pair[1]) {
            return Err(GraphError::DuplicateKey(pair[0]));
        }
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.prepare(config);
            if node.channels() == 0 {
                return Err(GraphError::NoChannels(NodeId(i)));
            }
        }
        self.sort_ports();
        let (delays, latency) = self.compensate(config, &mut output);
        self.output = Some(output);
        // Whose each node is: its key's, or a delay's, by where it is.
        self.ids = (self.keys.iter())
            .map(|&key| Some(LoadKey::Node(key)))
            .collect();
        self.ids.resize(self.nodes.len(), None);
        for (at, delay) in delays {
            let after = self.keys[self.inputs[delay.0][0].0];
            self.ids[delay.0] = Some(LoadKey::Delay { after, at });
        }
        // Where each node's notes go, by key, which the halves keep.
        let keys = |routes: &[(NodeId, NodeId, bool)], n: usize| {
            let from = routes.iter().filter(|r| r.0.0 == n);
            from.map(|r| (self.keys[r.1.0], r.2)).collect::<Vec<_>>()
        };
        let len = self.nodes.len();
        self.targets_of = (0..len).map(|n| keys(&self.note_routes, n)).collect();
        self.made_of = (0..len).map(|n| keys(&self.made_routes, n)).collect();
        self.rank()?;
        let live = self.liveness();
        if !split || !live.contains(&true) {
            let loads = Arc::new(NodeLoads::new(self.ids.clone(), len));
            return self.finish(config, latency, loads, 0);
        }
        let [ahead, live] = self.split(&live, config);
        let ids = ahead.ids.iter().chain(&live.ids).copied().collect();
        let loads = Arc::new(NodeLoads::new(ids, ahead.nodes.len()));
        let mut live_ids: Vec<LoadKey> = live.ids.iter().flatten().copied().collect();
        live_ids.sort_unstable();
        let base = ahead.nodes.len();
        let live = live.finish(config, latency, Arc::clone(&loads), base)?;
        let mut ahead = ahead.finish(config, latency, loads, 0)?;
        ahead.live_half = Some(Box::new(live));
        ahead.live_ids = live_ids;
        Ok(ahead)
    }

    /// Which nodes render live, as [`set_live`](Graph::set_live) says: each
    /// live source ([`Node::live`]) and node set live, and everything they
    /// reach through their outputs, their notes, the notes they make and
    /// their modulation, with the nodes kept on their side
    /// ([`render_together`](Graph::render_together)). A delay on a node's
    /// input renders on the node's side, after the queue from the ahead
    /// half if there's one, so it carries on from the delay it takes over
    /// from whichever side its input renders on; a delay after the output,
    /// the cue output or a tap, on its input's.
    fn liveness(&self) -> Vec<bool> {
        let mut live = vec![false; self.nodes.len()];
        let mut reached: Vec<NodeId> = (0..self.nodes.len())
            .map(NodeId)
            .filter(|n| self.nodes[n.0].live() == Some(true) || self.marks.contains(n))
            .collect();
        for n in &reached {
            live[n.0] = true;
        }
        while let Some(n) = reached.pop() {
            let audio = self.outputs[n.0].iter().copied();
            let routes = self.note_routes.iter().chain(&self.made_routes);
            let notes = routes.filter(|r| r.0 == n).map(|r| r.1);
            let modulated = (self.modulations.iter()).filter(|m| m.0 == n).map(|m| m.1);
            let together = (self.together.iter()).filter_map(|&(a, b)| match n {
                _ if a == n => Some(b),
                _ if b == n => Some(a),
                _ => None,
            });
            for next in audio.chain(notes).chain(modulated).chain(together) {
                if !std::mem::replace(&mut live[next.0], true) {
                    reached.push(next);
                }
            }
        }
        for (n, id) in self.ids.iter().enumerate() {
            if let Some(LoadKey::Delay {
                at: Joint::Input { .. },
                ..
            }) = id
            {
                live[n] = live[self.outputs[n][0].0];
            }
        }
        live
    }

    /// Numbers the nodes that play notes, modulate others and make notes in
    /// the order the whole graph has them do it: stage by stage, its
    /// players, then its controllers, then its makers. What a node gets
    /// from them on one frame comes to it in this order, whichever half
    /// sends it, so a node renders the same in a half as in the whole
    /// graph.
    fn rank(&mut self) -> Result<(), GraphError> {
        let stage_of = self.stages()?;
        let players = self.player_order(&stage_of);
        let makers = self.maker_order(&stage_of);
        let mut controllers: Vec<NodeId> = (self.order_controllers().into_iter())
            .map(|c| c.node)
            .collect();
        controllers.sort_by_key(|c| stage_of[c.0]);
        let mut ranks = vec![Rank::default(); self.nodes.len()];
        let mut next = 0;
        let stages = stage_of.iter().max().map_or(1, |&s| s + 1);
        for stage in 0..stages {
            let here = |n: &&NodeId| stage_of[n.0] == stage;
            for p in players.iter().filter(here) {
                next += 1;
                ranks[p.0].player = next;
            }
            for c in controllers.iter().filter(here) {
                next += 1;
                ranks[c.0].controller = next;
            }
            for m in makers.iter().filter(here) {
                next += 1;
                ranks[m.0].maker = next;
            }
        }
        self.ranks = ranks;
        Ok(())
    }

    /// The nodes that play notes, each after the players whose notes it
    /// gets, stage by stage. Notes don't go around loops, so one is always
    /// ready.
    fn player_order(&self, stage_of: &[u32]) -> Vec<NodeId> {
        let mut routes = self.note_routes.clone();
        routes.sort_unstable();
        let mut players: Vec<NodeId> = Vec::new();
        for &(from, ..) in &routes {
            if players.last() != Some(&from) {
                players.push(from);
            }
        }
        for &node in &self.playing {
            if !players.contains(&node) {
                players.push(node);
            }
        }
        let mut ordered = Vec::with_capacity(players.len());
        while !players.is_empty() {
            let waiting = |p: NodeId| {
                (routes.iter()).any(|&(from, to, _)| to == p && players.contains(&from))
            };
            let ready = players.iter().position(|&p| !waiting(p)).expect("no loops");
            ordered.push(players.remove(ready));
        }
        ordered.sort_by_key(|p| stage_of[p.0]);
        ordered
    }

    /// The nodes that make notes as they render, stage by stage.
    fn maker_order(&self, stage_of: &[u32]) -> Vec<NodeId> {
        let mut routes = self.made_routes.clone();
        routes.sort_unstable();
        let mut makers: Vec<NodeId> = Vec::new();
        for &(from, ..) in &routes {
            if makers.last() != Some(&from) {
                makers.push(from);
            }
        }
        makers.sort_by_key(|m| stage_of[m.0]);
        makers
    }

    /// Adds `node` to a half being split off, whose it is as `id` says,
    /// with where its notes go in the whole graph and its ranks there.
    fn push(
        &mut self,
        node: Box<dyn Node>,
        id: Option<LoadKey>,
        rank: Rank,
        targets: Vec<(NodeKey, bool)>,
        made: Vec<(NodeKey, bool)>,
    ) -> NodeId {
        self.nodes.push(node);
        self.ids.push(id);
        self.ranks.push(rank);
        self.targets_of.push(targets);
        self.made_of.push(made);
        self.inputs.push(Vec::new());
        self.ports.push(Vec::new());
        self.outputs.push(Vec::new());
        NodeId(self.nodes.len() - 1)
    }

    /// Adds a queue's node to a half being split off: no one's, so it takes
    /// over from nothing.
    fn push_queue(&mut self, node: Box<dyn Node>, rank: Rank) -> NodeId {
        self.push(node, None, rank, Vec::new(), Vec::new())
    }

    /// Feeds `from`'s output into `to`'s input `port`, in a half being
    /// split off.
    fn join(&mut self, from: NodeId, to: NodeId, port: InputPort) {
        self.inputs[to.0].push(from);
        self.ports[to.0].push(port);
        self.outputs[from.0].push(to);
    }

    /// Splits the graph, with its delays and ranks, into the half that
    /// renders ahead and the half that renders live, as `live` says, each
    /// to finish preparing, with a queue wherever something passes from
    /// the ahead half to the live one (see [`queue`](crate::queue)): one
    /// for each ahead node's sound that a live node takes, one for each
    /// ahead node's notes and made notes that a live node gets, and one
    /// for each ahead modulator's ports that go to live nodes. Nothing
    /// passes the other way, as what a live node reaches is live.
    fn split(self, live: &[bool], config: &EngineConfig) -> [Graph; 2] {
        let Graph {
            nodes,
            inputs,
            ports,
            outputs,
            output,
            cue,
            taps,
            records,
            note_records,
            note_routes,
            made_routes,
            playing,
            feeds,
            controllers,
            modulations,
            ids,
            ranks,
            targets_of,
            made_of,
            ..
        } = self;
        let len = nodes.len();
        let half = |n: NodeId| usize::from(live[n.0]);
        let mut halves = [Graph::default(), Graph::default()];
        let mut place = Vec::with_capacity(len);
        for (n, node) in nodes.into_iter().enumerate() {
            let (targets, made) = (targets_of[n].clone(), made_of[n].clone());
            let id = halves[half(NodeId(n))].push(node, ids[n], ranks[n], targets, made);
            place.push(id);
        }
        let [ahead, live_half] = &mut halves;
        // Each ahead node's sound that a live node takes, through a queue.
        let frames = crate::ahead::ring_frames(*config);
        let mut sound: Vec<Option<NodeId>> = vec![None; len];
        for n in (0..len).filter(|&n| !live[n]) {
            if !outputs[n].iter().any(|to| live[to.0]) {
                continue;
            }
            let channels = ahead.nodes[place[n].0].channels();
            let (out, into) = queue::sound(channels, frames);
            let out = ahead.push_queue(Box::new(out), Rank::default());
            ahead.join(place[n], out, InputPort::Main);
            sound[n] = Some(live_half.push_queue(Box::new(into), Rank::default()));
        }
        for to in 0..len {
            for (&from, &port) in inputs[to].iter().zip(&ports[to]) {
                match (live[from.0], live[to]) {
                    (false, false) => ahead.join(place[from.0], place[to], port),
                    (true, true) => live_half.join(place[from.0], place[to], port),
                    (false, true) => {
                        let queue = sound[from.0].expect("a queue for what goes live");
                        live_half.join(queue, place[to], port);
                    }
                    (true, false) => unreachable!("what a live node feeds is live"),
                }
            }
        }
        for (h, graph) in [&mut *ahead, &mut *live_half].into_iter().enumerate() {
            let here = |n: &NodeId| half(*n) == h;
            graph.output = output.filter(here).map(|n| place[n.0]);
            graph.cue = cue.filter(here).map(|n| place[n.0]);
            graph.taps = (taps.iter())
                .map(|tap| tap.filter(here).map(|n| place[n.0]))
                .collect();
            graph.records = records.clone();
        }
        for (key, node) in note_records {
            let graph = if live[node.0] {
                &mut *live_half
            } else {
                &mut *ahead
            };
            graph.note_records.push((key, place[node.0]));
        }
        for node in playing {
            let graph = if live[node.0] {
                &mut *live_half
            } else {
                &mut *ahead
            };
            graph.playing.push(place[node.0]);
        }
        for (node, param, feed) in feeds {
            let graph = if live[node.0] {
                &mut *live_half
            } else {
                &mut *ahead
            };
            graph.feeds.push((place[node.0], param, feed));
        }
        for node in controllers {
            let graph = if live[node.0] {
                &mut *live_half
            } else {
                &mut *ahead
            };
            graph.controllers.push(place[node.0]);
        }
        // Notes and made notes for live nodes from ahead ones, through a
        // queue for each node they come from. The node that plays them on
        // has the rank of the one they come from, as a player.
        let capacity = NOTE_QUEUE;
        let mut queues = [vec![None; len], vec![None; len]];
        for (made, routes) in [(false, note_routes), (true, made_routes)] {
            for (from, to, side) in routes {
                match (live[from.0], live[to.0]) {
                    (false, false) if made => {
                        ahead.made_routes.push((place[from.0], place[to.0], side))
                    }
                    (false, false) => ahead.note_routes.push((place[from.0], place[to.0], side)),
                    (true, true) if made => {
                        live_half
                            .made_routes
                            .push((place[from.0], place[to.0], side));
                    }
                    (true, true) => live_half
                        .note_routes
                        .push((place[from.0], place[to.0], side)),
                    (false, true) => {
                        let queue = &mut queues[usize::from(made)][from.0];
                        let into = *queue.get_or_insert_with(|| {
                            let (out, into, room) = queue::notes(capacity, Notes::CAPACITY);
                            let out = ahead.push_queue(Box::new(out), Rank::default());
                            match made {
                                true => ahead.made_routes.push((place[from.0], out, false)),
                                false => ahead.note_routes.push((place[from.0], out, false)),
                            }
                            ahead.queue_rooms.push(room);
                            let rank = ranks[from.0];
                            let player = if made { rank.maker } else { rank.player };
                            let rank = Rank {
                                player,
                                ..Rank::default()
                            };
                            live_half.push_queue(Box::new(into), rank)
                        });
                        live_half.note_routes.push((into, place[to.0], side));
                    }
                    (true, false) => unreachable!("what a live node's notes reach is live"),
                }
            }
        }
        // Modulation of live nodes from ahead ones, through a queue for
        // each modulator: its ports into the live half go to the queue's
        // parameters, numbered in turn, and the node that sends them on
        // has a port for each, the modulator's rank and as much room.
        let room = Feed::room(config.max_block);
        let blocks = frames.div_ceil(config.max_block.min(crate::AHEAD_BLOCK)) + 2;
        let mut modulators: Vec<Option<(NodeId, NodeId, u64)>> = vec![None; len];
        for &(from, to, param) in &modulations {
            match (live[from.0], live[to.0]) {
                (false, false) => ahead.modulations.push((place[from.0], place[to.0], param)),
                (true, true) => live_half
                    .modulations
                    .push((place[from.0], place[to.0], param)),
                (false, true) => {
                    let (out, into, port) = modulators[from.0].get_or_insert_with(|| {
                        let ports = modulations.iter().filter(|m| m.0 == from).count();
                        let block = ports * room;
                        let (out, into, queue) = queue::modulation(block * blocks, block);
                        let out = ahead.push_queue(Box::new(out), Rank::default());
                        ahead.queue_rooms.push(queue);
                        let rank = Rank {
                            controller: ranks[from.0].controller,
                            ..Rank::default()
                        };
                        let into = live_half.push_queue(Box::new(into), rank);
                        live_half.controllers.push(into);
                        live_half.rooms.push((into, block));
                        (out, into, 0)
                    });
                    ahead
                        .modulations
                        .push((place[from.0], *out, ParamId(*port)));
                    live_half.modulations.push((*into, place[to.0], param));
                    *port += 1;
                }
                (true, false) => unreachable!("what a live node modulates is live"),
            }
        }
        halves
    }

    /// The rest of [`prepare`](Graph::prepare), for the whole graph or for
    /// one of its halves: orders the nodes and allocates their buffers, and
    /// gathers what plays notes, makes them and modulates, each node timed
    /// in `loads` from `load_base` on.
    fn finish(
        mut self,
        config: &EngineConfig,
        latency: usize,
        loads: Arc<NodeLoads>,
        load_base: usize,
    ) -> Result<PreparedGraph, GraphError> {
        let buffers: Vec<AudioBuffer> = self
            .nodes
            .iter()
            .map(|node| AudioBuffer::new(node.channels(), config.max_block))
            .collect();
        // Each stage after the notes made in the ones before, and each
        // stage's nodes in order.
        let stage_of = self.stages()?;
        let stages = stage_of.iter().max().map_or(1, |&s| s as usize + 1);
        let mut order = self.topological_order();
        order.sort_by_key(|n| stage_of[n.0]);
        let schedule = Schedule::new(&self.inputs, &self.outputs, &order, &stage_of, stages);
        let ids = &self.ids;
        let key_of = |n: NodeId| match ids[n.0] {
            Some(LoadKey::Node(key)) => Some(key),
            _ => None,
        };
        let mut by_key: Vec<(NodeKey, NodeId)> = (0..ids.len())
            .filter_map(|n| Some((key_of(NodeId(n))?, NodeId(n))))
            .collect();
        by_key.sort_unstable();
        let mut delays: Vec<(Joint, NodeId)> = (ids.iter().enumerate())
            .filter_map(|(n, id)| match id {
                Some(LoadKey::Delay { at, .. }) => Some((*at, NodeId(n))),
                _ => None,
            })
            .collect();
        delays.sort_unstable();
        self.note_routes.sort_unstable();
        self.made_routes.sort_unstable();
        let player =
            |node: NodeId, routes: &[(NodeId, NodeId, bool)], keys: &[(NodeKey, bool)]| Player {
                node,
                key: key_of(node),
                targets: (routes.iter())
                    .filter(|r| r.0 == node)
                    .map(|r| (r.1, r.2))
                    .collect(),
                target_keys: keys.to_vec(),
                record: None,
                sounding: Sounding::new(),
                rank: 0,
            };
        // The nodes that play notes, each after the players whose notes it
        // gets, and those that make notes as they render, with where their
        // notes go, stage by stage.
        let mut ordered: Vec<Player> = (self.player_order(&stage_of).into_iter())
            .map(|node| Player {
                rank: self.ranks[node.0].player,
                ..player(node, &self.note_routes, &self.targets_of[node.0])
            })
            .collect();
        let mut makers: Vec<Player> = (self.maker_order(&stage_of).into_iter())
            .map(|node| Player {
                rank: self.ranks[node.0].maker,
                ..player(node, &self.made_routes, &self.made_of[node.0])
            })
            .collect();
        // One record for each node recorded, which each of its writers
        // reads: what it plays, then what it makes as it renders. A record
        // of a node that plays and makes no notes stays empty.
        let mut note_records: Vec<NoteRecord> = Vec::new();
        let note_keys: Vec<(NodeKey, usize)> = (self.note_records.iter())
            .map(|&(key, node)| {
                let i = (note_records.iter().position(|r| r.node == node)).unwrap_or_else(|| {
                    let i = note_records.len();
                    let lists = ordered.iter_mut().chain(makers.iter_mut());
                    for player in lists.filter(|p| p.node == node) {
                        player.record = Some(i);
                    }
                    note_records.push(NoteRecord {
                        node,
                        notes: Vec::with_capacity(Notes::CAPACITY),
                    });
                    i
                });
                (key, i)
            })
            .collect();
        let routes = &self.note_routes;
        let made = &self.made_routes;
        // What comes to a node as notes: from players and from makers.
        let notes_into = |node: NodeId| {
            let routes_in = routes.iter().filter(|r| r.1 == node).count();
            routes_in + made.iter().filter(|r| r.1 == node).count()
        };
        let room = Feed::room(config.max_block);
        let mut plays_notes = vec![false; self.nodes.len()];
        let mut inbox = 0;
        for player in &ordered {
            // What comes to it: the engine's events, the notes and its
            // automation.
            let routes_in = notes_into(player.node);
            let feeds_in = self.feeds.iter().filter(|f| f.0 == player.node).count();
            plays_notes[player.node.0] = true;
            inbox = inbox.max(EVENT_CAPACITY + routes_in * Notes::CAPACITY + feeds_in * room);
        }
        let mut controllers = self.order_controllers();
        controllers.sort_by_key(|c| stage_of[c.node.0]);
        let mut is_controller = vec![false; self.nodes.len()];
        for controller in &mut controllers {
            let node = controller.node;
            is_controller[node.0] = true;
            controller.rank = self.ranks[node.0].controller;
            controller.room = match self.rooms.iter().find(|r| r.0 == node) {
                Some(&(_, room)) => room,
                None => controller.ports.len() * room,
            };
            // What comes to it: the engine's events, the notes, its
            // automation and the modulation of the nodes that modulate it.
            let routes_in = notes_into(node);
            let feeds_in = self.feeds.iter().filter(|f| f.0 == node).count();
            let ports_in = self.modulations.iter().filter(|m| m.1 == node).count();
            let notes_in = routes_in * Notes::CAPACITY;
            inbox = inbox.max(EVENT_CAPACITY + notes_in + (feeds_in + ports_in) * room);
        }
        // Where each stage's nodes, players, makers and controllers are.
        let ranges = |count: &dyn Fn(u32) -> usize| {
            let mut start = 0;
            (0..stages as u32)
                .map(move |stage| {
                    let end = start + count(stage);
                    let range = start..end;
                    start = end;
                    range
                })
                .collect::<Vec<_>>()
        };
        let nodes_in = ranges(&|s| stage_of.iter().filter(|&&n| n == s).count());
        let players_in = ranges(&|s| ordered.iter().filter(|p| stage_of[p.node.0] == s).count());
        let makers_in = ranges(&|s| makers.iter().filter(|m| stage_of[m.node.0] == s).count());
        let controllers_in = ranges(&|s| {
            (controllers.iter())
                .filter(|c| stage_of[c.node.0] == s)
                .count()
        });
        let stages: Vec<Stage> = (nodes_in.into_iter().zip(players_in))
            .zip(makers_in.into_iter().zip(controllers_in))
            .map(|((nodes, players), (makers, controllers))| Stage {
                nodes,
                players,
                makers,
                controllers,
            })
            .collect();
        // Room for every parameter event the engine can hold, for each
        // player's and maker's notes to each of its targets, for the
        // automation the graph sends and for what each controller sends.
        let notes: usize = (ordered.iter().chain(&makers))
            .map(|p| p.targets.len())
            .sum();
        let feeds = self.feeds.len() * room;
        let ports: usize = controllers.iter().map(|c| c.room).sum();
        let capacity = EVENT_CAPACITY + notes * Notes::CAPACITY + feeds + ports;
        // The nodes that can be live sources, for the engine to ask before
        // each block, and their keys, for the events it sends them.
        let live: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&n| self.nodes[n].live().is_some())
            .map(NodeId)
            .collect();
        let mut live_keys: Vec<NodeKey> = live.iter().filter_map(|&id| key_of(id)).collect();
        live_keys.sort_unstable();
        Ok(PreparedGraph {
            config: *config,
            event_ranges: vec![(0, 0); self.nodes.len()],
            nodes: self.nodes,
            inputs: self.inputs,
            ports: self.ports,
            order,
            output: self.output,
            cue: self.cue,
            taps: self.taps,
            records: self.records,
            note_records,
            note_keys,
            buffers,
            by_key,
            delays,
            latency,
            events: Vec::with_capacity(capacity),
            routed: Vec::with_capacity(capacity),
            loaded: 0,
            players: ordered,
            makers,
            plays_notes,
            inbox: Vec::with_capacity(inbox),
            inbox_events: Vec::with_capacity(inbox),
            feeds: self.feeds,
            controllers,
            is_controller,
            stage_of,
            stages,
            stage: 0,
            schedule,
            loads,
            load_base,
            live,
            live_keys,
            live_half: None,
            live_ids: Vec::new(),
            queue_rooms: self.queue_rooms,
        })
    }

    /// The nodes whose [`Node::control`] runs, each after the nodes that
    /// modulate it, with where each of their ports goes. Modulation doesn't
    /// go around loops, so one is always ready.
    fn order_controllers(&self) -> Vec<Modulator> {
        let mut waiting: Vec<Modulator> = self
            .controllers
            .iter()
            .map(|&node| Modulator {
                node,
                ports: (self.modulations.iter())
                    .filter(|m| m.0 == node)
                    .map(|&(_, to, param)| (to, param))
                    .collect(),
                rank: 0,
                room: 0,
            })
            .collect();
        let mut ordered = Vec::with_capacity(waiting.len());
        while !waiting.is_empty() {
            let modulated = |c: &Modulator| {
                let from_waiting = |from: NodeId| waiting.iter().any(|w| w.node == from);
                (self.modulations.iter()).any(|&(from, to, _)| to == c.node && from_waiting(from))
            };
            let ready = waiting
                .iter()
                .position(|c| !modulated(c))
                .expect("no loops");
            ordered.push(waiting.remove(ready));
        }
        ordered
    }

    /// Puts each node's inputs in order of port, as [`Inputs`](crate::Inputs)
    /// takes them: the main input's first, then each side input's, each in
    /// the order they were connected.
    fn sort_ports(&mut self) {
        for (inputs, ports) in self.inputs.iter_mut().zip(&mut self.ports) {
            if ports.is_sorted() {
                continue;
            }
            let mut sorted: Vec<(InputPort, NodeId)> =
                ports.iter().copied().zip(inputs.iter().copied()).collect();
            sorted.sort_by_key(|&(port, _)| port);
            (*ports, *inputs) = sorted.into_iter().unzip();
        }
    }

    /// Lines up the paths through the graph, as [`prepare`](Graph::prepare)
    /// describes, once the nodes are prepared. Returns the delays it added,
    /// with where they are, and how late the output is. `output` becomes the
    /// delay after the output, if it needs one, and so do the cue output and
    /// each tap.
    fn compensate(
        &mut self,
        config: &EngineConfig,
        output: &mut NodeId,
    ) -> (Vec<(Joint, NodeId)>, usize) {
        // How late each node's output is.
        let mut late = vec![0usize; self.nodes.len()];
        let mut delays = Vec::new();
        for node in self.topological_order() {
            let inputs = &self.inputs[node.0];
            let arrival = inputs.iter().map(|i| late[i.0]).max().unwrap_or(0);
            let latency = self.nodes[node.0].latency();
            late[node.0] = arrival.saturating_add(latency).min(Self::MAX_LATENCY);
            for slot in 0..self.inputs[node.0].len() {
                let from = self.inputs[node.0][slot];
                let early = arrival - late[from.0];
                if early == 0 {
                    continue;
                }
                let nth = self.inputs[node.0][..slot]
                    .iter()
                    .filter(|&&i| i == from)
                    .count();
                let joint = Joint::Input {
                    to: self.keys[node.0],
                    from: self.keys[from.0],
                    nth: u32::try_from(nth).unwrap_or(u32::MAX),
                };
                let delay = self.add_delay(from, early, config);
                self.inputs[node.0][slot] = delay;
                let feeds = &mut self.outputs[from.0];
                let at = feeds.iter().position(|&n| n == node);
                feeds[at.expect("`from` feeds `node`")] = delay;
                self.outputs[delay.0].push(node);
                delays.push((joint, delay));
            }
        }
        let cue_late = self.cue.map_or(0, |cue| late[cue.0]);
        let taps_late = self.taps.iter().flatten().map(|tap| late[tap.0]).max();
        let latency = late[output.0].max(cue_late).max(taps_late.unwrap_or(0));
        if let Some(cue) = self.cue {
            self.cue = Some(self.line_up(cue, &late, latency, Joint::Cue, &mut delays, config));
        }
        *output = self.line_up(*output, &late, latency, Joint::Output, &mut delays, config);
        for n in 0..self.taps.len() {
            let joint = Joint::Tap(u32::try_from(n).unwrap_or(u32::MAX));
            if let Some(tap) = self.taps[n] {
                let tap = self.line_up(tap, &late, latency, joint, &mut delays, config);
                self.taps[n] = Some(tap);
            }
        }
        (delays, latency)
    }

    /// Lines `node`'s output up with the graph's `latency`, through a delay
    /// at `joint` if it's earlier, and returns the node whose output is
    /// lined up: `node` or the delay.
    fn line_up(
        &mut self,
        node: NodeId,
        late: &[usize],
        latency: usize,
        joint: Joint,
        delays: &mut Vec<(Joint, NodeId)>,
        config: &EngineConfig,
    ) -> NodeId {
        if late[node.0] >= latency {
            return node;
        }
        let delay = self.add_delay(node, latency - late[node.0], config);
        self.outputs[node.0].push(delay);
        delays.push((joint, delay));
        delay
    }

    /// Adds a prepared [`Delay`] of `frames` that `from` feeds. It feeds
    /// nothing yet, and has no key: [`Joint`]s say where delays are.
    fn add_delay(&mut self, from: NodeId, frames: usize, config: &EngineConfig) -> NodeId {
        let mut delay = Delay::new(frames).with_channels(self.nodes[from.0].channels());
        delay.prepare(config);
        self.nodes.push(Box::new(delay));
        self.inputs.push(vec![from]);
        self.ports.push(vec![InputPort::Main]);
        self.outputs.push(Vec::new());
        NodeId(self.nodes.len() - 1)
    }

    fn check(&self, id: NodeId) -> Result<(), GraphError> {
        if id.0 < self.nodes.len() {
            Ok(())
        } else {
            Err(GraphError::UnknownNode(id))
        }
    }

    /// Each node's stage: how many hops of notes made while rendering lead
    /// to it at most, so it renders after every node whose made notes reach
    /// it, the nodes they come through, and the nodes it takes audio, notes
    /// or modulation from ([`send_made_notes`](Graph::send_made_notes)).
    /// Without made notes, every node is in stage 0. A loop that a made note
    /// goes around can't be put in stages, but the routes refuse to make
    /// one.
    fn stages(&self) -> Result<Vec<u32>, GraphError> {
        let len = self.nodes.len();
        let mut stage = vec![0u32; len];
        if self.made_routes.is_empty() {
            return Ok(stage);
        }
        // Each edge, with how many stages later its end is at least.
        let audio = (self.inputs.iter().enumerate())
            .flat_map(|(to, inputs)| inputs.iter().map(move |from| (from.0, to, 0)));
        let notes = self
            .note_routes
            .iter()
            .map(|&(from, to, _)| (from.0, to.0, 0));
        let made = self
            .made_routes
            .iter()
            .map(|&(from, to, _)| (from.0, to.0, 1));
        let modulation = self
            .modulations
            .iter()
            .map(|&(from, to, _)| (from.0, to.0, 0));
        let edges: Vec<(usize, usize, u32)> =
            audio.chain(notes).chain(made).chain(modulation).collect();
        // The longest way to each node, found by stretching each edge until
        // none stretches: without such a loop, that takes as many rounds as
        // there are nodes, at most.
        for _ in 0..=len {
            let mut stretched = false;
            for &(from, to, step) in &edges {
                if stage[from] + step > stage[to] {
                    stage[to] = stage[from] + step;
                    stretched = true;
                }
            }
            if !stretched {
                return Ok(stage);
            }
        }
        Err(GraphError::Cycle)
    }

    /// Whether anything passes from `start` to `target`: audio, notes, made
    /// notes or modulation.
    fn leads_to(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                stack.extend(&self.outputs[n.0]);
                let routes = self.note_routes.iter().chain(&self.made_routes);
                stack.extend(routes.filter(|r| r.0 == n).map(|r| r.1));
                let modulated = self.modulations.iter().filter(|m| m.0 == n);
                stack.extend(modulated.map(|m| m.1));
            }
        }
        false
    }

    /// Whether something passes from `start` to `target` along a way with
    /// notes made while rendering on it, so a route from `target` back to
    /// `start` would close a loop with no stage to start in
    /// ([`stages`](Graph::stages)).
    fn made_way(&self, start: NodeId, target: NodeId) -> bool {
        if self.made_routes.is_empty() {
            return false;
        }
        // Each node, reached so far without made notes, and with them.
        let mut seen = vec![[false; 2]; self.nodes.len()];
        let mut stack = vec![(start, false)];
        while let Some((n, made)) = stack.pop() {
            if made && n == target {
                return true;
            }
            if std::mem::replace(&mut seen[n.0][usize::from(made)], true) {
                continue;
            }
            stack.extend(self.outputs[n.0].iter().map(|&to| (to, made)));
            let notes = self.note_routes.iter().filter(|r| r.0 == n);
            stack.extend(notes.map(|r| (r.1, made)));
            let modulated = self.modulations.iter().filter(|m| m.0 == n);
            stack.extend(modulated.map(|m| (m.1, made)));
            let made_notes = self.made_routes.iter().filter(|r| r.0 == n);
            stack.extend(made_notes.map(|r| (r.1, true)));
        }
        false
    }

    /// Whether notes can pass from `start` to `target`.
    fn notes_reach(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                let next = self.note_routes.iter().filter(|r| r.0 == n);
                stack.extend(next.map(|r| r.1));
            }
        }
        false
    }

    /// Whether modulation can pass from `start` to `target`.
    fn modulation_reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                let next = self.modulations.iter().filter(|m| m.0 == n);
                stack.extend(next.map(|m| m.1));
            }
        }
        false
    }

    /// Whether signal can flow from `start` to `target`.
    fn reaches(&self, start: NodeId, target: NodeId) -> bool {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![start];
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if !std::mem::replace(&mut seen[n.0], true) {
                stack.extend(&self.outputs[n.0]);
            }
        }
        false
    }

    /// Nodes in an order where every node comes after all of its inputs.
    fn topological_order(&self) -> Vec<NodeId> {
        let mut remaining: Vec<usize> = self.inputs.iter().map(Vec::len).collect();
        let mut ready: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&n| remaining[n] == 0)
            .map(NodeId)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(node) = ready.pop() {
            order.push(node);
            for &next in &self.outputs[node.0] {
                remaining[next.0] -= 1;
                if remaining[next.0] == 0 {
                    ready.push(next);
                }
            }
        }
        debug_assert_eq!(order.len(), self.nodes.len(), "connect() rejects cycles");
        order
    }
}

/// A graph ready to run, for one [`EngineConfig`]: nodes in order and their
/// buffers allocated. Hand it to the engine with
/// [`EngineHandle::swap_graph`](crate::EngineHandle::swap_graph).
pub struct PreparedGraph {
    pub(crate) config: EngineConfig,
    pub(crate) nodes: Vec<Box<dyn Node>>,
    pub(crate) inputs: Vec<Vec<NodeId>>,
    /// The port each of a node's inputs feeds, in order of port.
    pub(crate) ports: Vec<Vec<InputPort>>,
    /// Every node comes after all of its inputs, and each stage's nodes
    /// after the stage's before it.
    pub(crate) order: Vec<NodeId>,
    /// The output node: in a graph's halves, in the half that has it.
    pub(crate) output: Option<NodeId>,
    pub(crate) cue: Option<NodeId>,
    /// The nodes whose outputs are the taps, lined up with the output, by
    /// number: in a graph's halves, those in the half.
    taps: Vec<Option<NodeId>>,
    /// The record taps, by their writers' keys, with their numbers.
    records: Vec<(NodeKey, usize)>,
    /// The notes each recorded node played in the current block.
    note_records: Vec<NoteRecord>,
    /// The keys of the writers of the notes, in the order they were added,
    /// with where their notes are in `note_records`.
    note_keys: Vec<(NodeKey, usize)>,
    /// Each node's output for the current block.
    pub(crate) buffers: Vec<AudioBuffer>,
    /// Sorted by key.
    pub(crate) by_key: Vec<(NodeKey, NodeId)>,
    /// The delays delay compensation added, sorted by where they are.
    delays: Vec<(Joint, NodeId)>,
    /// How late the output is.
    latency: usize,
    /// The current block's node events, grouped by node.
    pub(crate) events: Vec<NodeEvent>,
    /// Where each node's events are in `events`.
    pub(crate) event_ranges: Vec<(u32, u32)>,
    /// The next block's node events, as they arrive. It has room for as many
    /// as `events`.
    routed: Vec<Routed>,
    /// How many of `routed`'s events, from the first, are in `events`: the
    /// current stage's.
    loaded: usize,
    /// The nodes that play notes, each after those whose notes it gets, and
    /// where their notes go, stage by stage.
    players: Vec<Player>,
    /// The nodes that make notes as they render, and where their notes go,
    /// stage by stage.
    makers: Vec<Player>,
    /// Whether each node is a player: it gets the notes sent to it with
    /// [`Node::play_notes`] rather than [`Node::process`].
    plays_notes: Vec<bool>,
    /// A player's notes as they're gathered for it, and then in order.
    inbox: Vec<Routed>,
    inbox_events: Vec<NodeEvent>,
    /// The automation the graph sends nodes as events, by node and
    /// parameter.
    feeds: Vec<(NodeId, ParamId, Feed)>,
    /// The nodes that modulate others, each after those that modulate it,
    /// and where their ports go.
    controllers: Vec<Modulator>,
    /// Whether each node is a controller: it gets its events with
    /// [`Node::control`] rather than [`Node::process`].
    is_controller: Vec<bool>,
    /// Each node's stage ([`Graph::send_made_notes`]).
    stage_of: Vec<u32>,
    /// Where each stage's nodes are in `order`, and its players, makers and
    /// controllers in theirs: one stage for a graph without made notes.
    stages: Vec<Stage>,
    /// The stage rendering now, or about to.
    pub(crate) stage: usize,
    /// How the parallel renderer runs the graph.
    pub(crate) schedule: Schedule,
    /// How long each node takes, for both halves of a graph.
    pub(crate) loads: Arc<NodeLoads>,
    /// Where this half's loads start in `loads`.
    pub(crate) load_base: usize,
    /// The nodes that can be live sources ([`Node::live`]).
    live: Vec<NodeId>,
    /// Their keys, sorted.
    live_keys: Vec<NodeKey>,
    /// The half of the graph that renders live, if any node does: this is
    /// the half that renders ahead then. The engine takes it out as it
    /// installs the graph, for its live side to render.
    pub(crate) live_half: Option<Box<PreparedGraph>>,
    /// Whose the live half's nodes are, sorted, for the engine to tell
    /// whether a node that renders live now will render ahead in the graph
    /// that takes over.
    pub(crate) live_ids: Vec<LoadKey>,
    /// The queues of notes and modulation into the live half, for the
    /// engine to check they have room for a block it renders ahead.
    queue_rooms: Vec<queue::Room>,
}

/// Where delay compensation put a delay. A delay takes over from the delay
/// at the same joint in the graph it replaces, as nodes with the same key
/// do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Joint {
    /// On the `nth` connection from `from` into `to`, counting from 0.
    Input {
        to: NodeKey,
        from: NodeKey,
        nth: u32,
    },
    /// After the cue output.
    Cue,
    /// After the output.
    Output,
    /// After the tap with this number.
    Tap(u32),
}

/// A node event on its way into a graph, with its arrival order and its
/// sender's rank: the events a node gets on one frame come to it in order
/// of rank, then of arrival.
#[derive(Clone, Copy)]
pub(crate) struct Routed {
    pub node: NodeId,
    pub rank: u32,
    pub order: u32,
    pub event: NodeEvent,
}

/// A node that plays notes, the nodes its notes go to, and whether to their
/// side inputs for notes, and its note record, if it has one.
struct Player {
    node: NodeId,
    /// Its key: none for a queue's, which takes over from nothing.
    key: Option<NodeKey>,
    targets: Vec<(NodeId, bool)>,
    /// The keys of the nodes its notes go to in the whole graph, in either
    /// half, for the graph that takes over.
    target_keys: Vec<(NodeKey, bool)>,
    /// Where its notes come among a node's events.
    rank: u32,
    record: Option<usize>,
    sounding: Sounding,
}

/// What a node has sounding where its notes go: the notes it started and
/// hasn't ended yet, by id, as many as [`Notes::CAPACITY`], and the
/// switches it holds on there, such as the sustain pedal.
struct Sounding {
    notes: Vec<NoteId>,
    switches: Switches,
}

/// The switches held on, a bit for each, the sustain pedal's lowest: for
/// the notes on no channel of their own, then for those on each MIDI
/// channel.
type Switches = [u8; 17];

impl Sounding {
    fn new() -> Self {
        Self {
            notes: Vec::with_capacity(Notes::CAPACITY),
            switches: [0; 17],
        }
    }

    /// Keeps track of `kind`, sent where the notes go.
    fn track(&mut self, kind: NodeEventKind) {
        match kind {
            NodeEventKind::Note(NoteEvent::On { note, .. })
                if self.notes.len() < self.notes.capacity() =>
            {
                if !self.notes.contains(&note) {
                    self.notes.push(note);
                }
            }
            NodeEventKind::Note(NoteEvent::Off { note, .. }) => {
                if let Some(at) = self.notes.iter().position(|&n| n == note) {
                    self.notes.swap_remove(at);
                }
            }
            NodeEventKind::Controller {
                controller,
                value,
                channel,
            } if controller.is_switch() => {
                let held = &mut self.switches[channel_lane(channel)];
                let bit = switch_bit(controller);
                match Controller::is_on(value) {
                    true => *held |= bit,
                    false => *held &= !bit,
                }
            }
            _ => {}
        }
    }
}

/// The place of what's on `channel` among 17 places: for no channel of
/// its own, then for each MIDI channel, as among [`Switches`].
pub(crate) fn channel_lane(channel: Option<u8>) -> usize {
    channel.map_or(0, |c| usize::from(c.min(15)) + 1)
}

/// A switch's bit among [`Switches`].
fn switch_bit(controller: Controller) -> u8 {
    1 << (controller.number() - Controller::SUSTAIN.number())
}

/// The switches held on among `switches`, with their channels, in order.
fn switches_on(switches: Switches) -> impl Iterator<Item = (Controller, Option<u8>)> {
    let channels = (0..17).map(|lane| (lane as u8).checked_sub(1));
    channels.zip(switches).flat_map(|(channel, held)| {
        let switch = Controller::all().filter(|c| c.is_switch());
        switch
            .filter(move |&c| held & switch_bit(c) != 0)
            .map(move |c| (c, channel))
    })
}

/// The notes `node` played in the current block, for the writers that
/// record it: see [`Graph::add_note_record`].
struct NoteRecord {
    node: NodeId,
    notes: Vec<NodeEvent>,
}

/// Where a stage's nodes are in [`PreparedGraph::order`], and its players,
/// makers and controllers in their lists.
struct Stage {
    nodes: Range<usize>,
    players: Range<usize>,
    makers: Range<usize>,
    controllers: Range<usize>,
}

/// A node that modulates others, and the node and parameter each of its
/// ports goes to, with where its modulation comes among a node's events
/// and how much it can send in a block.
struct Modulator {
    node: NodeId,
    ports: Vec<(NodeId, ParamId)>,
    rank: u32,
    room: usize,
}

/// Where a node puts the notes it plays in a block: see
/// [`Node::play_notes`].
pub struct Notes<'a> {
    routed: &'a mut Vec<Routed>,
    targets: &'a [(NodeId, bool)],
    /// Where the node's notes come among a target's events.
    rank: u32,
    /// Where the node's notes are recorded, if they are.
    record: Option<&'a mut Vec<NodeEvent>>,
    /// What the node has sounding where its notes go, if it's kept track
    /// of.
    sounding: Option<&'a mut Sounding>,
    frames: usize,
    /// How many more notes the node can send this block.
    room: usize,
    /// Notes there was no room for.
    dropped: u64,
}

impl Notes<'_> {
    /// The most note events a node can send in one block. Two seconds of
    /// dense MPE playing, ten fingers at once, comes to a few hundred in a
    /// block of 2,048 frames.
    pub const CAPACITY: usize = 512;

    /// Sends `note` to the nodes this node's notes go to, `offset` frames
    /// into the block. Send a block's notes in order of offset. Returns
    /// whether there was room for it: notes beyond a block's
    /// [`CAPACITY`](Self::CAPACITY) are dropped, and counted in
    /// [`Stats::events_dropped`](crate::Stats::events_dropped).
    pub fn send(&mut self, offset: usize, note: NoteEvent) -> bool {
        self.send_kind(offset, NodeEventKind::Note(note))
    }

    fn send_kind(&mut self, offset: usize, kind: NodeEventKind) -> bool {
        debug_assert!(offset < self.frames, "notes are sent within the block");
        if self.room == 0 {
            self.dropped += 1;
            return false;
        }
        self.room -= 1;
        if let Some(sounding) = &mut self.sounding {
            sounding.track(kind);
        }
        for &(node, side) in self.targets {
            let order = self.routed.len() as u32;
            let kind = if side { kind.to_side() } else { kind };
            self.routed.push(Routed {
                node,
                rank: self.rank,
                order,
                event: NodeEvent { offset, kind },
            });
        }
        if let Some(record) = &mut self.record {
            record.push(NodeEvent { offset, kind });
        }
        true
    }

    /// Sends a controller's move to where this node's notes go, `offset`
    /// frames into the block, in order with its notes, for every note, on
    /// no channel of its own. It takes room as a note does, and returns
    /// whether there was some.
    pub fn send_controller(&mut self, offset: usize, controller: Controller, value: f32) -> bool {
        self.send_controller_on(offset, controller, value, None)
    }

    /// Sends a controller's move as [`send_controller`](Self::send_controller)
    /// does, for the notes on `channel`, from 0 to 15, or with `None`, on
    /// none: to pass one on as it came, or as a note wire with a channel
    /// plays it.
    pub fn send_controller_on(
        &mut self,
        offset: usize,
        controller: Controller,
        value: f32,
        channel: Option<u8>,
    ) -> bool {
        let kind = NodeEventKind::Controller {
            controller,
            value,
            channel,
        };
        self.send_kind(offset, kind)
    }

    /// Ends every note the node has sounding where its notes go, `offset`
    /// frames into the block, as if let go at no particular speed, and
    /// lets go of the switches it holds on there, such as the sustain
    /// pedal, which would hold the notes on: as when what it plays changes
    /// all at once, such as a MIDI effect's switch bypassing it. Each end
    /// takes room as a note does.
    pub fn release_all(&mut self, offset: usize) {
        let Some(sounding) = self.sounding.take() else {
            return;
        };
        while let Some(note) = sounding.notes.pop() {
            let velocity = Note::RELEASE_VELOCITY;
            self.send_kind(
                offset,
                NodeEventKind::Note(NoteEvent::Off { note, velocity }),
            );
        }
        for (controller, channel) in switches_on(std::mem::take(&mut sounding.switches)) {
            self.send_controller_on(offset, controller, 0.0, channel);
        }
        self.sounding = Some(sounding);
    }

    /// The block's length: notes are sent at offsets below it.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

impl<'a> Notes<'a> {
    /// Notes that go into `routed` alone, with room for
    /// [`CAPACITY`](Self::CAPACITY) in a block `frames` long: for a node
    /// that hands on what the node in it plays, such as a switch. Each is
    /// sent to node 0.
    pub(crate) fn buffer(routed: &'a mut Vec<Routed>, frames: usize) -> Self {
        const NOWHERE: [(NodeId, bool); 1] = [(NodeId(0), false)];
        Self {
            routed,
            targets: &NOWHERE,
            rank: 0,
            record: None,
            sounding: None,
            frames,
            room: Self::CAPACITY,
            dropped: 0,
        }
    }

    /// Sends a note or a controller's move as it was played, as
    /// [`send`](Self::send) and [`send_controller_on`](Self::send_controller_on)
    /// do: for a node that plays on what another node played.
    pub(crate) fn send_played(&mut self, offset: usize, kind: NodeEventKind) -> bool {
        debug_assert!(kind.is_played(), "notes and controllers are played");
        self.send_kind(offset, kind)
    }

    /// How many notes there was no room for.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Counts `count` notes as dropped, as a node that hands on what
    /// another plays had no room for them.
    pub(crate) fn drop_more(&mut self, count: u64) {
        self.dropped += count;
    }
}

/// Where a node puts the modulation it makes in a block: see
/// [`Node::control`].
pub struct Controls<'a> {
    routed: &'a mut Vec<Routed>,
    /// Where each port goes.
    ports: &'a [(NodeId, ParamId)],
    /// Where the node's modulation comes among a node's events.
    rank: u32,
    frames: usize,
    /// How many more events the node can send this block.
    room: usize,
    /// Events there was no room for.
    dropped: u64,
}

impl Controls<'_> {
    /// Sends `amount` of modulation through `port`, `offset` frames into
    /// the block: a [`Modulation`](NodeEventKind::Modulation) event for the
    /// parameter the port goes to. Send a block's modulation in order of
    /// offset. Returns whether there was room for it: each port has room
    /// for a value on every 32nd frame of engine time and on 16 frames
    /// more, as lanes do, and events beyond its node's share are dropped
    /// and counted in [`Stats::events_dropped`](crate::Stats::events_dropped).
    /// A port the node hasn't takes nothing.
    pub fn send(&mut self, port: usize, offset: usize, amount: f64) -> bool {
        debug_assert!(offset < self.frames, "modulation is sent within the block");
        let Some(&(node, param)) = self.ports.get(port) else {
            return false;
        };
        if self.room == 0 {
            self.dropped += 1;
            return false;
        }
        self.room -= 1;
        let order = self.routed.len() as u32;
        let kind = NodeEventKind::Modulation {
            param,
            amount,
            note: None,
        };
        let event = NodeEvent { offset, kind };
        let rank = self.rank;
        self.routed.push(Routed {
            node,
            rank,
            order,
            event,
        });
        true
    }

    /// How many ports the node has.
    pub fn ports(&self) -> usize {
        self.ports.len()
    }

    /// The block's length: modulation is sent at offsets below it.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

#[cfg(test)]
impl<'a> Controls<'a> {
    /// For a node's own tests: what goes through `ports` lands in `routed`,
    /// with room for `room` events in a block of `frames`.
    pub(crate) fn for_tests(
        routed: &'a mut Vec<Routed>,
        ports: &'a [(NodeId, ParamId)],
        frames: usize,
        room: usize,
    ) -> Self {
        Self {
            routed,
            ports,
            rank: 0,
            frames,
            room,
            dropped: 0,
        }
    }

    /// How many events there was no room for.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl PreparedGraph {
    pub fn config(&self) -> EngineConfig {
        self.config
    }

    /// How many nodes the graph has, in both halves, the queues between
    /// them included.
    pub fn len(&self) -> usize {
        self.nodes.len() + self.live_half.as_ref().map_or(0, |live| live.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a node has `key`.
    pub fn contains(&self, key: NodeKey) -> bool {
        self.find(key).is_some() || self.live_half.as_ref().is_some_and(|l| l.contains(key))
    }

    /// The output node's most recent `frames` frames.
    pub fn output(&self, frames: usize) -> AudioRef<'_> {
        (self.own_output(frames))
            .or_else(|| Some(self.live_half.as_ref()?.output(frames)))
            .unwrap_or(AudioRef::empty(frames))
    }

    /// The output node's most recent `frames` frames, if it's in this half.
    pub(crate) fn own_output(&self, frames: usize) -> Option<AudioRef<'_>> {
        self.output
            .map(|output| self.buffers[output.0].view(frames))
    }

    /// The cue node's most recent `frames` frames, if the graph has one (see
    /// [`Graph::set_cue`]).
    pub fn cue(&self, frames: usize) -> Option<AudioRef<'_>> {
        (self.own_cue(frames)).or_else(|| self.live_half.as_ref()?.cue(frames))
    }

    /// The cue node's most recent `frames` frames, if it's in this half.
    pub(crate) fn own_cue(&self, frames: usize) -> Option<AudioRef<'_>> {
        self.cue.map(|cue| self.buffers[cue.0].view(frames))
    }

    /// Tap `n`'s most recent `frames` frames, if the graph has it (see
    /// [`Graph::add_tap`]).
    pub fn tap(&self, n: usize, frames: usize) -> Option<AudioRef<'_>> {
        (self.own_tap(n, frames)).or_else(|| self.live_half.as_ref()?.tap(n, frames))
    }

    /// Tap `n`'s most recent `frames` frames, if it's in this half.
    pub(crate) fn own_tap(&self, n: usize, frames: usize) -> Option<AudioRef<'_>> {
        let tap = (*self.taps.get(n)?)?;
        Some(self.buffers[tap.0].view(frames))
    }

    /// How many taps the graph has.
    pub fn taps(&self) -> usize {
        self.taps.len()
    }

    /// The record tap for the writer with `key`'s most recent `frames`
    /// frames, if the graph has one (see [`Graph::add_record`]).
    pub fn record(&self, key: NodeKey, frames: usize) -> Option<AudioRef<'_>> {
        let &(_, n) = self.records.iter().find(|(k, _)| *k == key)?;
        self.tap(n, frames)
    }

    /// The record tap for the writer with `key`'s most recent `frames`
    /// frames, if it's in this half.
    pub(crate) fn own_record(&self, key: NodeKey, frames: usize) -> Option<AudioRef<'_>> {
        let &(_, n) = self.records.iter().find(|(k, _)| *k == key)?;
        self.own_tap(n, frames)
    }

    /// The keys of the writers the graph has record taps for, in the order
    /// they were added.
    pub fn records(&self) -> impl Iterator<Item = NodeKey> + '_ {
        self.records.iter().map(|&(key, _)| key)
    }

    /// The notes the node the writer with `key` records played in the last
    /// block, in order, each at its offset into the block, if the graph
    /// has a note record with that key (see [`Graph::add_note_record`]).
    pub fn record_notes(&self, key: NodeKey) -> Option<&[NodeEvent]> {
        (self.own_record_notes(key)).or_else(|| self.live_half.as_ref()?.record_notes(key))
    }

    /// The same, if the node recorded is in this half.
    pub(crate) fn own_record_notes(&self, key: NodeKey) -> Option<&[NodeEvent]> {
        let &(_, i) = self.note_keys.iter().find(|&&(k, _)| k == key)?;
        Some(&self.note_records[i].notes)
    }

    /// The keys of the writers the graph has note records for: those of
    /// the half that renders ahead, then those of the live half, each in
    /// the order they were added.
    pub fn note_records(&self) -> impl Iterator<Item = NodeKey> + '_ {
        let live = self.live_half.iter().flat_map(|live| live.note_keys.iter());
        (self.note_keys.iter().chain(live)).map(|&(key, _)| key)
    }

    /// How many frames late the output plays, after delay compensation: the
    /// latency of the latest path to it, or to the cue output, which every
    /// other path is lined up with ([`Graph::prepare`]). What the output
    /// plays left the clips this many frames earlier, before the audio
    /// device adds its own latency.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// How long each node takes, for the control thread to read while the
    /// graph runs: keep them, and hand the graph to the engine. Each node's
    /// load carries on from the node it takes over from as the engine
    /// installs the graph, so they're only whole once
    /// [`NodeLoads::installed`] says so.
    pub fn loads(&self) -> Arc<NodeLoads> {
        self.loads.clone()
    }

    /// Whether any node is a live source now ([`Node::live`]), in this half
    /// and in the live half, unless the engine took it out.
    pub(crate) fn is_live(&self) -> bool {
        (self.live.iter()).any(|id| self.nodes[id.0].live() == Some(true))
            || self.live_half.as_ref().is_some_and(|live| live.is_live())
    }

    /// Whether the node with `key` can be a live source, in this half and
    /// in the live half, unless the engine took it out.
    pub(crate) fn can_be_live(&self, key: NodeKey) -> bool {
        self.live_keys.binary_search(&key).is_ok()
            || self
                .live_half
                .as_ref()
                .is_some_and(|live| live.can_be_live(key))
    }

    /// Whether the node with `key` renders in the graph's live half, which
    /// the engine may have taken out.
    pub(crate) fn renders_live(&self, key: NodeKey) -> bool {
        self.live_ids.binary_search(&LoadKey::Node(key)).is_ok()
    }

    /// Whether anything that renders in this graph's live half renders
    /// ahead in `new`, which takes over from it: a track disarmed, say.
    pub(crate) fn turns_ahead(&self, new: &PreparedGraph) -> bool {
        self.live_ids.iter().any(|id| match *id {
            LoadKey::Node(key) => new.find(key).is_some(),
            LoadKey::Delay { at, .. } => new.delays.binary_search_by_key(&at, |d| d.0).is_ok(),
        })
    }

    /// Whether every queue into the live half has room for the notes and
    /// the modulation of another block.
    pub(crate) fn queues_have_room(&self) -> bool {
        self.queue_rooms.iter().all(queue::Room::has_room)
    }

    /// How many channels the output has, in this half: none if it's in the
    /// live half.
    pub(crate) fn output_channels(&self) -> usize {
        self.output
            .map_or(0, |output| self.buffers[output.0].channels())
    }

    /// How many channels the cue output has, in this half: none without
    /// one.
    pub(crate) fn cue_channels(&self) -> usize {
        self.cue.map_or(0, |cue| self.buffers[cue.0].channels())
    }

    /// The node with `key`, in this half.
    pub(crate) fn find(&self, key: NodeKey) -> Option<NodeId> {
        self.by_key
            .binary_search_by_key(&key, |&(k, _)| k)
            .ok()
            .map(|i| self.by_key[i].1)
    }

    /// Lets each node take over from the node with the same key in `old`,
    /// and each delay from the delay at the same joint, whichever half of
    /// either graph they're in. Runs on the audio thread.
    ///
    /// The notes each player had sounding go on sounding where its notes
    /// still go. Those sounding where they don't any more end on the first
    /// frame, on the node they went to, if it's still there, and the
    /// switches it held on there, such as the sustain pedal, are let go:
    /// the notes that come to it now come from elsewhere, or by another
    /// way, so the ends of those sounding would never come, and the pedal
    /// would hold on those it ends. So a MIDI effect added or taken away
    /// while notes play leaves none hanging. The switches a player holds on
    /// then go on where its notes go now, after any let go there, so a
    /// pedal a track's clips hold down holds whichever way their notes go.
    #[cfg(test)]
    pub(crate) fn adopt_from(&mut self, old: &mut PreparedGraph) {
        let mut live = self.live_half.take();
        let mut old_live = old.live_half.take();
        take_over(self, live.as_deref_mut(), old);
        if let Some(old_live) = old_live.as_deref_mut() {
            take_over(self, live.as_deref_mut(), old_live);
        }
        self.live_half = live;
        old.live_half = old_live;
    }

    /// Lets each node of this half take over from the node with the same
    /// key in `old`, one half of the graph it replaces, and each delay from
    /// the delay at the same joint, with their loads.
    fn adopt_nodes(&mut self, old: &mut PreparedGraph) {
        let (nodes, old_nodes) = (&mut self.nodes, &mut old.nodes);
        let (loads, old_loads) = (&self.loads, &old.loads);
        let (base, old_base) = (self.load_base, old.load_base);
        let mut adopt = |id: NodeId, old_id: NodeId| {
            nodes[id.0].adopt(old_nodes[old_id.0].as_mut());
            loads.carry_on(base + id.0, old_loads, old_base + old_id.0);
        };
        pairs(&self.by_key, &old.by_key, &mut adopt);
        pairs(&self.delays, &old.delays, &mut adopt);
    }

    /// The players, or with `made` the makers.
    fn list(&self, made: bool) -> &[Player] {
        match made {
            true => &self.makers,
            false => &self.players,
        }
    }

    /// Queues an event for the next block. Returns whether there was room:
    /// there always is for the events the engine holds.
    pub(crate) fn route(&mut self, node: NodeId, event: NodeEvent) -> bool {
        if self.routed.len() == self.routed.capacity() {
            return false;
        }
        let order = self.routed.len() as u32;
        self.routed.push(Routed {
            node,
            rank: 0,
            order,
            event,
        });
        true
    }

    /// Queues the automation the graph sends as events for `block`
    /// ([`Node::automate`]), after the events queued so far. Runs on the
    /// audio thread, before the block's notes play.
    pub(crate) fn feed_automation(&mut self, block: &Block<'_>) {
        let Self {
            config,
            routed,
            feeds,
            ..
        } = self;
        for (node, param, feed) in feeds.iter_mut() {
            let (node, param) = (*node, *param);
            feed.block(block, config.sample_rate, |offset, value| {
                let order = routed.len() as u32;
                let kind = NodeEventKind::Param { param, value };
                let event = NodeEvent { offset, kind };
                debug_assert!(
                    routed.len() < routed.capacity(),
                    "a graph has room for its automation"
                );
                routed.push(Routed {
                    node,
                    rank: 0,
                    order,
                    event,
                });
            });
        }
    }

    /// Has each node that plays notes in the current stage send its notes
    /// for `block`, and returns how many there was no room for. Runs on the
    /// audio thread, before the stage renders.
    pub(crate) fn play_notes(&mut self, block: &Block<'_>) -> u64 {
        let Self {
            config,
            nodes,
            routed,
            players,
            plays_notes,
            note_records,
            inbox,
            inbox_events,
            loads,
            load_base,
            stage_of,
            stages,
            stage,
            ..
        } = self;
        let is_played = |r: &Routed| r.event.kind.is_played();
        if *stage == 0 {
            for record in note_records.iter_mut() {
                record.notes.clear();
            }
        }
        let range = stages[*stage].players.clone();
        let playing = !range.is_empty();
        let mut dropped = 0;
        for player in &mut players[range] {
            // What's sent to it so far: all its notes, as the engine routed
            // its own before the block, and the players they come from have
            // played, and all its parameter changes, as its automation is
            // fed by now. Not the modulation, which comes later.
            inbox.clear();
            inbox.extend(routed.iter().filter(|r| r.node == player.node));
            inbox.sort_unstable_by_key(|r| (r.event.offset, r.rank, r.order));
            inbox_events.clear();
            inbox_events.extend(inbox.iter().map(|r| r.event));
            let ctx = block.context(config.sample_rate, inbox_events);
            // A queue's player plays on notes another node played, which
            // that node keeps sounding.
            let sounding = match player.key {
                Some(_) => Some(&mut player.sounding),
                None => None,
            };
            let mut notes = Notes {
                routed,
                targets: &player.targets,
                rank: player.rank,
                record: player.record.map(|i| &mut note_records[i].notes),
                sounding,
                frames: block.frames,
                room: Notes::CAPACITY,
                dropped: 0,
            };
            let started = load::now();
            nodes[player.node.0].play_notes(&ctx, &mut notes);
            let ticks = load::now().wrapping_sub(started);
            loads.loads[*load_base + player.node.0].add_before(ticks);
            dropped += notes.dropped;
        }
        // The stage's players have had theirs.
        if playing {
            let here = |r: &Routed| stage_of[r.node.0] as usize == *stage;
            routed.retain(|r| !(plays_notes[r.node.0] && here(r) && is_played(r)));
            renumber(routed);
        }
        dropped
    }

    /// Has each node that modulates others send its modulation for `block`
    /// ([`Node::control`]), each after the nodes that modulate it, and
    /// returns how many of their events there was no room for. Runs on the
    /// audio thread, once the notes have played and before the block
    /// renders.
    pub(crate) fn control(&mut self, block: &Block<'_>) -> u64 {
        let Self {
            config,
            nodes,
            routed,
            controllers,
            is_controller,
            inbox,
            inbox_events,
            loads,
            load_base,
            stage_of,
            stages,
            stage,
            ..
        } = self;
        let range = stages[*stage].controllers.clone();
        if range.is_empty() {
            return 0;
        }
        let mut dropped = 0;
        for controller in &controllers[range] {
            // Everything sent to it so far, which is all of it: the nodes
            // that modulate it have had their turn.
            inbox.clear();
            inbox.extend(routed.iter().filter(|r| r.node == controller.node));
            inbox.sort_unstable_by_key(|r| (r.event.offset, r.rank, r.order));
            inbox_events.clear();
            inbox_events.extend(inbox.iter().map(|r| r.event));
            let ctx = block.context(config.sample_rate, inbox_events);
            let mut controls = Controls {
                routed,
                ports: &controller.ports,
                rank: controller.rank,
                frames: block.frames,
                room: controller.room,
                dropped: 0,
            };
            let started = load::now();
            nodes[controller.node.0].control(&ctx, &mut controls);
            let ticks = load::now().wrapping_sub(started);
            loads.loads[*load_base + controller.node.0].add_before(ticks);
            dropped += controls.dropped;
        }
        // The stage's controllers have had theirs.
        let here = |r: &Routed| stage_of[r.node.0] as usize == *stage;
        routed.retain(|r| !(is_controller[r.node.0] && here(r)));
        renumber(routed);
        dropped
    }

    /// Stores the events queued for the nodes of the stage about to render.
    /// The later stages' wait in the queue, in the order they came.
    pub(crate) fn load_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        if self.stages.len() > 1 {
            let (stage_of, stage) = (&self.stage_of, self.stage as u32);
            // The stage's first, each in the order it came, as the orders
            // are each one's place in the queue.
            routed.sort_unstable_by_key(|r| (stage_of[r.node.0] != stage, r.order));
            self.loaded = routed.partition_point(|r| stage_of[r.node.0] == stage);
        } else {
            self.loaded = routed.len();
        }
        let loaded = self.loaded;
        self.load_events(&mut routed[..loaded]);
        self.routed = routed;
    }

    /// Clears what [`load_routed`](Self::load_routed) stored, once the stage
    /// has rendered, and takes it from the queue.
    pub(crate) fn clear_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        self.clear_events(&routed[..self.loaded]);
        routed.drain(..self.loaded);
        renumber(&mut routed);
        self.loaded = 0;
        self.routed = routed;
    }

    /// Has each node that makes notes as it renders in the current stage
    /// send the notes it made in `block`, which has rendered, to where they
    /// go, for the stages after it, and returns how many there was no room
    /// for. Runs on the audio thread, once the stage has rendered.
    pub(crate) fn take_made_notes(&mut self, block: &Block<'_>) -> u64 {
        let Self {
            config,
            nodes,
            routed,
            makers,
            note_records,
            loads,
            load_base,
            stages,
            stage,
            ..
        } = self;
        let mut dropped = 0;
        for maker in &mut makers[stages[*stage].makers.clone()] {
            let ctx = block.context(config.sample_rate, &[]);
            let mut notes = Notes {
                routed,
                targets: &maker.targets,
                rank: maker.rank,
                record: maker.record.map(|i| &mut note_records[i].notes),
                sounding: Some(&mut maker.sounding),
                frames: block.frames,
                room: Notes::CAPACITY,
                dropped: 0,
            };
            let started = load::now();
            nodes[maker.node.0].made_notes(&ctx, &mut notes);
            let ticks = load::now().wrapping_sub(started);
            loads.loads[*load_base + maker.node.0].add_before(ticks);
            dropped += notes.dropped;
        }
        dropped
    }

    /// Plays `block`'s notes, sends its modulation and renders it with
    /// `renderer`, a stage at a time ([`Graph::send_made_notes`]): the
    /// notes the stage's players play, then the modulation its controllers
    /// make, then its nodes, and then the notes its nodes made as they
    /// rendered, which go to the stages after it. Returns how many notes
    /// and how much modulation there was no room for. Runs on the audio
    /// thread, or on a worker rendering ahead.
    ///
    /// A graph split into halves renders the half that renders ahead and
    /// then its live half, with what passes between them through their
    /// queues ([`Graph::prepare`]), unless the engine took the live half out
    /// to render it on its live side.
    pub(crate) fn run(&mut self, renderer: &mut dyn Renderer, block: &Block<'_>) -> u64 {
        let mut dropped = self.run_half(renderer, block);
        if let Some(live) = &mut self.live_half {
            dropped += live.run(renderer, block);
        }
        dropped
    }

    /// Renders `block` in this half of the graph, as [`run`](Self::run)
    /// does.
    fn run_half(&mut self, renderer: &mut dyn Renderer, block: &Block<'_>) -> u64 {
        self.feed_automation(block);
        let mut dropped = 0;
        for stage in 0..self.stages.len() {
            self.stage = stage;
            dropped += self.play_notes(block) + self.control(block);
            self.load_routed();
            renderer.render(self, block);
            self.clear_routed();
            dropped += self.take_made_notes(block);
        }
        self.stage = 0;
        dropped
    }

    /// Where the current stage's nodes are in `order`.
    pub(crate) fn stage_nodes(&self) -> Range<usize> {
        self.stages[self.stage].nodes.clone()
    }

    /// Stores a block's node events, grouped by node, in order of offset and
    /// otherwise in arrival order. `routed` must fit in `events`.
    pub(crate) fn load_events(&mut self, routed: &mut [Routed]) {
        routed.sort_unstable_by_key(|r| (r.node, r.event.offset, r.rank, r.order));
        for r in routed.iter() {
            let index = self.events.len() as u32;
            self.events.push(r.event);
            let range = &mut self.event_ranges[r.node.0];
            if range.0 == range.1 {
                *range = (index, index + 1);
            } else {
                range.1 = index + 1;
            }
        }
    }

    pub(crate) fn clear_events(&mut self, routed: &[Routed]) {
        for r in routed {
            self.event_ranges[r.node.0] = (0, 0);
        }
        self.events.clear();
    }
}

/// Lets each node of a new graph, which renders in `ahead` and in `live`
/// if it has a live half, take over from the node with the same key in
/// `old`, one half of the graph it replaces, and each delay from the delay
/// at the same joint, and hands over what `old`'s players and makers had
/// sounding: see [`PreparedGraph::adopt_from`]. The engine calls it for
/// each half of the old graph, on the side of the engine where that half
/// renders up to the swap. Runs on the audio thread.
pub(crate) fn take_over(
    ahead: &mut PreparedGraph,
    live: Option<&mut PreparedGraph>,
    old: &mut PreparedGraph,
) {
    ahead.adopt_nodes(old);
    match live {
        Some(live) => {
            live.adopt_nodes(old);
            hand_over(&mut [ahead, live], old);
        }
        None => hand_over(&mut [ahead], old),
    }
}

/// Hands over what `old`'s players and makers had sounding to `new`, the
/// halves of the graph that takes over. Every let go comes first, before
/// the events queued for the new graph so far, so none comes after a
/// switch goes on, though the other half of the old graph may have handed
/// over its switches before, on the other side of the engine: neither half
/// of the new graph has rendered yet.
fn hand_over(new: &mut [&mut PreparedGraph], old: &mut PreparedGraph) {
    let mut queued = [0; 2];
    for (queued, graph) in queued.iter_mut().zip(new.iter()) {
        *queued = graph.routed.len();
    }
    let_go(new, &old.players, false);
    let_go(new, &old.makers, true);
    for (&queued, graph) in queued.iter().zip(new.iter_mut()) {
        let let_go = graph.routed.len() - queued;
        graph.routed.rotate_right(let_go);
        renumber(&mut graph.routed);
    }
    hold_on(new, &mut old.players, false);
    hold_on(new, &mut old.makers, true);
}

/// Ends the notes each of `gone`, an old graph's players, or with `made`
/// its makers, had sounding where its notes don't go any more in `new`,
/// the halves of the graph that takes over, and lets go of the switches it
/// held on there: see [`PreparedGraph::adopt_from`]. A queue's player plays
/// on notes another node keeps sounding, so it lets go of nothing.
fn let_go(new: &mut [&mut PreparedGraph], gone: &[Player], made: bool) {
    for gone in gone.iter().filter(|gone| gone.key.is_some()) {
        let player = find_player(new, gone.key, made);
        for &(to, side) in &gone.target_keys {
            let targets = |(half, p): (usize, usize)| &new[half].list(made)[p].target_keys;
            if player.is_some_and(|p| targets(p).contains(&(to, side))) {
                continue;
            }
            let Some((graph, to)) = (new.iter_mut()).find_map(|g| g.find(to).map(|to| (g, to)))
            else {
                continue;
            };
            let ends = gone.sounding.notes.iter().map(|&note| {
                let velocity = Note::RELEASE_VELOCITY;
                NodeEventKind::Note(NoteEvent::Off { note, velocity })
            });
            let ups = switches_on(gone.sounding.switches).map(|(controller, channel)| {
                NodeEventKind::Controller {
                    controller,
                    value: 0.0,
                    channel,
                }
            });
            for kind in ends.chain(ups) {
                let kind = if side { kind.to_side() } else { kind };
                graph.route(to, NodeEvent { offset: 0, kind });
            }
        }
    }
}

/// Has the notes each of `gone`, an old graph's players, or with `made` its
/// makers, had sounding sound on where its notes still go in `new`, the
/// halves of the graph that takes over, and the switches it held on hold
/// on where they go now: see [`PreparedGraph::adopt_from`].
fn hold_on(new: &mut [&mut PreparedGraph], gone: &mut [Player], made: bool) {
    for gone in gone.iter_mut().filter(|gone| gone.key.is_some()) {
        let Some((half, p)) = find_player(new, gone.key, made) else {
            continue;
        };
        let mut kept_any = false;
        for t in 0..new[half].list(made)[p].target_keys.len() {
            let (to, side) = new[half].list(made)[p].target_keys[t];
            if gone.target_keys.contains(&(to, side)) {
                kept_any = true;
                continue;
            }
            let Some((graph, to)) = (new.iter_mut()).find_map(|g| g.find(to).map(|to| (g, to)))
            else {
                continue;
            };
            for (controller, channel) in switches_on(gone.sounding.switches) {
                let kind = NodeEventKind::Controller {
                    controller,
                    value: 1.0,
                    channel,
                };
                let kind = if side { kind.to_side() } else { kind };
                graph.route(to, NodeEvent { offset: 0, kind });
            }
        }
        let player = match made {
            true => &mut new[half].makers[p],
            false => &mut new[half].players[p],
        };
        // Its notes sound on where it still sends them, if anywhere.
        if kept_any {
            std::mem::swap(&mut player.sounding.notes, &mut gone.sounding.notes);
        }
        player.sounding.switches = gone.sounding.switches;
    }
}

/// Which of `new`'s halves has the player, or with `made` the maker, with
/// `key`, and where it is among them.
fn find_player(
    new: &[&mut PreparedGraph],
    key: Option<NodeKey>,
    made: bool,
) -> Option<(usize, usize)> {
    (new.iter().enumerate()).find_map(|(half, graph)| {
        let p = graph.list(made).iter().position(|p| p.key == key)?;
        Some((half, p))
    })
}

/// Numbers the events in `routed` in the order they're in, after some have
/// gone, so the events sent next come after all of them.
fn renumber(routed: &mut [Routed]) {
    for (order, r) in routed.iter_mut().enumerate() {
        r.order = order as u32;
    }
}

/// Calls `f` with the node of each identity in both `new` and `old`, which
/// are sorted by identity, and the old one's.
fn pairs<T: Ord>(new: &[(T, NodeId)], old: &[(T, NodeId)], f: &mut impl FnMut(NodeId, NodeId)) {
    let (mut i, mut j) = (0, 0);
    while let (Some((key, id)), Some((old_key, old_id))) = (new.get(i), old.get(j)) {
        match key.cmp(old_key) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                f(*id, *old_id);
                i += 1;
                j += 1;
            }
        }
    }
}

impl fmt::Debug for PreparedGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedGraph")
            .field("config", &self.config)
            .field("nodes", &self.nodes.len())
            .field("output", &self.output)
            .field("cue", &self.cue)
            .field("taps", &self.taps.len())
            .field("latency", &self.latency)
            .field("live_half", &self.live_half)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::TempoMap;

    use super::*;
    use crate::nodes::{Constant, Gain, Impulse, Latent, Sum};
    use crate::{
        AudioMut, Inputs, NoteId, ParallelOptions, ParallelRenderer, ProcessContext,
        ReferenceRenderer, Renderer, TransportState, rt,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    #[test]
    fn feedback_loops_are_rejected() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), Sum::default());
        let b = g.add(NodeKey(1), Sum::default());
        let c = g.add(NodeKey(2), Sum::default());
        g.connect(a, b).unwrap();
        g.connect(b, c).unwrap();
        assert_eq!(g.connect(c, a), Err(GraphError::Cycle));
        assert_eq!(g.connect(a, a), Err(GraphError::Cycle));
        assert_eq!(
            g.connect(a, NodeId(7)),
            Err(GraphError::UnknownNode(NodeId(7)))
        );
    }

    #[test]
    fn keys_must_be_unique() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(3), Constant::new(1.0));
        g.add(NodeKey(3), Constant::new(2.0));
        g.set_output(a).unwrap();
        assert_eq!(
            g.prepare(&CONFIG).unwrap_err(),
            GraphError::DuplicateKey(NodeKey(3))
        );
    }

    #[test]
    fn a_graph_needs_an_output() {
        let mut g = Graph::new();
        g.add(NodeKey(0), Constant::new(1.0));
        assert_eq!(g.prepare(&CONFIG).unwrap_err(), GraphError::NoOutput);
    }

    #[test]
    fn nodes_come_after_their_inputs() {
        // A layered graph where every node feeds several later ones.
        let mut g = Graph::new();
        let ids: Vec<NodeId> = (0..40).map(|i| g.add(NodeKey(i), Sum::default())).collect();
        for (i, &to) in ids.iter().enumerate() {
            for &from in ids[..i].iter().rev().step_by(3).take(4) {
                g.connect(from, to).unwrap();
            }
        }
        g.set_output(ids[39]).unwrap();
        let prepared = g.prepare(&CONFIG).unwrap();
        let mut position = vec![0; prepared.len()];
        for (p, node) in prepared.order.iter().enumerate() {
            position[node.0] = p;
        }
        for (node, inputs) in prepared.inputs.iter().enumerate() {
            for input in inputs {
                assert!(position[input.0] < position[node]);
            }
        }
    }

    /// Plays a note through each block, and counts the notes sent to it.
    #[derive(Default)]
    struct Pulse(usize);

    impl Node for Pulse {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            self.0 += ctx.events.len();
            let note = NoteId(ctx.transport.position);
            notes.send(
                0,
                NoteEvent::On {
                    note,
                    key: 60,
                    velocity: 1.0,
                    channel: None,
                },
            );
            notes.send(
                ctx.frames - 1,
                NoteEvent::Off {
                    note,
                    velocity: 0.5,
                },
            );
        }
    }

    /// Plays the notes it gets an octave up, and counts the events
    /// `process` gets.
    #[derive(Default)]
    struct OctaveUp(usize);

    impl Node for OctaveUp {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.0 += ctx.events.len();
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(mut note) = event.kind {
                    if let NoteEvent::On { key, .. } = &mut note {
                        *key += 12;
                    }
                    notes.send(event.offset, note);
                }
            }
        }
    }

    /// Keeps the notes it gets, with their offsets.
    struct Keep(Vec<(usize, NoteEvent)>);

    impl Node for Keep {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(note) = event.kind {
                    self.0.push((event.offset, note));
                }
            }
            output.clear();
        }
    }

    #[test]
    fn notes_pass_through_a_chain_of_nodes() {
        let mut g = Graph::new();
        // Added in another order than the notes flow.
        let octave = g.add(NodeKey(0), OctaveUp::default());
        let pulse = g.add(NodeKey(1), Pulse::default());
        let keep = g.add(NodeKey(2), Keep(Vec::with_capacity(64)));
        g.send_notes(octave, keep).unwrap();
        g.send_notes(pulse, octave).unwrap();
        assert_eq!(g.send_notes(keep, pulse), Err(GraphError::Cycle));
        assert_eq!(g.send_notes(keep, keep), Err(GraphError::Cycle));
        g.set_output(keep).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let tempo = TempoMap::default();
        for position in [0, 64] {
            let block = Block {
                frames: 64,
                time: 0,
                transport: TransportState {
                    playing: true,
                    position,
                    ..TransportState::default()
                },
                tempo: &tempo,
                input: AudioRef::empty(64),
                midi: &[],
                ahead: false,
            };
            // Notes from the engine, as `Event::Note` sends them, come to a
            // player with the notes of the players before it.
            let from_engine = |key| NodeEvent {
                offset: 10,
                kind: NodeEventKind::Note(NoteEvent::On {
                    note: NoteId(1000 + position),
                    key,
                    velocity: 1.0,
                    channel: None,
                }),
            };
            if position == 64 {
                assert!(graph.route(octave, from_engine(50)));
                assert!(graph.route(pulse, from_engine(0)));
            }
            rt::scope(|| {
                assert_eq!(graph.play_notes(&block), 0);
                graph.load_routed();
                ReferenceRenderer.render(&mut graph, &block);
                graph.clear_routed();
            });
        }
        let kept = &graph.nodes[keep.0]
            .as_mut()
            .downcast_mut::<Keep>()
            .unwrap()
            .0;
        let on = |note, key| NoteEvent::On {
            note: NoteId(note),
            key,
            velocity: 1.0,
            channel: None,
        };
        let off = |note| NoteEvent::Off {
            note: NoteId(note),
            velocity: 0.5,
        };
        assert_eq!(
            *kept,
            [
                (0, on(0, 72)),
                (63, off(0)),
                (0, on(64, 72)),
                (10, on(1064, 62)),
                (63, off(64))
            ]
        );
        // The notes came to the players with `play_notes`, not `process`.
        let octave = graph.nodes[octave.0]
            .as_mut()
            .downcast_mut::<OctaveUp>()
            .unwrap();
        assert_eq!(octave.0, 0);
        let pulse = graph.nodes[pulse.0]
            .as_mut()
            .downcast_mut::<Pulse>()
            .unwrap();
        assert_eq!(pulse.0, 1);
    }

    /// Keeps the events it gets, with their offsets.
    struct KeepAll(Vec<(usize, NodeEventKind)>);

    impl Node for KeepAll {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.0.extend(ctx.events.iter().map(|e| (e.offset, e.kind)));
            output.clear();
        }
    }

    #[test]
    fn side_notes_come_as_side_events_and_note_records_keep_what_a_node_plays() {
        let mut g = Graph::new();
        let pulse = g.add(NodeKey(0), Pulse::default());
        let main = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
        let side = g.add(NodeKey(2), KeepAll(Vec::with_capacity(8)));
        g.send_notes(pulse, main).unwrap();
        g.send_side_notes(pulse, side).unwrap();
        assert_eq!(g.send_side_notes(side, pulse), Err(GraphError::Cycle));
        g.add_note_record(NodeKey(9), pulse).unwrap();
        // One node for each writer, and as many writers as there are keys
        // for each node.
        g.add_note_record(NodeKey(9), main).unwrap();
        // A node that plays no notes has an empty record.
        g.add_note_record(NodeKey(11), main).unwrap();
        g.add_note_record(NodeKey(10), pulse).unwrap();
        g.set_output(main).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let keys = [NodeKey(9), NodeKey(11), NodeKey(10)];
        assert!(graph.note_records().eq(keys));
        let tempo = TempoMap::default();
        let block = Block {
            frames: 64,
            time: 0,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        rt::scope(|| {
            assert_eq!(graph.play_notes(&block), 0);
            graph.load_routed();
            ReferenceRenderer.render(&mut graph, &block);
            graph.clear_routed();
        });
        let on = NoteEvent::On {
            note: NoteId(0),
            key: 60,
            velocity: 1.0,
            channel: None,
        };
        let off = NoteEvent::Off {
            note: NoteId(0),
            velocity: 0.5,
        };
        let mut kept = |node: NodeId| {
            let node = graph.nodes[node.0].as_mut();
            std::mem::take(&mut node.downcast_mut::<KeepAll>().unwrap().0)
        };
        assert_eq!(
            kept(main),
            [(0, NodeEventKind::Note(on)), (63, NodeEventKind::Note(off))]
        );
        assert_eq!(
            kept(side),
            [
                (0, NodeEventKind::SideNote(on)),
                (63, NodeEventKind::SideNote(off))
            ]
        );
        let recorded = [
            NodeEvent {
                offset: 0,
                kind: NodeEventKind::Note(on),
            },
            NodeEvent {
                offset: 63,
                kind: NodeEventKind::Note(off),
            },
        ];
        assert_eq!(graph.record_notes(NodeKey(9)), Some(&recorded[..]));
        assert_eq!(graph.record_notes(NodeKey(10)), Some(&recorded[..]));
        assert_eq!(graph.record_notes(NodeKey(11)), Some(&[][..]));
        assert_eq!(graph.record_notes(NodeKey(12)), None);
    }

    /// Keeps the events `play_notes` gets, and those `process` gets, with
    /// their offsets, and starts note 7 on the first block it plays.
    #[derive(Default)]
    struct Holds {
        played: Vec<(usize, NodeEventKind)>,
        processed: Vec<(usize, NodeEventKind)>,
        started: bool,
    }

    impl Holds {
        fn new() -> Self {
            Self {
                played: Vec::with_capacity(8),
                processed: Vec::with_capacity(8),
                started: false,
            }
        }
    }

    impl Node for Holds {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.processed
                .extend(ctx.events.iter().map(|e| (e.offset, e.kind)));
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            self.played
                .extend(ctx.events.iter().map(|e| (e.offset, e.kind)));
            if !std::mem::replace(&mut self.started, true) {
                let note = NoteId(7);
                assert!(notes.send(
                    0,
                    NoteEvent::On {
                        note,
                        key: 60,
                        velocity: 1.0,
                        channel: None,
                    }
                ));
            }
        }
    }

    /// Renders one block of 64 frames of `graph`, from engine time `time`.
    fn render_block(graph: &mut PreparedGraph, time: u64) {
        let tempo = TempoMap::default();
        let block = Block {
            frames: 64,
            time,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        rt::scope(|| {
            assert_eq!(graph.play_notes(&block), 0);
            graph.load_routed();
            ReferenceRenderer.render(graph, &block);
            graph.clear_routed();
        });
    }

    fn node<T: Node>(graph: &mut PreparedGraph, id: NodeId) -> &mut T {
        graph.nodes[id.0].as_mut().downcast_mut::<T>().unwrap()
    }

    #[test]
    fn players_get_their_parameter_changes_with_their_notes() {
        let mut g = Graph::new();
        let holds = g.add(NodeKey(0), Holds::new());
        let keep = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
        g.send_notes(holds, keep).unwrap();
        g.set_output(keep).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let param = NodeEventKind::Param {
            param: ParamId(3),
            value: 0.5,
        };
        let note = NodeEventKind::Note(NoteEvent::On {
            note: NoteId(1),
            key: 64,
            velocity: 1.0,
            channel: None,
        });
        assert!(graph.route(
            holds,
            NodeEvent {
                offset: 20,
                kind: note
            }
        ));
        assert!(graph.route(
            holds,
            NodeEvent {
                offset: 10,
                kind: param
            }
        ));
        render_block(&mut graph, 0);
        // In order of offset, and the parameter again with `process`, which
        // doesn't get the note.
        let holds = node::<Holds>(&mut graph, holds);
        assert_eq!(holds.played, [(10, param), (20, note)]);
        assert_eq!(holds.processed, [(10, param)]);
    }

    #[test]
    fn a_node_can_play_its_notes_with_nowhere_to_send_them() {
        let mut g = Graph::new();
        let holds = g.add(NodeKey(0), Holds::new());
        let out = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
        g.plays_notes(holds).unwrap();
        g.add_note_record(NodeKey(9), holds).unwrap();
        g.set_output(out).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let param = NodeEventKind::Param {
            param: ParamId(3),
            value: 0.5,
        };
        assert!(graph.route(
            holds,
            NodeEvent {
                offset: 4,
                kind: param
            }
        ));
        render_block(&mut graph, 0);
        assert_eq!(node::<Holds>(&mut graph, holds).played, [(4, param)]);
        // What it plays is kept as it would be on its way.
        let on = NoteEvent::On {
            note: NoteId(7),
            key: 60,
            velocity: 1.0,
            channel: None,
        };
        let recorded = [NodeEvent {
            offset: 0,
            kind: NodeEventKind::Note(on),
        }];
        assert_eq!(graph.record_notes(NodeKey(9)), Some(&recorded[..]));
    }

    #[test]
    fn notes_end_where_the_way_they_went_has_gone() {
        // A player holding a note, and two nodes it can send it to.
        let build = |to: &[u64]| {
            let mut g = Graph::new();
            let holds = g.add(NodeKey(0), Holds::new());
            let a = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
            let b = g.add(NodeKey(2), KeepAll(Vec::with_capacity(8)));
            for &key in to {
                let target = if key == 1 { a } else { b };
                g.send_notes(holds, target).unwrap();
            }
            g.set_output(a).unwrap();
            (g.prepare(&CONFIG).unwrap(), [a, b])
        };
        let (mut first, [a, b]) = build(&[1]);
        render_block(&mut first, 0);
        let on = NodeEventKind::Note(NoteEvent::On {
            note: NoteId(7),
            key: 60,
            velocity: 1.0,
            channel: None,
        });
        assert_eq!(node::<KeepAll>(&mut first, a).0, [(0, on)]);
        let off = NodeEventKind::Note(NoteEvent::Off {
            note: NoteId(7),
            velocity: Note::RELEASE_VELOCITY,
        });
        let swap = |old: &mut PreparedGraph, to: &[u64], time| {
            let (mut new, _) = build(to);
            rt::scope(|| new.adopt_from(old));
            node::<Holds>(&mut new, NodeId(0)).started = true;
            render_block(&mut new, time);
            let mut kept = |id| std::mem::take(&mut node::<KeepAll>(&mut new, id).0);
            let got = [kept(a), kept(b)];
            (new, got)
        };
        // The way to the first node stays, so the note sounds on there,
        // until that way goes: then it ends, on the next block's first
        // frame.
        let (mut second, got) = swap(&mut first, &[1, 2], 64);
        assert_eq!(got, [vec![], vec![]]);
        let (_, got) = swap(&mut second, &[2], 128);
        assert_eq!(got, [vec![(0, off)], vec![]]);
        // Gone all at once, the note ends there, and no more sounds where
        // the notes go from then on.
        let (mut first, _) = build(&[1]);
        render_block(&mut first, 0);
        let (mut second, got) = swap(&mut first, &[2], 64);
        assert_eq!(got, [vec![(0, off)], vec![]]);
        let (_, got) = swap(&mut second, &[], 128);
        assert_eq!(got, [vec![], vec![]]);
    }

    /// Holds the sustain pedal down, on no channel and on channel 2, and
    /// starts note 7 on the first block it plays.
    #[derive(Default)]
    struct Pedal {
        started: bool,
    }

    impl Node for Pedal {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, _: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            if !std::mem::replace(&mut self.started, true) {
                assert!(notes.send_controller(0, Controller::SUSTAIN, 1.0));
                assert!(notes.send_controller_on(0, Controller::SUSTAIN, 1.0, Some(2)));
                let note = NoteId(7);
                assert!(notes.send(
                    0,
                    NoteEvent::On {
                        note,
                        key: 60,
                        velocity: 1.0,
                        channel: None,
                    }
                ));
            }
        }
    }

    #[test]
    fn pedals_come_up_where_the_way_they_went_has_gone_and_go_down_where_it_goes_now() {
        // A player holding the pedal down and a note, and two nodes it can
        // send them to.
        let build = |to: &[u64]| {
            let mut g = Graph::new();
            let pedal = g.add(NodeKey(0), Pedal::default());
            let a = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
            let b = g.add(NodeKey(2), KeepAll(Vec::with_capacity(8)));
            for &key in to {
                let target = if key == 1 { a } else { b };
                g.send_notes(pedal, target).unwrap();
            }
            g.set_output(a).unwrap();
            (g.prepare(&CONFIG).unwrap(), [a, b])
        };
        let swap = |old: &mut PreparedGraph, to: &[u64], time| {
            let (mut new, [a, b]) = build(to);
            rt::scope(|| new.adopt_from(old));
            node::<Pedal>(&mut new, NodeId(0)).started = true;
            render_block(&mut new, time);
            let mut kept = |id| std::mem::take(&mut node::<KeepAll>(&mut new, id).0);
            let got = [kept(a), kept(b)];
            (new, got)
        };
        let (mut first, _) = build(&[1]);
        render_block(&mut first, 0);
        let pedal = |value, channel| {
            let kind = NodeEventKind::Controller {
                controller: Controller::SUSTAIN,
                value,
                channel,
            };
            (0, kind)
        };
        // Where the way goes, the note ends, and the pedal, which would
        // hold it on, comes up, on each channel. Where it goes now, the
        // pedal goes down.
        let off = NodeEventKind::Note(NoteEvent::Off {
            note: NoteId(7),
            velocity: Note::RELEASE_VELOCITY,
        });
        let (mut second, got) = swap(&mut first, &[2], 64);
        let up = [(0, off), pedal(0.0, None), pedal(0.0, Some(2))];
        let down = [pedal(1.0, None), pedal(1.0, Some(2))];
        assert_eq!(got, [up.to_vec(), down.to_vec()]);
        // Where the way stays, it holds on as it is.
        let (_, got) = swap(&mut second, &[1, 2], 128);
        assert_eq!(got, [down.to_vec(), vec![]]);
    }

    /// Hears a note in what comes into it, as a note detector does: one on
    /// key 60 from where its input rises above nothing until it falls
    /// back, under the id of the engine time it rose at. It makes them as
    /// it renders, plays silence, and counts the times it's told its notes
    /// go somewhere.
    struct Detects {
        made: Vec<(usize, NoteEvent)>,
        sounding: Option<NoteId>,
        told: usize,
    }

    impl Detects {
        fn new() -> Self {
            Self {
                made: Vec::with_capacity(8),
                sounding: None,
                told: 0,
            }
        }
    }

    impl Node for Detects {
        fn makes_notes(&mut self) {
            self.told += 1;
        }

        fn adopt(&mut self, previous: &mut dyn Node) {
            if let Some(previous) = previous.downcast_mut::<Detects>() {
                self.sounding = previous.sounding;
            }
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            let input = inputs.iter().next().expect("it hears its input");
            for (offset, &sample) in input.channel(0).iter().enumerate() {
                match (sample > 0.0, self.sounding) {
                    (true, None) => {
                        let note = NoteId(ctx.time + offset as u64);
                        self.sounding = Some(note);
                        let on = NoteEvent::On {
                            note,
                            key: 60,
                            velocity: 1.0,
                            channel: None,
                        };
                        self.made.push((offset, on));
                    }
                    (false, Some(note)) => {
                        self.sounding = None;
                        let velocity = 0.5;
                        self.made.push((offset, NoteEvent::Off { note, velocity }));
                    }
                    _ => {}
                }
            }
            output.clear();
        }

        fn made_notes(&mut self, _: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for (offset, note) in self.made.drain(..) {
                assert!(notes.send(offset, note));
            }
        }
    }

    /// Plays the notes it gets again as it renders, an octave up, as an
    /// instrument plugin that plays its notes out does.
    struct Echoes(Vec<(usize, NoteEvent)>);

    impl Node for Echoes {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Note(mut note) = event.kind {
                    if let NoteEvent::On { key, .. } = &mut note {
                        *key += 12;
                    }
                    self.0.push((event.offset, note));
                }
            }
            output.clear();
        }

        fn made_notes(&mut self, _: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for (offset, note) in self.0.drain(..) {
                assert!(notes.send(offset, note));
            }
        }
    }

    /// Renders one block of 64 frames of `graph` with `renderer`, from
    /// engine time `time`, stage by stage, as the engine does.
    fn run_block(graph: &mut PreparedGraph, renderer: &mut dyn Renderer, time: u64) {
        let tempo = TempoMap::default();
        let block = Block {
            frames: 64,
            time,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        rt::scope(|| assert_eq!(graph.run(renderer, &block), 0));
    }

    #[test]
    fn notes_made_while_rendering_play_in_the_same_block_a_stage_later() {
        // An impulse a detector hears as a note, which an echo plays again
        // an octave up as it renders, which plays on another octave up to
        // a node that keeps it.
        let build = || {
            let mut g = Graph::new();
            let keep = g.add(NodeKey(0), Keep(Vec::with_capacity(8)));
            let octave = g.add(NodeKey(1), OctaveUp::default());
            let echoes = g.add(NodeKey(2), Echoes(Vec::with_capacity(8)));
            let detects = g.add(NodeKey(3), Detects::new());
            let impulse = g.add(NodeKey(4), impulse(70));
            let out = g.add(NodeKey(5), Sum::default());
            g.connect(impulse, detects).unwrap();
            g.send_made_notes(detects, echoes).unwrap();
            g.send_made_notes(echoes, octave).unwrap();
            g.send_notes(octave, keep).unwrap();
            g.add_note_record(NodeKey(9), echoes).unwrap();
            for node in [detects, echoes, keep] {
                g.connect(node, out).unwrap();
            }
            g.set_output(out).unwrap();
            let graph = g.prepare(&CONFIG).unwrap();
            (graph, [keep, octave, echoes, detects, impulse, out])
        };
        let (graph, [keep, octave, echoes, detects, impulse, out]) = build();
        // Each stage after the notes made in the one before.
        assert_eq!(graph.stages.len(), 3);
        let stage = |n: NodeId| graph.stage_of[n.0];
        let nodes = [impulse, detects, echoes, octave, keep, out];
        assert_eq!(nodes.map(stage), [0, 0, 1, 2, 2, 2]);

        let on = |key| NoteEvent::On {
            note: NoteId(70),
            key,
            velocity: 1.0,
            channel: None,
        };
        let off = NoteEvent::Off {
            note: NoteId(70),
            velocity: 0.5,
        };
        let parallel = ParallelRenderer::new(ParallelOptions {
            threads: 2,
            spin: std::time::Duration::from_micros(20),
            inline_below: None,
            max_nodes: 64,
            realtime: false,
        });
        let renderers: [Box<dyn Renderer>; 2] = [Box::new(ReferenceRenderer), Box::new(parallel)];
        for mut renderer in renderers {
            let (mut graph, _) = build();
            assert_eq!(node::<Detects>(&mut graph, detects).told, 1);
            let mut kept = Vec::new();
            let mut recorded = Vec::new();
            for time in [0, 64, 128] {
                run_block(&mut graph, &mut *renderer, time);
                // Leaving the node its room, so it never allocates.
                kept.push(node::<Keep>(&mut graph, keep).0.split_off(0));
                let record = graph.record_notes(NodeKey(9)).unwrap();
                recorded.push(
                    record
                        .iter()
                        .map(|e| (e.offset, e.kind))
                        .collect::<Vec<_>>(),
                );
            }
            // In the block the impulse comes in, on its frame, and the next.
            assert_eq!(kept, [vec![], vec![(6, on(84)), (7, off)], vec![]]);
            // The record keeps what the echo made.
            let note = |n| NodeEventKind::Note(n);
            let made = vec![(6, note(on(72))), (7, note(off))];
            assert_eq!(recorded, [vec![], made, vec![]]);
        }
    }

    #[test]
    fn routes_that_would_close_a_loop_through_made_notes_are_refused() {
        let mut g = Graph::new();
        let [a, b, c, d] = [0, 1, 2, 3].map(|k| g.add(NodeKey(k), Keep(Vec::new())));
        g.send_made_notes(a, b).unwrap();
        g.connect(b, c).unwrap();
        // Nothing comes back to `a` from where its made notes go, whatever
        // way: `a` would have to render after them.
        assert_eq!(g.send_made_notes(a, a), Err(GraphError::Cycle));
        assert_eq!(g.send_made_notes(c, a), Err(GraphError::Cycle));
        assert_eq!(g.connect(c, a), Err(GraphError::Cycle));
        assert_eq!(g.send_notes(b, a), Err(GraphError::Cycle));
        assert_eq!(g.send_side_notes(c, a), Err(GraphError::Cycle));
        assert_eq!(g.modulate(c, a, ParamId(0)), Err(GraphError::Cycle));
        // Nor round through another node.
        g.connect(d, a).unwrap();
        assert_eq!(g.send_made_notes(c, d), Err(GraphError::Cycle));
        assert_eq!(g.send_notes(c, d), Err(GraphError::Cycle));
        // The other way is no loop.
        g.send_made_notes(d, c).unwrap();
        g.set_output(c).unwrap();
        let graph = g.prepare(&CONFIG).unwrap();
        assert_eq!([a, b, c, d].map(|n| graph.stage_of[n.0]), [0, 1, 1, 0]);

        // Without made notes, notes can go against the audio, as they play
        // before anything renders.
        let mut g = Graph::new();
        let [a, b] = [0, 1].map(|k| g.add(NodeKey(k), Keep(Vec::new())));
        g.connect(a, b).unwrap();
        g.send_notes(b, a).unwrap();
        g.set_output(b).unwrap();
        assert_eq!(g.prepare(&CONFIG).unwrap().stages.len(), 1);
    }

    #[test]
    fn notes_made_while_rendering_end_where_the_way_they_went_has_gone() {
        // A detector hearing a note held from the start, and two nodes its
        // notes can go to.
        let build = |to: &[u64]| {
            let mut g = Graph::new();
            let held = g.add(NodeKey(0), Constant::new(1.0));
            let detects = g.add(NodeKey(1), Detects::new());
            let a = g.add(NodeKey(2), KeepAll(Vec::with_capacity(8)));
            let b = g.add(NodeKey(3), KeepAll(Vec::with_capacity(8)));
            g.connect(held, detects).unwrap();
            for &key in to {
                let target = if key == 2 { a } else { b };
                g.send_made_notes(detects, target).unwrap();
            }
            g.set_output(a).unwrap();
            (g.prepare(&CONFIG).unwrap(), [a, b])
        };
        let on = NodeEventKind::Note(NoteEvent::On {
            note: NoteId(0),
            key: 60,
            velocity: 1.0,
            channel: None,
        });
        let off = NodeEventKind::Note(NoteEvent::Off {
            note: NoteId(0),
            velocity: Note::RELEASE_VELOCITY,
        });
        let (mut first, [a, b]) = build(&[2]);
        run_block(&mut first, &mut ReferenceRenderer, 0);
        assert_eq!(node::<KeepAll>(&mut first, a).0, [(0, on)]);
        let swap = |old: &mut PreparedGraph, to: &[u64], time| {
            let (mut new, _) = build(to);
            rt::scope(|| new.adopt_from(old));
            run_block(&mut new, &mut ReferenceRenderer, time);
            let mut kept = |id| std::mem::take(&mut node::<KeepAll>(&mut new, id).0);
            let got = [kept(a), kept(b)];
            (new, got)
        };
        // The way to the first node stays, so the note sounds on there,
        // until that way goes: then it ends, on the next block's first
        // frame. The second never got it.
        let (mut second, got) = swap(&mut first, &[2, 3], 64);
        assert_eq!(got, [vec![], vec![]]);
        let (_, got) = swap(&mut second, &[3], 128);
        assert_eq!(got, [vec![(0, off)], vec![]]);
    }

    /// Sends the modulation it gets plus one, or one if it gets none,
    /// through its port on each block's first frame, and counts the events
    /// `process` gets.
    #[derive(Default)]
    struct AddOne(usize);

    impl Node for AddOne {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            self.0 += ctx.events.len();
            output.clear();
        }

        fn control(&mut self, ctx: &ProcessContext<'_>, controls: &mut Controls<'_>) {
            let got = ctx.events.iter().find_map(|e| match e.kind {
                NodeEventKind::Modulation { amount, .. } => Some(amount),
                _ => None,
            });
            assert!(controls.send(0, 0, got.unwrap_or(0.0) + 1.0));
        }
    }

    /// Keeps the modulation it gets, and which parameters the graph says
    /// are modulated.
    struct Modulated {
        got: Vec<(usize, ParamId, f64)>,
        modulated: Vec<ParamId>,
    }

    impl Node for Modulated {
        fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            for event in ctx.events {
                if let NodeEventKind::Modulation { param, amount, .. } = event.kind {
                    self.got.push((event.offset, param, amount));
                }
            }
            output.clear();
        }

        fn modulated(&mut self, param: ParamId) {
            self.modulated.push(param);
        }
    }

    #[test]
    fn modulation_flows_through_chains_of_nodes_but_not_around_loops() {
        let mut g = Graph::new();
        // Added in another order than the modulation flows.
        let target = Modulated {
            got: Vec::with_capacity(8),
            modulated: Vec::new(),
        };
        let target = g.add(NodeKey(0), target);
        let second = g.add(NodeKey(1), AddOne::default());
        let first = g.add(NodeKey(2), AddOne::default());
        assert_eq!(g.modulate(second, target, ParamId(7)), Ok(0));
        assert_eq!(g.modulate(first, second, ParamId(3)), Ok(0));
        assert_eq!(
            g.modulate(target, first, ParamId(0)),
            Err(GraphError::Cycle)
        );
        assert_eq!(g.modulate(first, first, ParamId(0)), Err(GraphError::Cycle));
        g.set_output(target).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        let tempo = TempoMap::default();
        let block = Block {
            frames: 64,
            time: 0,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
            ahead: false,
        };
        rt::scope(|| {
            assert_eq!(graph.control(&block), 0);
            graph.load_routed();
            ReferenceRenderer.render(&mut graph, &block);
            graph.clear_routed();
        });
        let target = graph.nodes[target.0]
            .as_mut()
            .downcast_mut::<Modulated>()
            .unwrap();
        // One from the first, and one more from the second, which runs
        // after it.
        assert_eq!(target.got, [(0, ParamId(7), 2.0)]);
        assert_eq!(target.modulated, [ParamId(7)]);
        // The modulation came to the second with `control`, not `process`.
        let second = graph.nodes[second.0]
            .as_mut()
            .downcast_mut::<AddOne>()
            .unwrap();
        assert_eq!(second.0, 0);
    }

    fn impulse(at: u64) -> Impulse {
        Impulse { at }
    }

    fn latent(frames: usize) -> Latent {
        Latent(Delay::new(frames))
    }

    /// Renders `frames` frames of `graph` from engine time `from`, in blocks
    /// of `block`, and returns the first channel of the output and of the
    /// cue output.
    fn play(
        graph: &mut PreparedGraph,
        from: u64,
        frames: usize,
        block: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let tempo = TempoMap::default();
        let (mut output, mut cue) = (Vec::new(), Vec::new());
        let mut done = 0;
        while done < frames {
            let n = block.min(frames - done);
            let block = Block {
                frames: n,
                time: from + done as u64,
                transport: TransportState::default(),
                tempo: &tempo,
                input: AudioRef::empty(n),
                midi: &[],
                ahead: false,
            };
            rt::scope(|| ReferenceRenderer.render(graph, &block));
            output.extend_from_slice(graph.output(n).channel(0));
            if let Some(c) = graph.cue(n) {
                cue.extend_from_slice(c.channel(0));
            }
            done += n;
        }
        (output, cue)
    }

    /// The frames that aren't silent, and what's on them.
    fn peaks(signal: &[f32]) -> Vec<(usize, f32)> {
        let loud = signal.iter().enumerate().filter(|&(_, &s)| s != 0.0);
        loud.map(|(i, &s)| (i, s)).collect()
    }

    #[test]
    fn a_latent_path_and_a_dry_one_line_up_where_they_meet() {
        let mut g = Graph::new();
        let source = g.add(NodeKey(0), impulse(50));
        let wet = g.add(NodeKey(1), latent(37));
        let sum = g.add(NodeKey(2), Sum::default());
        g.connect(source, wet).unwrap();
        g.connect(wet, sum).unwrap();
        g.connect(source, sum).unwrap();
        g.set_output(sum).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        assert_eq!(graph.latency(), 37);
        assert_eq!(graph.len(), 4, "one delay, on the dry path");
        let (output, _) = play(&mut graph, 0, 200, 16);
        assert_eq!(peaks(&output), [(87, 2.0)]);
    }

    #[test]
    fn paths_line_up_at_every_node_they_meet() {
        let mut g = Graph::new();
        let [source, five, eleven, bus, mix, out] = [
            g.add(NodeKey(0), impulse(10)),
            g.add(NodeKey(1), latent(5)),
            g.add(NodeKey(2), latent(11)),
            g.add(NodeKey(3), Sum::default()),
            g.add(NodeKey(4), Sum::default()),
            g.add(NodeKey(5), Sum::default()),
        ];
        for (from, to) in [
            (source, five),
            (five, bus),
            (source, bus),
            (source, eleven),
            (eleven, mix),
            // Twice, so it counts twice.
            (bus, mix),
            (bus, mix),
            (mix, out),
            (source, out),
        ] {
            g.connect(from, to).unwrap();
        }
        g.set_output(out).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        assert_eq!(graph.latency(), 11);
        // Delays of 5 from the source into the bus, 6 on both connections
        // from the bus into the mix, and 11 from the source into the output.
        assert_eq!(graph.len(), 10);
        let (output, _) = play(&mut graph, 0, 64, 7);
        // The bus has 2 of the impulse, the mix 1 + 2 + 2, the output one more.
        assert_eq!(peaks(&output), [(21, 6.0)]);
    }

    #[test]
    fn latency_is_compensated_up_to_a_limit() {
        let mut g = Graph::new();
        let source = g.add(NodeKey(0), impulse(0));
        let wild = g.add(NodeKey(1), Wild);
        let sum = g.add(NodeKey(2), Sum::default());
        g.connect(source, wild).unwrap();
        g.connect(wild, sum).unwrap();
        g.connect(source, sum).unwrap();
        g.set_output(sum).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        assert_eq!(graph.latency(), Graph::MAX_LATENCY);
        let (output, _) = play(&mut graph, 0, 64, 64);
        assert_eq!(
            peaks(&output),
            [(0, 1.0)],
            "the wild path, which isn't late"
        );
    }

    /// Passes what comes in straight through, and reports far more latency
    /// than anything has.
    struct Wild;

    impl Node for Wild {
        fn latency(&self) -> usize {
            usize::MAX
        }

        fn process(
            &mut self,
            _: &ProcessContext<'_>,
            inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
            for input in inputs.iter() {
                output.add_from(input);
            }
        }
    }

    #[test]
    fn graphs_without_latency_have_no_delays() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), impulse(3));
        let b = g.add(NodeKey(1), Gain::new(0.5));
        let sum = g.add(NodeKey(2), Sum::default());
        g.connect(a, b).unwrap();
        g.connect(a, sum).unwrap();
        g.connect(b, sum).unwrap();
        g.set_output(sum).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        assert_eq!((graph.len(), graph.latency()), (3, 0));
        assert_eq!(peaks(&play(&mut graph, 0, 16, 16).0), [(3, 1.5)]);
    }

    #[test]
    fn the_cue_output_lines_up_with_the_output() {
        for (output_latency, cue_latency) in [(20, 0), (0, 20), (8, 3)] {
            let mut g = Graph::new();
            let source = g.add(NodeKey(0), impulse(5));
            let output = g.add(NodeKey(1), latent(output_latency));
            let click = g.add(NodeKey(2), impulse(5));
            let cue = g.add(NodeKey(3), latent(cue_latency));
            g.connect(source, output).unwrap();
            g.connect(click, cue).unwrap();
            g.set_output(output).unwrap();
            g.set_cue(cue).unwrap();
            let mut graph = g.prepare(&CONFIG).unwrap();
            let latency = output_latency.max(cue_latency);
            assert_eq!(graph.latency(), latency);
            let (output, cue) = play(&mut graph, 0, 64, 16);
            assert_eq!(peaks(&output), [(5 + latency, 1.0)], "{latency}");
            assert_eq!(peaks(&cue), [(5 + latency, 1.0)], "{latency}");
        }
    }

    #[test]
    fn taps_line_up_with_the_output() {
        // A dry source tapped before a latent path to the output, and a
        // latent node off to the side that feeds nothing but its tap, which
        // makes it the latest of all.
        for side_latency in [0, 30] {
            let mut g = Graph::new();
            let source = g.add(NodeKey(0), impulse(5));
            let wet = g.add(NodeKey(1), latent(20));
            let side = g.add(NodeKey(2), latent(side_latency));
            g.connect(source, wet).unwrap();
            g.connect(source, side).unwrap();
            g.set_output(wet).unwrap();
            assert_eq!(g.add_tap(source), Ok(0));
            assert_eq!(g.add_tap(side), Ok(1));
            assert_eq!(
                g.add_tap(NodeId(9)),
                Err(GraphError::UnknownNode(NodeId(9)))
            );
            let mut graph = g.prepare(&CONFIG).unwrap();
            let latency = 20.max(side_latency);
            assert_eq!((graph.latency(), graph.taps()), (latency, 2));
            let tempo = TempoMap::default();
            let mut heard: [Vec<f32>; 3] = Default::default();
            for from in (0..64).step_by(16) {
                let block = Block {
                    frames: 16,
                    time: from,
                    transport: TransportState::default(),
                    tempo: &tempo,
                    input: AudioRef::empty(16),
                    midi: &[],
                    ahead: false,
                };
                rt::scope(|| ReferenceRenderer.render(&mut graph, &block));
                heard[0].extend_from_slice(graph.output(16).channel(0));
                for n in 0..2 {
                    heard[n + 1].extend_from_slice(graph.tap(n, 16).unwrap().channel(0));
                }
            }
            assert!(graph.tap(2, 16).is_none());
            for signal in &heard {
                assert_eq!(peaks(signal), [(5 + latency, 1.0)], "{side_latency}");
            }
        }
    }

    /// A latent path and a dry one at half the level, into a sum. With
    /// `more`, another path into the sum comes first, so the dry path is the
    /// sum's third input instead of its second.
    fn wet_and_dry(latency: usize, more: bool) -> PreparedGraph {
        let mut g = Graph::new();
        let sum = g.add(NodeKey(0), Sum::default());
        if more {
            let silent = g.add(NodeKey(4), impulse(u64::MAX));
            g.connect(silent, sum).unwrap();
        }
        let source = g.add(NodeKey(1), impulse(30));
        let wet = g.add(NodeKey(2), latent(latency));
        let dry = g.add(NodeKey(3), Gain::new(0.5));
        g.connect(source, wet).unwrap();
        g.connect(wet, sum).unwrap();
        g.connect(source, dry).unwrap();
        g.connect(dry, sum).unwrap();
        g.set_output(sum).unwrap();
        g.prepare(&CONFIG).unwrap()
    }

    #[test]
    fn delays_carry_on_across_swaps() {
        // The impulse at frame 30 is on its way through both paths when the
        // graph is swapped at frame 40. Each delay takes over from the one
        // in the same place, whichever input of the sum it's on, so the
        // impulse comes out once: on time when the latency stays the same,
        // and otherwise as late as the new latency says.
        for (latency, at) in [(40, 70), (25, 55), (60, 90)] {
            let mut first = wet_and_dry(40, false);
            let (mut output, _) = play(&mut first, 0, 40, 8);
            let mut second = wet_and_dry(latency, true);
            rt::scope(|| second.adopt_from(&mut first));
            output.extend(play(&mut second, 40, 60, 8).0);
            assert_eq!(peaks(&output), [(at, 1.5)], "{latency}");
        }
    }

    #[test]
    fn nodes_are_found_by_key() {
        let mut g = Graph::new();
        g.add(NodeKey(30), Constant::new(1.0));
        let b = g.add(NodeKey(10), Constant::new(1.0));
        g.add(NodeKey(20), Constant::new(1.0));
        g.set_output(b).unwrap();
        let prepared = g.prepare(&CONFIG).unwrap();
        assert_eq!(prepared.find(NodeKey(10)), Some(b));
        assert_eq!(prepared.find(NodeKey(20)), Some(NodeId(2)));
        assert!(!prepared.contains(NodeKey(15)));
    }

    /// A track monitoring the device inputs, always: a live source.
    fn monitoring() -> crate::nodes::InputMonitor {
        crate::nodes::InputMonitor::new(musicable_model::Monitor::In, false).with_input(0, 1)
    }

    /// The keys of the nodes in `graph`'s half that renders ahead, and in
    /// its live half.
    fn sides(graph: &PreparedGraph) -> [Vec<u64>; 2] {
        let keys = |graph: &PreparedGraph| graph.by_key.iter().map(|&(key, _)| key.0).collect();
        let live = graph.live_half.as_deref().map_or_else(Vec::new, keys);
        [keys(graph), live]
    }

    #[test]
    fn what_a_live_source_reaches_renders_live_and_the_rest_ahead() {
        let mut g = Graph::new();
        let clips = g.add(NodeKey(0), Constant::new(0.5));
        let input = g.add(NodeKey(1), monitoring());
        let track = g.add(NodeKey(2), Gain::new(1.0));
        let other = g.add(NodeKey(3), Constant::new(0.25));
        let master = g.add(NodeKey(4), Sum::default());
        g.connect(clips, input).unwrap();
        g.connect(input, track).unwrap();
        g.connect(track, master).unwrap();
        g.connect(other, master).unwrap();
        g.set_output(master).unwrap();
        let graph = g.prepare(&CONFIG).unwrap();
        assert_eq!(sides(&graph), [vec![0, 3], vec![1, 2, 4]]);
        assert!(graph.renders_live(NodeKey(2)) && !graph.renders_live(NodeKey(3)));
        // The output is the live half's.
        assert_eq!(graph.output_channels(), 0);

        // Nothing live, nothing to split.
        let mut g = Graph::new();
        let clips = g.add(NodeKey(0), Constant::new(0.5));
        g.set_output(clips).unwrap();
        let graph = g.prepare(&CONFIG).unwrap();
        assert!(graph.live_half.is_none());
    }

    #[test]
    fn nodes_always_live_and_nodes_kept_together_render_live_with_what_they_reach() {
        let mut g = Graph::new();
        let a = g.add(NodeKey(0), Constant::new(0.5));
        let marked = g.add(NodeKey(1), Gain::new(1.0));
        let after = g.add(NodeKey(2), Gain::new(1.0));
        let partner = g.add(NodeKey(3), Constant::new(0.25));
        let reached = g.add(NodeKey(4), Gain::new(1.0));
        let master = g.add(NodeKey(5), Sum::default());
        g.connect(a, marked).unwrap();
        g.connect(marked, after).unwrap();
        g.connect(partner, reached).unwrap();
        g.connect(after, master).unwrap();
        g.connect(reached, master).unwrap();
        g.set_live(marked).unwrap();
        g.render_together(reached, after).unwrap();
        g.set_output(master).unwrap();
        let graph = g.prepare(&CONFIG).unwrap();
        // Kept together with a live node, `reached` renders live, but not
        // what feeds it.
        assert_eq!(sides(&graph), [vec![0, 3], vec![1, 2, 4, 5]]);
    }

    /// The node with `key` in either half of `graph`.
    fn node_in<T: Node>(graph: &mut PreparedGraph, key: u64) -> &mut T {
        let half = match graph.find(NodeKey(key)) {
            Some(_) => graph,
            None => graph.live_half.as_deref_mut().expect("a live half"),
        };
        let id = half.find(NodeKey(key)).expect("the node");
        half.nodes[id.0]
            .as_mut()
            .downcast_mut()
            .expect("the node's type")
    }

    #[test]
    fn a_graph_split_in_two_renders_what_it_renders_whole() {
        // Notes, notes made as a node renders, modulation and sound, each
        // from nodes that render ahead to nodes that render live, and a
        // path that delay compensation delays on the live side.
        let build = |split: bool| {
            let mut g = Graph::new();
            let pulse = g.add(NodeKey(10), Pulse::default());
            let octave = g.add(NodeKey(11), OctaveUp::default());
            let keep = g.add(NodeKey(12), Keep(Vec::with_capacity(64)));
            g.send_notes(pulse, octave).unwrap();
            g.send_notes(octave, keep).unwrap();
            g.set_live(keep).unwrap();

            let hit = g.add(NodeKey(20), impulse(70));
            let detects = g.add(NodeKey(21), Detects::new());
            let echoes = g.add(NodeKey(22), Echoes(Vec::with_capacity(8)));
            let octave = g.add(NodeKey(23), OctaveUp::default());
            let kept = g.add(NodeKey(24), Keep(Vec::with_capacity(64)));
            g.connect(hit, detects).unwrap();
            g.send_made_notes(detects, echoes).unwrap();
            g.send_made_notes(echoes, octave).unwrap();
            g.send_notes(octave, kept).unwrap();
            g.set_live(octave).unwrap();

            let first = g.add(NodeKey(30), AddOne::default());
            let second = g.add(NodeKey(31), AddOne::default());
            let target = Modulated {
                got: Vec::with_capacity(64),
                modulated: Vec::new(),
            };
            let target = g.add(NodeKey(32), target);
            g.modulate(first, second, ParamId(3)).unwrap();
            g.modulate(second, target, ParamId(7)).unwrap();
            g.set_live(second).unwrap();

            let dry = g.add(NodeKey(40), impulse(100));
            let wet = g.add(NodeKey(41), latent(10));
            let out = g.add(NodeKey(42), Sum::default());
            g.connect(dry, wet).unwrap();
            g.connect(dry, out).unwrap();
            g.connect(wet, out).unwrap();
            g.set_live(out).unwrap();
            for node in [keep, kept, target] {
                g.connect(node, out).unwrap();
            }
            g.set_output(out).unwrap();
            g.prepare_split(&CONFIG, split).unwrap()
        };
        let (mut whole, mut split) = (build(false), build(true));
        assert_eq!(
            sides(&split),
            [
                vec![10, 11, 20, 21, 22, 30, 40, 41],
                vec![12, 23, 24, 31, 32, 42]
            ]
        );
        assert_eq!(sides(&whole)[1], Vec::<u64>::new());
        let mut outputs = [Vec::new(), Vec::new()];
        for time in (0..256).step_by(64) {
            for (graph, output) in [&mut whole, &mut split].into_iter().zip(&mut outputs) {
                run_block(graph, &mut ReferenceRenderer, time);
                output.extend_from_slice(graph.output(64).channel(0));
            }
        }
        assert_eq!(outputs[0], outputs[1]);
        // Both impulses, lined up on the live side.
        assert_eq!(peaks(&outputs[1]), [(110, 2.0)]);
        let keep = |graph: &mut PreparedGraph, key| node_in::<Keep>(graph, key).0.clone();
        for key in [12, 24] {
            assert_eq!(keep(&mut whole, key), keep(&mut split, key), "{key}");
        }
        assert!(!keep(&mut split, 24).is_empty(), "the echo's notes came");
        let got = |graph: &mut PreparedGraph| node_in::<Modulated>(graph, 32).got.clone();
        assert_eq!(got(&mut whole), got(&mut split));
        assert_eq!(got(&mut split)[0], (0, ParamId(7), 2.0));
    }

    /// Plays on the notes and the switches it gets, as a MIDI effect that
    /// changes nothing would.
    struct PassesOn;

    impl Node for PassesOn {
        fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
            output.clear();
        }

        fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
            for event in ctx.events {
                match event.kind {
                    NodeEventKind::Note(note) => {
                        notes.send(event.offset, note);
                    }
                    NodeEventKind::Controller {
                        controller,
                        value,
                        channel,
                    } => {
                        notes.send_controller_on(event.offset, controller, value, channel);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn a_pedal_let_go_in_one_half_comes_up_before_one_held_on_from_the_other() {
        // A player holding the pedal down and a note, which renders ahead,
        // and a node that keeps what it gets. In the first graph the notes
        // go through a node that plays them on, which renders live; in the
        // second they go straight to the node that keeps them, and nothing
        // renders live. The old graph's halves hand over one after the
        // other, as the engine's two sides do, the ahead half first.
        let build = |between: bool| {
            let mut g = Graph::new();
            let pedal = g.add(NodeKey(0), Pedal::default());
            let keep = g.add(NodeKey(1), KeepAll(Vec::with_capacity(8)));
            match between {
                true => {
                    let passes = g.add(NodeKey(2), PassesOn);
                    g.send_notes(pedal, passes).unwrap();
                    g.send_notes(passes, keep).unwrap();
                    g.set_live(passes).unwrap();
                }
                false => g.send_notes(pedal, keep).unwrap(),
            }
            g.set_output(keep).unwrap();
            g.prepare(&CONFIG).unwrap()
        };
        let mut first = build(true);
        assert_eq!(sides(&first), [vec![0], vec![1, 2]]);
        run_block(&mut first, &mut ReferenceRenderer, 0);
        let mut second = build(false);
        assert_eq!(sides(&second), [vec![0, 1], vec![]]);
        rt::scope(|| second.adopt_from(&mut first));
        node_in::<Pedal>(&mut second, 0).started = true;
        run_block(&mut second, &mut ReferenceRenderer, 64);
        // The node between ends the note and lets the pedal come up on
        // each channel, and only then does the pedal the player holds go
        // down where its notes go now, so it holds on.
        let off = NodeEventKind::Note(NoteEvent::Off {
            note: NoteId(7),
            velocity: Note::RELEASE_VELOCITY,
        });
        let pedal = |value, channel| {
            let kind = NodeEventKind::Controller {
                controller: Controller::SUSTAIN,
                value,
                channel,
            };
            (0, kind)
        };
        let got = std::mem::take(&mut node_in::<KeepAll>(&mut second, 1).0);
        let up = [(0, off), pedal(0.0, None), pedal(0.0, Some(2))];
        let down = [pedal(1.0, None), pedal(1.0, Some(2))];
        assert_eq!(got, [&up[..], &down[..]].concat());
    }
}
