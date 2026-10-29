//! Building processing graphs and preparing them to run.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use musicable_model::clip::Controller;

use crate::automation::Feed;
use crate::buffer::{AudioBuffer, AudioRef, InputPort};
use crate::engine::EVENT_CAPACITY;
use crate::load::{self, LoadKey, NodeLoads};
use crate::nodes::Delay;
use crate::parallel::Schedule;
use crate::{Automation, Block, EngineConfig, Node, NodeEvent, NodeEventKind, NoteEvent, ParamId};

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
    /// The nodes [`add_tap`](Graph::add_tap) tapped, in order.
    taps: Vec<NodeId>,
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
    /// The automation nodes handed back, to send them as events.
    feeds: Vec<(NodeId, ParamId, Feed)>,
    /// The nodes whose [`Node::control`] runs each block, from
    /// [`control`](Graph::control) and [`modulate`](Graph::modulate).
    controllers: Vec<NodeId>,
    /// Which nodes modulate which nodes' parameters, from
    /// [`modulate`](Graph::modulate), in the order they were added.
    modulations: Vec<(NodeId, NodeId, ParamId)>,
}

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
    /// the main input's.
    pub fn connect_to(
        &mut self,
        from: NodeId,
        to: NodeId,
        port: InputPort,
    ) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.reaches(to, from) {
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
    /// would close one is refused.
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

    fn route_notes(&mut self, from: NodeId, to: NodeId, side: bool) -> Result<(), GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.notes_reach(to, from) {
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
    /// modulating each other is refused. Tells `to` that `param` is
    /// modulated ([`Node::modulated`]).
    pub fn modulate(
        &mut self,
        from: NodeId,
        to: NodeId,
        param: ParamId,
    ) -> Result<usize, GraphError> {
        self.check(from)?;
        self.check(to)?;
        if from == to || self.modulation_reaches(to, from) {
            return Err(GraphError::Cycle);
        }
        self.control(from)?;
        self.nodes[to.0].modulated(param);
        let ports = self.modulations.iter().filter(|m| m.0 == from).count();
        self.modulations.push((from, to, param));
        Ok(ports)
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
        self.taps.push(node);
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
    /// MIDI track records from another's notes. A node plays only notes
    /// that go somewhere ([`send_notes`](Graph::send_notes)). Each node's
    /// notes go to one writer, and each writer gets one node's: a record
    /// of a node, or with a key, that has one already isn't added.
    pub fn add_note_record(&mut self, key: NodeKey, node: NodeId) -> Result<(), GraphError> {
        self.check(node)?;
        if !(self.note_records.iter()).any(|&(k, n)| k == key || n == node) {
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
    /// # Panics
    ///
    /// If the sample rate or the block size in `config` is zero.
    pub fn prepare(mut self, config: &EngineConfig) -> Result<PreparedGraph, GraphError> {
        assert!(
            config.sample_rate > 0 && config.max_block > 0,
            "invalid engine config {config:?}"
        );
        let mut output = self.output.ok_or(GraphError::NoOutput)?;
        let mut by_key: Vec<(NodeKey, NodeId)> =
            self.keys.iter().copied().zip((0..).map(NodeId)).collect();
        by_key.sort_unstable();
        if let Some(pair) = by_key.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(GraphError::DuplicateKey(pair[0].0));
        }
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.prepare(config);
            if node.channels() == 0 {
                return Err(GraphError::NoChannels(NodeId(i)));
            }
        }
        self.sort_ports();
        let (mut delays, latency) = self.compensate(config, &mut output);
        delays.sort_unstable();
        let buffers: Vec<AudioBuffer> = self
            .nodes
            .iter()
            .map(|node| AudioBuffer::new(node.channels(), config.max_block))
            .collect();
        let order = self.topological_order();
        let schedule = Schedule::new(&self.inputs, &self.outputs, &order);
        // Each delay has one input, the node it comes after, which has a key,
        // and one joint.
        let mut joints = vec![None; self.nodes.len() - self.keys.len()];
        for &(joint, delay) in &delays {
            joints[delay.0 - self.keys.len()] = Some(joint);
        }
        let delayed = (self.keys.len()..)
            .zip(joints)
            .map(|(n, at)| LoadKey::Delay {
                after: self.keys[self.inputs[n][0].0],
                at: at.expect("each delay is at a joint"),
            });
        let loads = (self.keys.iter().map(|&key| LoadKey::Node(key)))
            .chain(delayed)
            .collect();
        self.note_routes.sort_unstable();
        let mut players: Vec<Player> = Vec::new();
        for &(from, to, side) in &self.note_routes {
            match players.last_mut() {
                Some(player) if player.node == from => player.targets.push((to, side)),
                _ => players.push(Player {
                    node: from,
                    targets: vec![(to, side)],
                    record: None,
                }),
            }
        }
        // A record of a node that plays no notes stays empty.
        let note_records: Vec<NoteRecord> = (self.note_records.iter())
            .enumerate()
            .map(|(i, &(key, node))| {
                if let Some(player) = players.iter_mut().find(|p| p.node == node) {
                    player.record = Some(i);
                }
                NoteRecord {
                    key,
                    notes: Vec::with_capacity(Notes::CAPACITY),
                }
            })
            .collect();
        // Each player after the players whose notes it gets. Notes don't go
        // around loops, so one is always ready.
        let routes = &self.note_routes;
        let mut ordered: Vec<Player> = Vec::with_capacity(players.len());
        while !players.is_empty() {
            let waiting = |p: &Player| {
                let from_waiting = |from: NodeId| players.iter().any(|q| q.node == from);
                routes
                    .iter()
                    .any(|&(from, to, _)| to == p.node && from_waiting(from))
            };
            let ready = players.iter().position(|p| !waiting(p)).expect("no loops");
            ordered.push(players.remove(ready));
        }
        let mut plays_notes = vec![false; self.nodes.len()];
        let mut inbox = 0;
        for player in &ordered {
            let routes_in = (routes.iter()).filter(|r| r.1 == player.node).count();
            plays_notes[player.node.0] = true;
            inbox = inbox.max(EVENT_CAPACITY + routes_in * Notes::CAPACITY);
        }
        let room = Feed::room(config.max_block);
        let controllers = self.order_controllers();
        let mut is_controller = vec![false; self.nodes.len()];
        for controller in &controllers {
            let node = controller.node;
            is_controller[node.0] = true;
            // What comes to it: the engine's events, the notes, its
            // automation and the modulation of the nodes that modulate it.
            let routes_in = routes.iter().filter(|r| r.1 == node).count();
            let feeds_in = self.feeds.iter().filter(|f| f.0 == node).count();
            let ports_in = self.modulations.iter().filter(|m| m.1 == node).count();
            let notes_in = routes_in * Notes::CAPACITY;
            inbox = inbox.max(EVENT_CAPACITY + notes_in + (feeds_in + ports_in) * room);
        }
        // Room for every parameter event the engine can hold, for each
        // player's notes to each of its targets, for the automation the
        // graph sends and for what each port of each controller sends.
        let notes: usize = ordered.iter().map(|p| p.targets.len()).sum();
        let feeds = self.feeds.len() * room;
        let ports = self.modulations.len() * room;
        let capacity = EVENT_CAPACITY + notes * Notes::CAPACITY + feeds + ports;
        // The nodes that can be live sources, for the engine to ask before
        // each block, and their keys, for the events it sends them.
        let live: Vec<NodeId> = (0..self.nodes.len())
            .filter(|&n| self.nodes[n].live().is_some())
            .map(NodeId)
            .collect();
        let mut live_keys: Vec<NodeKey> = (live.iter())
            .filter_map(|id| self.keys.get(id.0).copied())
            .collect();
        live_keys.sort_unstable();
        Ok(PreparedGraph {
            config: *config,
            event_ranges: vec![(0, 0); self.nodes.len()],
            nodes: self.nodes,
            inputs: self.inputs,
            ports: self.ports,
            order,
            output,
            cue: self.cue,
            taps: self.taps,
            records: self.records,
            note_records,
            buffers,
            by_key,
            delays,
            latency,
            events: Vec::with_capacity(capacity),
            routed: Vec::with_capacity(capacity),
            players: ordered,
            plays_notes,
            inbox: Vec::with_capacity(inbox),
            inbox_events: Vec::with_capacity(inbox),
            feeds: self.feeds,
            controllers,
            is_controller,
            schedule,
            loads: Arc::new(NodeLoads::new(loads)),
            live,
            live_keys,
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
        let taps_late = self.taps.iter().map(|tap| late[tap.0]).max();
        let latency = late[output.0].max(cue_late).max(taps_late.unwrap_or(0));
        if let Some(cue) = self.cue {
            self.cue = Some(self.line_up(cue, &late, latency, Joint::Cue, &mut delays, config));
        }
        *output = self.line_up(*output, &late, latency, Joint::Output, &mut delays, config);
        for n in 0..self.taps.len() {
            let joint = Joint::Tap(u32::try_from(n).unwrap_or(u32::MAX));
            let tap = self.taps[n];
            self.taps[n] = self.line_up(tap, &late, latency, joint, &mut delays, config);
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
    /// Every node comes after all of its inputs.
    pub(crate) order: Vec<NodeId>,
    pub(crate) output: NodeId,
    pub(crate) cue: Option<NodeId>,
    /// The nodes whose outputs are the taps, lined up with the output.
    taps: Vec<NodeId>,
    /// The record taps, by their writers' keys, with their numbers.
    records: Vec<(NodeKey, usize)>,
    /// The notes each recorded node played in the current block, by their
    /// writers' keys.
    note_records: Vec<NoteRecord>,
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
    /// The nodes that play notes, each after those whose notes it gets, and
    /// where their notes go.
    players: Vec<Player>,
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
    /// How the parallel renderer runs the graph.
    pub(crate) schedule: Schedule,
    /// How long each node takes.
    pub(crate) loads: Arc<NodeLoads>,
    /// The nodes that can be live sources ([`Node::live`]).
    live: Vec<NodeId>,
    /// Their keys, sorted.
    live_keys: Vec<NodeKey>,
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

/// A node event on its way into a graph, with its arrival order.
#[derive(Clone, Copy)]
pub(crate) struct Routed {
    pub node: NodeId,
    pub order: u32,
    pub event: NodeEvent,
}

/// A node that plays notes, the nodes its notes go to, and whether to their
/// side inputs for notes, and its note record, if it has one.
struct Player {
    node: NodeId,
    targets: Vec<(NodeId, bool)>,
    record: Option<usize>,
}

/// The notes a node played in the current block, for the writer with
/// `key`: see [`Graph::add_note_record`].
struct NoteRecord {
    key: NodeKey,
    notes: Vec<NodeEvent>,
}

/// A node that modulates others, and the node and parameter each of its
/// ports goes to.
struct Modulator {
    node: NodeId,
    ports: Vec<(NodeId, ParamId)>,
}

/// Where a node puts the notes it plays in a block: see
/// [`Node::play_notes`].
pub struct Notes<'a> {
    routed: &'a mut Vec<Routed>,
    targets: &'a [(NodeId, bool)],
    /// Where the node's notes are recorded, if they are.
    record: Option<&'a mut Vec<NodeEvent>>,
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
        for &(node, side) in self.targets {
            let order = self.routed.len() as u32;
            let kind = if side { kind.to_side() } else { kind };
            self.routed.push(Routed {
                node,
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
    /// frames into the block, in order with its notes. It takes room as a
    /// note does, and returns whether there was some.
    pub fn send_controller(&mut self, offset: usize, controller: Controller, value: f32) -> bool {
        self.send_kind(offset, NodeEventKind::Controller { controller, value })
    }

    /// The block's length: notes are sent at offsets below it.
    pub fn frames(&self) -> usize {
        self.frames
    }
}

/// Where a node puts the modulation it makes in a block: see
/// [`Node::control`].
pub struct Controls<'a> {
    routed: &'a mut Vec<Routed>,
    /// Where each port goes.
    ports: &'a [(NodeId, ParamId)],
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
        self.routed.push(Routed { node, order, event });
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

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether a node has `key`.
    pub fn contains(&self, key: NodeKey) -> bool {
        self.find(key).is_some()
    }

    /// The output node's most recent `frames` frames.
    pub fn output(&self, frames: usize) -> AudioRef<'_> {
        self.buffers[self.output.0].view(frames)
    }

    /// The cue node's most recent `frames` frames, if the graph has one (see
    /// [`Graph::set_cue`]).
    pub fn cue(&self, frames: usize) -> Option<AudioRef<'_>> {
        self.cue.map(|cue| self.buffers[cue.0].view(frames))
    }

    /// Tap `n`'s most recent `frames` frames, if the graph has it (see
    /// [`Graph::add_tap`]).
    pub fn tap(&self, n: usize, frames: usize) -> Option<AudioRef<'_>> {
        self.taps.get(n).map(|tap| self.buffers[tap.0].view(frames))
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

    /// The keys of the writers the graph has record taps for, in the order
    /// they were added.
    pub fn records(&self) -> impl Iterator<Item = NodeKey> + '_ {
        self.records.iter().map(|&(key, _)| key)
    }

    /// The notes the node the writer with `key` records played in the last
    /// block, in order, each at its offset into the block, if the graph
    /// has a note record with that key (see [`Graph::add_note_record`]).
    pub fn record_notes(&self, key: NodeKey) -> Option<&[NodeEvent]> {
        let record = self.note_records.iter().find(|r| r.key == key)?;
        Some(&record.notes)
    }

    /// The keys of the writers the graph has note records for, in the
    /// order they were added.
    pub fn note_records(&self) -> impl Iterator<Item = NodeKey> + '_ {
        self.note_records.iter().map(|r| r.key)
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

    /// Whether any node is a live source now ([`Node::live`]).
    pub(crate) fn is_live(&self) -> bool {
        (self.live.iter()).any(|id| self.nodes[id.0].live() == Some(true))
    }

    /// Whether the node with `key` can be a live source.
    pub(crate) fn can_be_live(&self, key: NodeKey) -> bool {
        self.live_keys.binary_search(&key).is_ok()
    }

    /// How many channels the output has.
    pub(crate) fn output_channels(&self) -> usize {
        self.buffers[self.output.0].channels()
    }

    /// How many channels the cue output has: none without one.
    pub(crate) fn cue_channels(&self) -> usize {
        self.cue.map_or(0, |cue| self.buffers[cue.0].channels())
    }

    pub(crate) fn find(&self, key: NodeKey) -> Option<NodeId> {
        self.by_key
            .binary_search_by_key(&key, |&(k, _)| k)
            .ok()
            .map(|i| self.by_key[i].1)
    }

    /// Lets each node take over from the node with the same key in `old`,
    /// and each delay from the delay at the same joint. Runs on the audio
    /// thread.
    pub(crate) fn adopt_from(&mut self, old: &mut PreparedGraph) {
        let (nodes, old_nodes) = (&mut self.nodes, &mut old.nodes);
        let (loads, old_loads) = (&self.loads, &old.loads);
        let mut adopt = |id: NodeId, old_id: NodeId| {
            nodes[id.0].adopt(old_nodes[old_id.0].as_mut());
            loads.carry_on(id.0, old_loads, old_id.0);
        };
        pairs(&self.by_key, &old.by_key, &mut adopt);
        pairs(&self.delays, &old.delays, &mut adopt);
    }

    /// Queues an event for the next block. Returns whether there was room:
    /// there always is for the events the engine holds.
    pub(crate) fn route(&mut self, node: NodeId, event: NodeEvent) -> bool {
        if self.routed.len() == self.routed.capacity() {
            return false;
        }
        let order = self.routed.len() as u32;
        self.routed.push(Routed { node, order, event });
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
                routed.push(Routed { node, order, event });
            });
        }
    }

    /// Has each node that plays notes send its notes for `block`, and
    /// returns how many there was no room for. Runs on the audio thread,
    /// before the block renders.
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
            ..
        } = self;
        let is_played = |r: &Routed| r.event.kind.is_played();
        for record in note_records.iter_mut() {
            record.notes.clear();
        }
        let mut dropped = 0;
        for player in players.iter() {
            // The notes sent to it so far, which is all of them: the engine
            // routed its own before the block, and the players they come
            // from have played.
            inbox.clear();
            let sent = routed
                .iter()
                .filter(|r| r.node == player.node && is_played(r));
            inbox.extend(sent);
            inbox.sort_unstable_by_key(|r| (r.event.offset, r.order));
            inbox_events.clear();
            inbox_events.extend(inbox.iter().map(|r| r.event));
            let ctx = block.context(config.sample_rate, inbox_events);
            let mut notes = Notes {
                routed,
                targets: &player.targets,
                record: player.record.map(|i| &mut note_records[i].notes),
                frames: block.frames,
                room: Notes::CAPACITY,
                dropped: 0,
            };
            let started = load::now();
            nodes[player.node.0].play_notes(&ctx, &mut notes);
            let ticks = load::now().wrapping_sub(started);
            loads.loads[player.node.0].add_before(ticks);
            dropped += notes.dropped;
        }
        // The players have had theirs.
        if !players.is_empty() {
            routed.retain(|r| !(plays_notes[r.node.0] && is_played(r)));
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
            ..
        } = self;
        if controllers.is_empty() {
            return 0;
        }
        let room = Feed::room(config.max_block);
        let mut dropped = 0;
        for controller in controllers.iter() {
            // Everything sent to it so far, which is all of it: the nodes
            // that modulate it have had their turn.
            inbox.clear();
            inbox.extend(routed.iter().filter(|r| r.node == controller.node));
            inbox.sort_unstable_by_key(|r| (r.event.offset, r.order));
            inbox_events.clear();
            inbox_events.extend(inbox.iter().map(|r| r.event));
            let ctx = block.context(config.sample_rate, inbox_events);
            let mut controls = Controls {
                routed,
                ports: &controller.ports,
                frames: block.frames,
                room: controller.ports.len() * room,
                dropped: 0,
            };
            let started = load::now();
            nodes[controller.node.0].control(&ctx, &mut controls);
            let ticks = load::now().wrapping_sub(started);
            loads.loads[controller.node.0].add_before(ticks);
            dropped += controls.dropped;
        }
        // The controllers have had theirs.
        routed.retain(|r| !is_controller[r.node.0]);
        dropped
    }

    /// Stores the events queued for the block about to render, and empties
    /// the queue.
    pub(crate) fn load_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        self.load_events(&mut routed);
        self.routed = routed;
    }

    /// Clears what [`load_routed`](Self::load_routed) stored, once the block
    /// has rendered.
    pub(crate) fn clear_routed(&mut self) {
        let mut routed = std::mem::take(&mut self.routed);
        self.clear_events(&routed);
        routed.clear();
        self.routed = routed;
    }

    /// Stores a block's node events, grouped by node, in order of offset and
    /// otherwise in arrival order. `routed` must fit in `events`.
    pub(crate) fn load_events(&mut self, routed: &mut [Routed]) {
        routed.sort_unstable_by_key(|r| (r.node, r.event.offset, r.order));
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
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::TempoMap;

    use super::*;
    use crate::nodes::{Constant, Gain, Impulse, Latent, Sum};
    use crate::{
        AudioMut, Inputs, NoteId, ProcessContext, ReferenceRenderer, Renderer, TransportState, rt,
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
            };
            // Notes from the engine, as `Event::Note` sends them, come to a
            // player with the notes of the players before it.
            let from_engine = |key| NodeEvent {
                offset: 10,
                kind: NodeEventKind::Note(NoteEvent::On {
                    note: NoteId(1000 + position),
                    key,
                    velocity: 1.0,
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
        // One writer for each node, and one node for each writer.
        g.add_note_record(NodeKey(10), pulse).unwrap();
        g.add_note_record(NodeKey(9), main).unwrap();
        // A node that plays no notes has an empty record.
        g.add_note_record(NodeKey(11), main).unwrap();
        g.set_output(main).unwrap();
        let mut graph = g.prepare(&CONFIG).unwrap();
        assert!(graph.note_records().eq([NodeKey(9), NodeKey(11)]));
        let tempo = TempoMap::default();
        let block = Block {
            frames: 64,
            time: 0,
            transport: TransportState::default(),
            tempo: &tempo,
            input: AudioRef::empty(64),
            midi: &[],
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
        assert_eq!(graph.record_notes(NodeKey(11)), Some(&[][..]));
        assert_eq!(graph.record_notes(NodeKey(10)), None);
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
}
