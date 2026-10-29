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

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use musicable_engine::compile::keys::{self, Owner};
use musicable_engine::{EngineConfig, LoadKey, NodeLoads};
use musicable_model::{Chain, DeviceId, Project, TrackId};
use musicable_ui::{Cpu, Performance, Row};

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
    /// The engine the readings are from.
    config: Option<EngineConfig>,
    performance: Performance,
}

/// What each node had taken by one moment.
#[derive(Debug, Default)]
struct Reading {
    /// The engine's time: the frames it had rendered.
    time: u64,
    /// The time each node had taken, on every thread.
    busy: BTreeMap<LoadKey, Duration>,
    /// The longest block each node took since the reading before.
    longest: BTreeMap<LoadKey, Duration>,
}

impl Figures {
    /// The figures, as last worked out.
    pub fn performance(&self) -> &Performance {
        &self.performance
    }

    /// Reads `loads`, the running graph's, if it's time to, and works the
    /// figures out again. `time` is the engine's, the frames it has
    /// rendered, and `config` what it renders for. Without loads, or while
    /// no device plays, there are no figures.
    pub fn update(
        &mut self,
        now: Instant,
        loads: Option<&NodeLoads>,
        playing: bool,
        time: u64,
        config: EngineConfig,
        project: &Project,
    ) {
        let (Some(loads), true) = (loads, playing) else {
            *self = Self::default();
            return;
        };
        if self.read_at.is_some_and(|at| now < at + EVERY) {
            return;
        }
        // Until the engine installs the graph, its loads read nothing: the
        // readings carry on once it has.
        if !loads.iter().any(|(_, load)| load.frames() > 0) {
            return;
        }
        self.read_at = Some(now);
        // Another engine, or the same one started again.
        let restarted = (self.readings.back()).is_some_and(|last| time < last.time);
        if restarted || self.config != Some(config) {
            self.readings.clear();
            self.config = Some(config);
        }
        self.readings.push_back(read(loads, time));
        let window = (WINDOW * f64::from(config.sample_rate)) as u64;
        while self.readings.len() > 2 && self.readings[1].time + window <= time {
            self.readings.pop_front();
        }
        self.performance = figures(&self.readings, config, &Places::new(project));
    }
}

/// The time each node in `loads` had taken by engine time `time`.
fn read(loads: &NodeLoads, time: u64) -> Reading {
    let mut reading = Reading {
        time,
        ..Reading::default()
    };
    for (key, load) in loads.iter() {
        // Delay compensation can add more than one delay after a node.
        *reading.busy.entry(key).or_default() += load.busy();
        let longest = reading.longest.entry(key).or_default();
        *longest = (*longest).max(load.take_longest());
    }
    reading
}

/// The figures over `readings`, oldest first: the time each row's nodes
/// took from the oldest to the newest, over the time the engine rendered
/// meanwhile, and their longest block since the oldest.
fn figures(readings: &VecDeque<Reading>, config: EngineConfig, places: &Places) -> Performance {
    let (Some(oldest), Some(newest)) = (readings.front(), readings.back()) else {
        return Performance::default();
    };
    let rendered = newest.time.saturating_sub(oldest.time) as f64 / f64::from(config.sample_rate);
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
    let period = config.max_block as f64 / f64::from(config.sample_rate);
    let mut performance = Performance::default();
    for (row, busy) in busy {
        let average = (busy.as_secs_f64() / rendered) as f32;
        let longest = longest.get(&row).copied().unwrap_or_default();
        let longest = (longest.as_secs_f64() / period) as f32;
        performance.rows.insert(row, Cpu { average, longest });
        if let Some(chain) = places.chain(row) {
            *performance.channels.entry(chain).or_default() += average;
        }
        performance.total += average;
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
            (LoadKey::Delay(_), Some(Owner::Metronome)) => Row::Metronome,
            (LoadKey::Delay(_), Some(owner)) => mixer(owner),
            (LoadKey::Node(_), Some(Owner::Device(device))) => Row::Device(device),
            (LoadKey::Node(_), Some(Owner::Clips(track))) => Row::Clips(track),
            (LoadKey::Node(_), Some(Owner::Modulators(chain))) => Row::Modulators(chain),
            (LoadKey::Node(_), Some(Owner::Metronome)) => Row::Metronome,
            (LoadKey::Node(_), Some(owner)) => mixer(owner),
            // No node the graphs are made of has a key without an owner.
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
    use musicable_engine::{Engine, NodeKey};
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
        let delay = |key| places.row(LoadKey::Delay(key));
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
        }
    }

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
        let performance = figures(&readings, CONFIG, &places);
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
        assert_eq!(figures(&readings, CONFIG, &places), Performance::default());
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
        let update = |figures: &mut Figures, session: &Session, after, playing| {
            let (stats, project) = (session.stats(), session.project());
            let at = start + Duration::from_millis(after);
            figures.update(at, session.loads(), playing, stats.time, CONFIG, project);
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
