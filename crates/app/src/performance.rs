//! The CPU figures: what each device, each channel and the whole project
//! cost the CPU, worked out from how long the engine's nodes take
//! ([`NodeLoads`]).
//!
//! Four times a second the figures read every node's load, take the
//! difference from the reading a second before, and add the differences up
//! by the device or channel each node belongs to ([`keys::owner`]): the
//! time each took to render a second of audio is the share of a core it
//! needs to keep up. Its longest block is the longest any of its nodes
//! took in that second, as a share of the audio device's period.
//!
//! While the engine renders ahead, the second of audio is a second
//! rendered, which runs ahead of what the device has played as the lead
//! grows, and all of the graph but its live half renders ahead, so only the
//! figures of the devices and channels in the live half have a longest
//! block that matters: an ahead block has only to be ready before the lead
//! runs out, and the engine leaves those out of a node's longest.
//!
//! The Live marks in the tracks' headers come from the same loads: a
//! channel with a node in the live half renders live, as the track or the
//! plugin that put the node there has it, its own if it's one of them.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use musicable_engine::compile::keys::{self, Owner};
use musicable_engine::{EngineConfig, LoadKey, NodeLoads, Stats};
use musicable_model::{Chain, DeviceId, Project, TrackId};
use musicable_ui::{Cpu, Live, Performance, Row};

/// How often the figures are worked out again.
const EVERY: Duration = Duration::from_millis(250);

/// How long the figures average over, in seconds of audio rendered.
const WINDOW: f64 = 1.0;

/// Works out the figures from the engine's loads.
#[derive(Debug, Default)]
pub struct Figures {
    /// The readings of the last second or so, oldest first.
    readings: VecDeque<Reading>,
    /// When the loads were last read.
    read_at: Option<Instant>,
    /// The engine the readings are from, and the frames the device asks
    /// for at a time.
    config: Option<(EngineConfig, u32)>,
    performance: Performance,
}

/// What each node had taken by one moment.
#[derive(Debug, Default)]
struct Reading {
    /// The frames the engine had rendered: played, and ahead.
    time: u64,
    /// The time each node had taken, on every thread.
    busy: BTreeMap<LoadKey, Duration>,
    /// The longest block each node took since the reading before.
    longest: BTreeMap<LoadKey, Duration>,
    /// Whether the engine was rendering ahead.
    ahead: bool,
    /// The nodes that render live even then, as the device asks: the
    /// graph's live half.
    live: BTreeSet<LoadKey>,
}

impl Figures {
    /// The figures, as last worked out.
    pub fn performance(&self) -> &Performance {
        &self.performance
    }

    /// Reads `loads`, the running graph's, if it's time to, and works the
    /// figures out again. `stats` are the engine's, rendering for `config`
    /// through a device that asks for `period` frames at a time, or none
    /// while no device plays. Then, or without loads, there are no figures.
    pub fn update(
        &mut self,
        now: Instant,
        loads: Option<&NodeLoads>,
        period: Option<u32>,
        stats: &Stats,
        config: EngineConfig,
        project: &Project,
    ) {
        let (Some(loads), Some(period)) = (loads, period) else {
            *self = Self::default();
            return;
        };
        if self.read_at.is_some_and(|at| now < at + EVERY) {
            return;
        }
        // Until the engine installs the graph, its loads read nothing, or
        // only some of what they carry on from the graph before.
        if !loads.installed() {
            return;
        }
        self.read_at = Some(now);
        // As of now, as the loads are: the time the renderer's threads
        // spent rendering ahead since the last callback is in them.
        let time = stats.rendered;
        // Another engine, or the same one started again, or playing
        // through another buffer size.
        let restarted = (self.readings.back()).is_some_and(|last| time < last.time);
        if restarted || self.config != Some((config, period)) {
            self.readings.clear();
            self.config = Some((config, period));
        }
        self.readings
            .push_back(read(loads, time, stats.lead_target > 0));
        let window = (WINDOW * f64::from(config.sample_rate)) as u64;
        while self.readings.len() > 2 && self.readings[1].time + window <= time {
            self.readings.pop_front();
        }
        let period = f64::from(period) / f64::from(config.sample_rate);
        let places = Places::new(project);
        self.performance = figures(&self.readings, config.sample_rate, period, &places);
    }
}

/// The time each node in `loads` had taken by the time the engine had
/// rendered `time` frames, rendering ahead or not.
fn read(loads: &NodeLoads, time: u64, ahead: bool) -> Reading {
    let mut reading = Reading {
        time,
        ahead,
        ..Reading::default()
    };
    for (key, load, source) in loads.sides() {
        reading.busy.insert(key, load.busy());
        reading.longest.insert(key, load.take_longest());
        if source.is_some() {
            reading.live.insert(key);
        }
    }
    reading
}

/// The channels that render live while the rest of the project renders
/// ahead, each with what has it render live, for the Live marks in the
/// tracks' headers: worked out again for each graph sent to the engine.
#[derive(Debug, Default)]
pub struct LiveMarks {
    /// The loads of the graph they're from.
    loads: Option<Arc<NodeLoads>>,
    marks: BTreeMap<Chain, Live>,
}

impl LiveMarks {
    pub fn marks(&self) -> &BTreeMap<Chain, Live> {
        &self.marks
    }

    /// Works the marks out from `loads`, those of the graph last sent to
    /// the engine, if they're another graph's than last time, or none if
    /// it has none, or renders nothing ahead. `asks` says whether a plugin
    /// device's plugin says it must render live, rather than being set to.
    pub fn update(
        &mut self,
        loads: Option<&Arc<NodeLoads>>,
        project: &Project,
        asks: impl Fn(DeviceId) -> bool,
    ) {
        let same = match (&self.loads, loads) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (old, new) => old.is_none() && new.is_none(),
        };
        if !same {
            self.loads = loads.cloned();
            self.marks = match loads {
                Some(loads) => live_marks(loads, project, asks),
                None => BTreeMap::new(),
            };
        }
    }
}

/// Each channel with a node in the live half of the graph `loads` are
/// from, with what put the node there: a track's input or its Always Live,
/// or a plugin device, which `asks` says the plugin of has to render live,
/// or else is set to. The channel's own track if it's one, or else one of
/// its devices, or else the first node's.
fn live_marks(
    loads: &NodeLoads,
    project: &Project,
    asks: impl Fn(DeviceId) -> bool,
) -> BTreeMap<Chain, Live> {
    let places = Places::new(project);
    let chain_of = |key| keys::owner(key).and_then(|owner| places.owner_chain(owner));
    // And how near each mark's source is: the channel's own track, one of
    // its devices, or what feeds it.
    let mut marks: BTreeMap<Chain, (u8, Live)> = BTreeMap::new();
    for (key, _, source) in loads.sides() {
        let (LoadKey::Node(key), Some(source)) = (key, source) else {
            continue;
        };
        let Some(chain) = chain_of(key) else {
            continue;
        };
        let (live, on) = match keys::owner(source) {
            Some(Owner::Device(device)) => match asks(device) {
                true => (Live::Plugin(device), places.device(device)),
                false => (Live::Never(device), places.device(device)),
            },
            _ => match chain_of(source) {
                Some(Chain::Track(track)) => (Live::Track(track), Some(Chain::Track(track))),
                _ => continue,
            },
        };
        let near = match (on == Some(chain), live) {
            (true, Live::Track(_)) => 0,
            (true, _) => 1,
            (false, _) => 2,
        };
        match marks.entry(chain) {
            Entry::Vacant(entry) => {
                entry.insert((near, live));
            }
            Entry::Occupied(mut entry) if near < entry.get().0 => {
                entry.insert((near, live));
            }
            Entry::Occupied(_) => {}
        }
    }
    (marks.into_iter())
        .map(|(chain, (_, live))| (chain, live))
        .collect()
}

/// The figures over `readings`, oldest first, of an engine rendering at
/// `sample_rate` through a device with a period of `period` seconds: the
/// time each row's nodes took from the oldest to the newest, over the time
/// the engine rendered meanwhile, and their longest block since the
/// oldest. Each row renders ahead if the engine was, at the newest, and
/// none of its nodes was in the graph's live half.
fn figures(
    readings: &VecDeque<Reading>,
    sample_rate: u32,
    period: f64,
    places: &Places,
) -> Performance {
    let (Some(oldest), Some(newest)) = (readings.front(), readings.back()) else {
        return Performance::default();
    };
    let rendered = newest.time.saturating_sub(oldest.time) as f64 / f64::from(sample_rate);
    if rendered <= 0.0 {
        return Performance::default();
    }
    // A node's load carries on across graphs, and a node new since the
    // oldest reading took all its time since. A node gone since counts no
    // more.
    let mut busy: BTreeMap<Row, Duration> = BTreeMap::new();
    for (&key, &took) in &newest.busy {
        let before = oldest.busy.get(&key).copied().unwrap_or_default();
        *busy.entry(places.row(key)).or_default() += took.saturating_sub(before);
    }
    let mut longest: BTreeMap<Row, Duration> = BTreeMap::new();
    for (&key, &block) in readings.iter().skip(1).flat_map(|reading| &reading.longest) {
        let row = longest.entry(places.row(key)).or_default();
        *row = (*row).max(block);
    }
    let live: BTreeSet<Row> = newest.live.iter().map(|&key| places.row(key)).collect();
    let mut performance = Performance::default();
    for (row, busy) in busy {
        let ahead = newest.ahead && !live.contains(&row);
        let average = (busy.as_secs_f64() / rendered) as f32;
        let longest = longest.get(&row).copied().unwrap_or_default();
        let longest = (longest.as_secs_f64() / period) as f32;
        let cpu = Cpu {
            average,
            longest,
            ahead,
        };
        performance.rows.insert(row, cpu);
        if let Some(chain) = places.chain(row) {
            *performance.channels.entry(chain).or_default() += average;
        }
        performance.total += average;
        if ahead {
            performance.ahead += average;
        }
    }
    performance
}

/// Where the project's devices, sends and wires are, to tell which row
/// each node's load goes in.
struct Places {
    /// The chain each device is on.
    devices: BTreeMap<DeviceId, Chain>,
    /// The track each send is on.
    sends: BTreeMap<u32, TrackId>,
    /// The device each wire runs to.
    wires: BTreeMap<u32, DeviceId>,
}

impl Places {
    fn new(project: &Project) -> Self {
        let devices = (project.chains())
            .flat_map(|(chain, devices)| devices.iter().map(move |device| (device.id, chain)))
            .collect();
        let sends = (project.tracks.iter())
            .flat_map(|track| track.sends.iter().map(|send| (send.id.0, track.id)))
            .collect();
        let wires = (project.wires.iter())
            .map(|wire| (wire.id.0, wire.to.device()))
            .collect();
        Self {
            devices,
            sends,
            wires,
        }
    }

    /// The row the node with `key`'s time goes in: its device's, or else
    /// its channel's. A delay is its channel's mixer's.
    fn row(&self, key: LoadKey) -> Row {
        let owner = keys::owner(key.node());
        let mixer = |owner| {
            self.owner_chain(owner)
                .map_or(Row::Mixer(Chain::Master), Row::Mixer)
        };
        match (key, owner) {
            (LoadKey::Delay { .. }, Some(Owner::Metronome)) => Row::Metronome,
            (LoadKey::Delay { .. }, Some(owner)) => mixer(owner),
            (LoadKey::Node(_), Some(Owner::Device(device))) => Row::Device(device),
            (LoadKey::Node(_), Some(Owner::Clips(track))) => Row::Clips(track),
            (LoadKey::Node(_), Some(Owner::Modulators(chain))) => Row::Modulators(chain),
            (LoadKey::Node(_), Some(Owner::Metronome)) => Row::Metronome,
            (LoadKey::Node(_), Some(owner)) => mixer(owner),
            // No node the graphs are made of has a key without an owner,
            // but the note delays the graph adds of itself, which take
            // next to nothing.
            (_, None) => Row::Mixer(Chain::Master),
        }
    }

    /// The chain whatever `owner` names is on, if the project has it.
    fn owner_chain(&self, owner: Owner) -> Option<Chain> {
        match owner {
            Owner::Device(device) | Owner::BeforeDevice(device) => self.device(device),
            Owner::Clips(track) => Some(Chain::Track(track)),
            Owner::Modulators(chain) | Owner::Strip(chain) => Some(chain),
            Owner::Send(send) => self.sends.get(&send.0).map(|&track| Chain::Track(track)),
            Owner::Wire(wire) => self.wires.get(&wire.0).and_then(|&to| self.device(to)),
            Owner::Metronome => None,
        }
    }

    fn device(&self, device: DeviceId) -> Option<Chain> {
        self.devices.get(&device).copied()
    }

    /// The channel `row`'s time counts towards, if it's a channel's.
    fn chain(&self, row: Row) -> Option<Chain> {
        match row {
            Row::Device(device) => self.device(device),
            Row::Clips(track) => Some(Chain::Track(track)),
            Row::Modulators(chain) | Row::Mixer(chain) => Some(chain),
            Row::Metronome => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_commands::{Keymap, transport};
    use musicable_engine::compile::Slot;
    use musicable_engine::media::Media;
    use musicable_engine::{Devices, Engine, Joint, Lead, Meters, NodeKey, compile_with_devices};
    use musicable_model::routing::{Port, Tap, WireId};
    use musicable_model::{DeviceKind, Send, TrackKind};

    use super::*;
    use crate::Session;

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 480,
    };

    /// What [`project`] has.
    struct Ids {
        bass: TrackId,
        drive: DeviceId,
        reverb: DeviceId,
        limiter: DeviceId,
        send: Send,
        wire: u32,
    }

    /// A Bass track with a Drive that sends to a Reverb's return track,
    /// with a wire from the Drive into the Reverb's side input, and a
    /// Limiter on the master.
    fn project() -> (Project, Ids) {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Audio);
        let reverbs = project.add_track("Reverbs", TrackKind::Return);
        let device = |project: &mut Project, chain| {
            let device = project.new_device(DeviceKind::TestGain { gain: 0.5 });
            project.devices_mut(chain).unwrap().push(device.clone());
            device.id
        };
        let drive = device(&mut project, Chain::Track(bass));
        let reverb = device(&mut project, Chain::Track(reverbs));
        let limiter = device(&mut project, Chain::Master);
        let send = project.new_send(reverbs);
        project.track_mut(bass).unwrap().sends.push(send);
        let wire = project.new_wire(Tap::Device(drive), Port::Sidechain(reverb));
        project.wires.push(wire);
        let ids = Ids {
            bass,
            drive,
            reverb,
            limiter,
            send,
            wire: wire.id.0,
        };
        (project, ids)
    }

    #[test]
    fn nodes_count_towards_their_device_or_their_channel() {
        let (project, ids) = project();
        let places = Places::new(&project);
        let bass = Chain::Track(ids.bass);
        let row = |key| places.row(LoadKey::Node(key));
        let delay = |after| {
            places.row(LoadKey::Delay {
                after,
                at: Joint::Output,
            })
        };
        assert_eq!(row(keys::device(ids.drive)), Row::Device(ids.drive));
        assert_eq!(row(keys::device(ids.limiter)), Row::Device(ids.limiter));
        assert_eq!(row(keys::track_clips(ids.bass)), Row::Clips(ids.bass));
        assert_eq!(row(keys::track_input(ids.bass)), Row::Clips(ids.bass));
        assert_eq!(row(keys::track_tone(ids.bass)), Row::Clips(ids.bass));
        assert_eq!(row(keys::modulation(bass)), Row::Modulators(bass));
        let master = keys::modulation(Chain::Master);
        assert_eq!(row(master), Row::Modulators(Chain::Master));
        // The strip, and what it feeds others.
        assert_eq!(row(keys::track_gain(ids.bass)), Row::Mixer(bass));
        assert_eq!(row(keys::send(ids.send.id)), Row::Mixer(bass));
        let slot = keys::track_slot(ids.bass, Slot::BeforeDevice(ids.drive));
        assert_eq!(row(slot), Row::Mixer(bass));
        // A wire is the mixer's of the channel it feeds.
        let reverbs = Chain::Track(ids.send.to);
        let wire = keys::wire(WireId(ids.wire));
        assert_eq!(row(wire), Row::Mixer(reverbs));
        assert_eq!(row(keys::MASTER), Row::Mixer(Chain::Master));
        assert_eq!(row(keys::master_gain()), Row::Mixer(Chain::Master));
        assert_eq!(row(keys::METRONOME), Row::Metronome);
        assert_eq!(row(keys::CUE), Row::Metronome);
        // Delays are the mixer's of the channel they're on, even after a
        // device.
        assert_eq!(delay(keys::device(ids.reverb)), Row::Mixer(reverbs));
        assert_eq!(delay(keys::track_gain(ids.bass)), Row::Mixer(bass));
        assert_eq!(delay(keys::METRONOME), Row::Metronome);
        // What each row counts towards.
        assert_eq!(places.chain(Row::Device(ids.limiter)), Some(Chain::Master));
        assert_eq!(places.chain(Row::Device(ids.reverb)), Some(reverbs));
        assert_eq!(places.chain(Row::Clips(ids.bass)), Some(bass));
        assert_eq!(places.chain(Row::Metronome), None);
    }

    /// A reading at engine time `time` of the nodes with the keys in
    /// `busy`, with the microseconds each had taken, and the longest block,
    /// in microseconds, of those in `longest`.
    fn reading(time: u64, busy: &[(NodeKey, u64)], longest: &[(NodeKey, u64)]) -> Reading {
        let times = |nodes: &[(NodeKey, u64)]| {
            let time = |&(key, micros)| (LoadKey::Node(key), Duration::from_micros(micros));
            nodes.iter().map(time).collect()
        };
        Reading {
            time,
            busy: times(busy),
            longest: times(longest),
            ..Reading::default()
        }
    }

    /// The period of [`CONFIG`]'s device, in seconds.
    const PERIOD: f64 = 0.01;

    #[track_caller]
    fn assert_near(got: f32, expected: f32) {
        assert!((got - expected).abs() < 1e-4, "{got} for {expected}");
    }

    #[test]
    fn figures_are_shares_of_a_core_over_the_last_second() {
        let (project, ids) = project();
        let places = Places::new(&project);
        let (drive, limiter) = (keys::device(ids.drive), keys::device(ids.limiter));
        let (fader, send) = (keys::track_gain(ids.bass), keys::send(ids.send.id));
        let readings = VecDeque::from([
            reading(
                0,
                &[(drive, 1_000_000), (fader, 0), (send, 9_000)],
                &[(drive, 9_000)],
            ),
            reading(
                24_000,
                &[(drive, 1_100_000), (fader, 10_000)],
                &[(drive, 2_000)],
            ),
            reading(
                48_000,
                &[(drive, 1_250_000), (fader, 20_000), (limiter, 500_000)],
                &[(drive, 5_000), (fader, 100), (limiter, 12_000)],
            ),
        ]);
        let performance = figures(&readings, CONFIG.sample_rate, PERIOD, &places);
        // A quarter of a second's work in a second: a quarter of a core.
        // Its longest block since the oldest reading, 5 ms, is half the
        // 10 ms period.
        let cpu = performance.device(ids.drive).unwrap();
        assert_near(cpu.average, 0.25);
        assert_near(cpu.longest, 0.5);
        // The send's gone, and the fader's time is all there's left of the
        // track's mixer.
        let mixer = performance.rows[&Row::Mixer(Chain::Track(ids.bass))];
        assert_near(mixer.average, 0.02);
        // A device new since took all its time since: here, half a core,
        // with a block longer than the period.
        let cpu = performance.device(ids.limiter).unwrap();
        assert_near(cpu.average, 0.5);
        assert_near(cpu.longest, 1.2);
        // Shares add up.
        let bass = performance.channel(Chain::Track(ids.bass)).unwrap();
        assert_near(bass, 0.27);
        assert_near(performance.channel(Chain::Master).unwrap(), 0.5);
        assert_near(performance.total, 0.77);
        assert_eq!(performance.rows.len(), 3);
        // Nothing rendered in between, no figures.
        let readings = VecDeque::from([reading(48_000, &[], &[]), reading(48_000, &[], &[])]);
        assert_eq!(
            figures(&readings, CONFIG.sample_rate, PERIOD, &places),
            Performance::default()
        );
    }

    #[test]
    fn rows_with_nodes_in_the_live_half_render_live_while_the_rest_renders_ahead() {
        let (project, ids) = project();
        let places = Places::new(&project);
        let (drive, fader) = (keys::device(ids.drive), keys::track_gain(ids.bass));
        let ahead = |mut reading: Reading| {
            reading.ahead = true;
            reading
        };
        let mut newest = ahead(reading(24_000, &[(drive, 250_000), (fader, 25_000)], &[]));
        newest.live.insert(LoadKey::Node(fader));
        let readings = VecDeque::from([ahead(reading(0, &[(drive, 0), (fader, 0)], &[])), newest]);
        let performance = figures(&readings, CONFIG.sample_rate, PERIOD, &places);
        assert!(performance.device(ids.drive).unwrap().ahead);
        assert!(!performance.rows[&Row::Mixer(Chain::Track(ids.bass))].ahead);
        // What renders ahead is the drive's share alone.
        assert_near(performance.ahead, 0.5);
        assert_near(performance.total, 0.55);
    }

    #[test]
    fn live_marks_name_the_track_that_has_each_channel_render_live() {
        let (mut project, ids) = project();
        let reverbs = project.tracks[1].id;
        let keys = project.add_track("Keys", TrackKind::Midi);
        let pads = project.add_track("Pads", TrackKind::Midi);
        for track in [ids.bass, keys] {
            project.track_mut(track).unwrap().always_live = true;
        }
        let (_engine, handle) = Engine::new(CONFIG);
        let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
        session.wait_for_graph();
        let mut marks = LiveMarks::default();
        marks.update(session.loads(), session.project(), |_| false);
        let mark = |chain| marks.marks().get(&chain).copied();
        // Each Always Live track for itself, and the return the bass sends
        // to, and wires into, for the bass.
        let (by_bass, by_keys) = (Live::Track(ids.bass), Live::Track(keys));
        assert_eq!(mark(Chain::Track(ids.bass)), Some(by_bass));
        assert_eq!(mark(Chain::Track(keys)), Some(by_keys));
        assert_eq!(mark(Chain::Track(reverbs)), Some(by_bass));
        assert!([Some(by_bass), Some(by_keys)].contains(&mark(Chain::Master)));
        assert_eq!(mark(Chain::Track(pads)), None);
        // Nothing renders ahead without a graph.
        marks.update(None, session.project(), |_| false);
        assert!(marks.marks().is_empty());
    }

    #[test]
    fn live_marks_name_the_plugin_that_has_each_channel_render_live() {
        let (mut project, ids) = project();
        let reverbs = project.tracks[1].id;
        // The drive says it must render live, and the limiter is set to.
        let mut devices = Devices::new();
        devices.set_live(ids.drive);
        devices.set_live(ids.limiter);
        let loads = |project: &Project| {
            let graph = compile_with_devices(project, &Meters::new(), &Media::new(), &devices);
            graph.prepare(&CONFIG).unwrap().loads()
        };
        let drive = |device| device == ids.drive;
        let mut marks = LiveMarks::default();
        marks.update(Some(&loads(&project)), &project, drive);
        let mark = |marks: &LiveMarks, chain| marks.marks().get(&chain).copied();
        // The drive for its track and the return it feeds, and the
        // limiter for the master, which the drive feeds too.
        let bass = Chain::Track(ids.bass);
        assert_eq!(mark(&marks, bass), Some(Live::Plugin(ids.drive)));
        let reverbs = Chain::Track(reverbs);
        assert_eq!(mark(&marks, reverbs), Some(Live::Plugin(ids.drive)));
        assert_eq!(mark(&marks, Chain::Master), Some(Live::Never(ids.limiter)));
        // A track that renders live of itself names itself first.
        project.track_mut(ids.bass).unwrap().always_live = true;
        marks.update(Some(&loads(&project)), &project, drive);
        assert_eq!(mark(&marks, bass), Some(Live::Track(ids.bass)));
        assert_eq!(mark(&marks, Chain::Master), Some(Live::Never(ids.limiter)));
    }

    #[test]
    fn figures_count_the_frames_rendered_ahead() {
        let (project, ids) = project();
        let (mut engine, handle) = Engine::new(EngineConfig {
            max_block: 1024,
            ..CONFIG
        });
        let config = handle.config();
        let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
        session.set_lead(Lead::Long);
        session.run(&transport::PLAY).unwrap();
        session.wait_for_graph();
        let mut figures = Figures::default();
        let start = Instant::now();
        // Callbacks of 128 frames.
        let mut buffer = vec![0.0; 2 * 128];
        let mut read = |figures: &mut Figures, engine: &mut Engine, after| {
            for _ in 0..100 {
                engine.process_interleaved(&mut buffer, 2);
            }
            let (stats, project) = (session.stats(), session.project());
            let at = start + Duration::from_millis(after);
            let loads = session.loads().map(Arc::as_ref);
            figures.update(at, loads, Some(128), &stats, config, project);
            stats
        };
        read(&mut figures, &mut engine, 0);
        let stats = read(&mut figures, &mut engine, 250);
        // The engine renders ahead, the lead growing, and every row with
        // it.
        assert!(stats.lead > 0 && stats.lead_target > 0, "{stats:?}");
        let performance = figures.performance();
        let drive = performance.device(ids.drive).unwrap();
        assert!(drive.ahead && drive.average > 0.0, "{drive:?}");
        assert!(performance.rows.values().all(|cpu| cpu.ahead));
        assert_near(performance.ahead, performance.total);
        // Over the frames rendered: the readings say how far, as of when
        // they read the loads.
        let readings = &figures.readings;
        let newest = readings.back().unwrap().time;
        assert_eq!(newest, stats.rendered);
        assert!(newest >= stats.time + stats.lead);
    }

    #[test]
    fn a_delay_gone_leaves_the_others_after_its_node_as_they_were() {
        let (project, ids) = project();
        let places = Places::new(&project);
        // Delay compensation added delays after the fader into the master
        // and into the send, and then only the one into the master.
        let fader = keys::track_gain(ids.bass);
        let delay = |to| LoadKey::Delay {
            after: fader,
            at: Joint::Input {
                to,
                from: fader,
                nth: 0,
            },
        };
        let (master, send) = (delay(keys::MASTER), delay(keys::send(ids.send.id)));
        let reading = |time, busy: &[(LoadKey, u64)]| Reading {
            time,
            busy: (busy.iter())
                .map(|&(key, micros)| (key, Duration::from_micros(micros)))
                .collect(),
            ..Reading::default()
        };
        let readings = VecDeque::from([
            reading(0, &[(master, 100_000), (send, 50_000)]),
            reading(48_000, &[(master, 150_000)]),
        ]);
        let performance = figures(&readings, CONFIG.sample_rate, PERIOD, &places);
        // The delay into the master took 50 ms of the second.
        let mixer = performance.rows[&Row::Mixer(Chain::Track(ids.bass))];
        assert_near(mixer.average, 0.05);
    }

    #[test]
    fn figures_wait_for_the_engine_and_follow_it() {
        let (project, ids) = project();
        let (mut engine, handle) = Engine::new(CONFIG);
        let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
        session.run(&transport::PLAY).unwrap();
        session.wait_for_graph();
        let mut figures = Figures::default();
        let start = Instant::now();
        let update = |figures: &mut Figures, session: &Session, after, playing: bool| {
            let (stats, project) = (session.stats(), session.project());
            let at = start + Duration::from_millis(after);
            let period = playing.then_some(CONFIG.max_block as u32);
            let loads = session.loads().map(Arc::as_ref);
            figures.update(at, loads, period, &stats, CONFIG, project);
        };
        let mut buffer = vec![0.0; 2 * CONFIG.max_block];
        let mut play = |engine: &mut Engine, blocks| {
            for _ in 0..blocks {
                engine.process_interleaved(&mut buffer, 2);
            }
        };
        // Until the engine plays the graph, there's nothing to read.
        update(&mut figures, &session, 0, true);
        assert_eq!(figures.performance(), &Performance::default());
        // Then the first reading, which a figure needs another for.
        play(&mut engine, 25);
        update(&mut figures, &session, 0, true);
        assert_eq!(figures.performance(), &Performance::default());
        play(&mut engine, 25);
        // Too soon after it.
        update(&mut figures, &session, 100, true);
        assert_eq!(figures.performance(), &Performance::default());
        update(&mut figures, &session, 250, true);
        let performance = figures.performance().clone();
        for row in [
            Row::Device(ids.drive),
            Row::Device(ids.limiter),
            Row::Mixer(Chain::Track(ids.bass)),
            Row::Mixer(Chain::Master),
        ] {
            let cpu = performance.rows.get(&row).copied().unwrap_or_default();
            assert!(cpu.average > 0.0, "{row:?} reads {cpu:?}");
            assert!(cpu.longest > 0.0, "{row:?} reads {cpu:?}");
        }
        let rows: f32 = performance.rows.values().map(|cpu| cpu.average).sum();
        let channels: f32 = performance.channels.values().sum();
        assert!((performance.total - rows).abs() <= performance.total * 1e-4);
        assert!(channels <= performance.total * (1.0 + 1e-4));
        // Without a device playing, no figures.
        update(&mut figures, &session, 500, false);
        assert_eq!(figures.performance(), &Performance::default());
    }
}
