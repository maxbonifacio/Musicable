//! Routing between channels: taps, ports and wires.
//!
//! A [`Tap`] is a point on a channel where routing can take its signal:
//! what comes into a track's devices (Pre FX), what a device puts out, what
//! comes out of the last device (Post FX), and what comes out of the fader
//! and pan (Post Fader). The master has Post FX and Post Fader. Taps come
//! before mute and solo, so what's taken from a muted track carries on: a
//! muted ghost kick still keys a compressor. A [`Port`] takes a signal into
//! a device: the Compressor's and the Gate's Sidechain, or one of a
//! plugin's side inputs. A [`Wire`]
//! joins a tap to a port, with a gain, and wires into the same port sum.
//! The project keeps one list of them.
//!
//! Signal never flows in a loop. Outputs and sends can't make one on their
//! own (see [`mixer`](crate::mixer)), but a wire can close one with them,
//! such as a compressor keyed from the group its own track feeds, or from a
//! device after it. The check follows points, not tracks, so two tracks can
//! key each other from their Pre FX. [`Project::check_wire`] says whether a
//! wire can join the project, and [`Project::wire_in_a_loop`] finds the
//! wire that closes a loop, after an edit that moves devices, outputs or
//! sends.

use std::collections::{HashMap, HashSet};

use crate::mixer::Bus;
use crate::native::NativeKind;
use crate::{Chain, DeviceId, DeviceKind, Project, TrackId, TrackKind, next_id};

/// Stable identifier for a wire, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WireId(pub u32);

/// A point on a channel where routing can take its signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tap {
    /// What comes into a track's devices: its clips and its input, or on a
    /// group or a return track, what feeds it. A MIDI track's is its
    /// notes, which no wire takes yet. The master has none.
    PreFx(TrackId),
    /// What a device puts out. A deactivated device is out of the signal,
    /// so its tap has what comes into it.
    Device(DeviceId),
    /// What comes out of a channel's last device, where its pre-fader
    /// sends take their signal.
    PostFx(Chain),
    /// What comes out of a channel's fader and pan, where its post-fader
    /// sends take their signal, before mute and solo.
    PostFader(Chain),
}

/// Where a wire takes its signal into a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Port {
    /// A Compressor's or a Gate's side input, which its detector hears
    /// while its Sidechain is on.
    Sidechain(DeviceId),
    /// One of a plugin's side inputs, by the plugin's id for it
    /// ([`PluginInput`](crate::plugin::PluginInput)).
    PluginInput(DeviceId, u32),
}

impl Port {
    /// The device it takes the signal into.
    pub fn device(self) -> DeviceId {
        match self {
            Self::Sidechain(device) | Self::PluginInput(device, _) => device,
        }
    }

    /// The same port on the device with id `device`: where a copy of its
    /// device takes the signal.
    pub fn on(self, device: DeviceId) -> Self {
        match self {
            Self::Sidechain(_) => Self::Sidechain(device),
            Self::PluginInput(_, input) => Self::PluginInput(device, input),
        }
    }
}

/// A tap's signal, fed into a port at a gain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wire {
    pub id: WireId,
    pub from: Tap,
    pub to: Port,
    /// Linear gain, 1.0 = unity, up to [`GAIN_MAX`](Self::GAIN_MAX).
    pub gain: f32,
}

impl Wire {
    /// The loudest a wire's gain goes: +24 dB, as linear gain.
    pub const GAIN_MAX: f32 = 15.848_932;

    /// Whether it runs to or from the device with id `device`: from its
    /// tap, or into one of its ports.
    pub fn touches(&self, device: DeviceId) -> bool {
        self.from == Tap::Device(device) || self.to.device() == device
    }
}

/// Why a wire can't join a project: see [`Project::check_wire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    /// The project hasn't the tap's track or device.
    NoTap,
    /// The tap is a MIDI track's Pre FX, which is notes.
    Notes,
    /// The project hasn't the port: its device, or a device of a kind that
    /// has it.
    NoPort,
    /// The tap is wired to the port already, by this wire.
    Wired(WireId),
    /// Signal from the port's device reaches the tap already, so the wire
    /// would close a loop.
    Loop,
}

impl Project {
    /// A wire from `from` to `to` at unity gain, with the next wire id, for
    /// the caller to add.
    ///
    /// # Panics
    ///
    /// If the project has used up every wire id.
    pub fn new_wire(&mut self, from: Tap, to: Port) -> Wire {
        Wire {
            id: WireId(next_id(&mut self.next_wire_id, "wire")),
            from,
            to,
            gain: 1.0,
        }
    }

    pub fn wire(&self, id: WireId) -> Option<&Wire> {
        self.wires.iter().find(|w| w.id == id)
    }

    pub fn wire_mut(&mut self, id: WireId) -> Option<&mut Wire> {
        self.wires.iter_mut().find(|w| w.id == id)
    }

    /// Where the wire with `id` is in the project's list of them.
    pub fn wire_index(&self, id: WireId) -> Option<usize> {
        self.wires.iter().position(|w| w.id == id)
    }

    /// The wires into `port`, in the order the project lists them.
    pub fn wires_into(&self, port: Port) -> impl Iterator<Item = &Wire> + '_ {
        self.wires.iter().filter(move |w| w.to == port)
    }

    /// The chain `tap` is on, if the project has its track or device.
    pub fn tap_chain(&self, tap: Tap) -> Option<Chain> {
        match tap {
            Tap::PreFx(track) => self.track(track).map(|t| Chain::Track(t.id)),
            Tap::Device(device) => self.find_device(device).map(|(chain, ..)| chain),
            Tap::PostFx(chain) | Tap::PostFader(chain) => {
                self.devices(chain).is_some().then_some(chain)
            }
        }
    }

    /// The chain the device that `port` belongs to is on, if the project
    /// has the device.
    pub fn port_chain(&self, port: Port) -> Option<Chain> {
        self.find_device(port.device()).map(|(chain, ..)| chain)
    }

    /// The taps on `chain`, in signal order: Pre FX, after each device,
    /// Post FX and Post Fader. The master has no Pre FX, and a chain the
    /// project hasn't has none.
    pub fn taps(&self, chain: Chain) -> Vec<Tap> {
        let Some(devices) = self.devices(chain) else {
            return Vec::new();
        };
        let mut taps = Vec::with_capacity(devices.len() + 3);
        if let Chain::Track(track) = chain {
            taps.push(Tap::PreFx(track));
        }
        taps.extend(devices.iter().map(|d| Tap::Device(d.id)));
        taps.extend([Tap::PostFx(chain), Tap::PostFader(chain)]);
        taps
    }

    /// What the tap is called on its channel: "Pre FX", "after EQ", "Post
    /// FX" or "Post Fader". `None` for a device the project hasn't.
    pub fn tap_point_name(&self, tap: Tap) -> Option<String> {
        Some(match tap {
            Tap::PreFx(_) => "Pre FX".to_owned(),
            Tap::Device(device) => format!("after {}", self.find_device(device)?.2.kind.name()),
            Tap::PostFx(_) => "Post FX".to_owned(),
            Tap::PostFader(_) => "Post Fader".to_owned(),
        })
    }

    /// What the tap is called anywhere: its channel's name and its point on
    /// it, such as "Kick Post FX" or "Bass after EQ". `None` for a tap the
    /// project hasn't.
    pub fn tap_name(&self, tap: Tap) -> Option<String> {
        let channel = self.channel_name(self.tap_chain(tap)?)?;
        Some(format!("{channel} {}", self.tap_point_name(tap)?))
    }

    /// What the port is called on its device: "Sidechain" on a Compressor
    /// or a Gate, and a plugin's side input what the plugin calls it, or
    /// "Side Input" where that's nothing. `None` for a port the project
    /// hasn't.
    pub fn input_name(&self, port: Port) -> Option<&str> {
        let (_, _, device) = self.find_device(port.device())?;
        match (port, &device.kind) {
            (Port::Sidechain(_), _) => self.has_port(port).then_some("Sidechain"),
            (Port::PluginInput(_, input), DeviceKind::Plugin(plugin)) => {
                let name = plugin.input(input)?.name.trim();
                Some(if name.is_empty() { "Side Input" } else { name })
            }
            (Port::PluginInput(..), _) => None,
        }
    }

    /// What the port is called anywhere: its device and its channel, such
    /// as "Compressor on Bass", and a plugin's side input's name after the
    /// plugin's, such as "Pro-C 2 Sidechain on Bass". `None` for a device
    /// the project hasn't, or a side input its plugin hasn't.
    pub fn port_name(&self, port: Port) -> Option<String> {
        let (chain, _, device) = self.find_device(port.device())?;
        let name = device.kind.name();
        let device = match port {
            Port::Sidechain(_) => name.to_owned(),
            Port::PluginInput(..) => format!("{name} {}", self.input_name(port)?),
        };
        Some(format!("{device} on {}", self.channel_name(chain)?))
    }

    /// The name of the track with `chain`, or "Master".
    fn channel_name(&self, chain: Chain) -> Option<&str> {
        match chain {
            Chain::Track(track) => Some(&self.track(track)?.name),
            Chain::Master => Some("Master"),
        }
    }

    /// The ports of the device with id `device`: a Compressor's or a Gate's
    /// Sidechain, or a plugin's side inputs, in the plugin's order. None
    /// for other devices, or a device the project hasn't.
    pub fn ports(&self, device: DeviceId) -> Vec<Port> {
        let Some((_, _, found)) = self.find_device(device) else {
            return Vec::new();
        };
        match &found.kind {
            DeviceKind::Native(native)
                if matches!(native.kind, NativeKind::Compressor | NativeKind::Gate) =>
            {
                vec![Port::Sidechain(device)]
            }
            DeviceKind::Plugin(plugin) => (plugin.inputs.iter())
                .map(|input| Port::PluginInput(device, input.id))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Whether the project has `port`: its device, of a kind that has it.
    pub fn has_port(&self, port: Port) -> bool {
        self.ports(port.device()).contains(&port)
    }

    /// Whether a wire from `from` to `to` can join the project: see
    /// [`check_wire`](Self::check_wire).
    pub fn can_wire(&self, from: Tap, to: Port) -> bool {
        self.check_wire(from, to).is_ok()
    }

    /// Whether a wire from `from` to `to` can join the project, and if not,
    /// why not. It can if the project has both ends, the tap carries sound,
    /// they aren't wired together already, and the signal from the port's
    /// device doesn't reach the tap, so the wire makes no loop.
    pub fn check_wire(&self, from: Tap, to: Port) -> Result<(), WireError> {
        self.check_ends(from, to)?;
        if let Some(wired) = self.wires.iter().find(|w| (w.from, w.to) == (from, to)) {
            return Err(WireError::Wired(wired.id));
        }
        let flow = Flow::new(self, self.live_wires().iter().copied());
        if flow.reaches(Point::port(to), Point::tap(from)) {
            return Err(WireError::Loop);
        }
        Ok(())
    }

    /// Whether the project has the ends of a wire from `from` to `to`, and
    /// the tap carries sound.
    fn check_ends(&self, from: Tap, to: Port) -> Result<(), WireError> {
        match from {
            Tap::PreFx(track) if self.track(track).is_some_and(|t| t.kind == TrackKind::Midi) => {
                return Err(WireError::Notes);
            }
            _ if self.tap_chain(from).is_none() => return Err(WireError::NoTap),
            _ => {}
        }
        if !self.has_port(to) {
            return Err(WireError::NoPort);
        }
        Ok(())
    }

    /// The wires the engine plays, in the order the project lists them:
    /// each whose ends the project has and whose tap carries sound, unless
    /// it would close a loop with the outputs, the sends and the wires
    /// before it. That's all of them in a project the commands edit, which
    /// refuse a wire that would make a loop.
    pub fn live_wires(&self) -> Vec<&Wire> {
        let mut flow = Flow::new(self, []);
        let mut live = Vec::with_capacity(self.wires.len());
        for wire in &self.wires {
            let (from, to) = (Point::tap(wire.from), Point::port(wire.to));
            if self.check_ends(wire.from, wire.to).is_ok() && !flow.reaches(to, from) {
                flow.add(from, to);
                live.push(wire);
            }
        }
        live
    }

    /// The first wire, in the order the project lists them, that closes a
    /// loop with the outputs, the sends and the wires before it, if there's
    /// a loop. Outputs and sends can't make one on their own, so any loop
    /// runs through a wire. An edit that moves a device, an output or a
    /// send checks it, to take itself back if it made a loop.
    pub fn wire_in_a_loop(&self) -> Option<WireId> {
        let mut flow = Flow::new(self, []);
        for wire in &self.wires {
            if self.check_ends(wire.from, wire.to).is_err() {
                continue;
            }
            let (from, to) = (Point::tap(wire.from), Point::port(wire.to));
            if flow.reaches(to, from) {
                return Some(wire.id);
            }
            flow.add(from, to);
        }
        None
    }

    /// The wires to and from the track with id `track`: from its taps and
    /// its devices', and into its devices' ports, in the order the project
    /// lists them.
    pub fn wires_of_track(&self, track: TrackId) -> Vec<WireId> {
        let chain = Some(Chain::Track(track));
        let on_track = |tap: Tap| match tap {
            Tap::PreFx(t) => t == track,
            Tap::Device(_) | Tap::PostFx(_) | Tap::PostFader(_) => self.tap_chain(tap) == chain,
        };
        (self.wires.iter())
            .filter(|w| on_track(w.from) || self.port_chain(w.to) == chain)
            .map(|w| w.id)
            .collect()
    }

    /// The wires to and from the device with id `device`, in the order the
    /// project lists them.
    pub fn wires_of_device(&self, device: DeviceId) -> Vec<WireId> {
        (self.wires.iter())
            .filter(|w| w.touches(device))
            .map(|w| w.id)
            .collect()
    }
}

/// A point on a channel: a tap, or where what feeds the master sums.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Point {
    /// Where a channel's signal starts: a track's Pre FX, or what feeds
    /// the master.
    Head(Chain),
    Device(DeviceId),
    PostFx(Chain),
    PostFader(Chain),
}

impl Point {
    fn tap(tap: Tap) -> Self {
        match tap {
            Tap::PreFx(track) => Self::Head(Chain::Track(track)),
            Tap::Device(device) => Self::Device(device),
            Tap::PostFx(chain) => Self::PostFx(chain),
            Tap::PostFader(chain) => Self::PostFader(chain),
        }
    }

    /// The point a port's signal flows into: what its device puts out.
    fn port(port: Port) -> Self {
        Self::Device(port.device())
    }
}

/// Where the signal flows, point by point.
struct Flow {
    next: HashMap<Point, Vec<Point>>,
}

impl Flow {
    /// The project's channels, from their heads through their devices to
    /// Post Fader, its tracks' outputs and sends as the engine plays them,
    /// and `wires`.
    fn new<'a>(project: &Project, wires: impl IntoIterator<Item = &'a Wire>) -> Self {
        let mut flow = Self {
            next: HashMap::new(),
        };
        for (chain, devices) in project.chains() {
            let mut at = Point::Head(chain);
            for device in devices {
                flow.add(at, Point::Device(device.id));
                at = Point::Device(device.id);
            }
            flow.add(at, Point::PostFx(chain));
            flow.add(Point::PostFx(chain), Point::PostFader(chain));
        }
        for track in &project.tracks {
            let chain = Chain::Track(track.id);
            let bus = match project.output_of(track) {
                Bus::Master => Chain::Master,
                Bus::Group(group) => Chain::Track(group),
            };
            flow.add(Point::PostFader(chain), Point::Head(bus));
            for send in project.sends_of(track) {
                let from = if send.pre_fader {
                    Point::PostFx(chain)
                } else {
                    Point::PostFader(chain)
                };
                flow.add(from, Point::Head(Chain::Track(send.to)));
            }
        }
        for wire in wires {
            flow.add(Point::tap(wire.from), Point::port(wire.to));
        }
        flow
    }

    fn add(&mut self, from: Point, to: Point) {
        self.next.entry(from).or_default().push(to);
    }

    /// Whether signal at `start` flows on to `target`. A point reaches
    /// itself.
    fn reaches(&self, start: Point, target: Point) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![start];
        while let Some(point) = stack.pop() {
            if point == target {
                return true;
            }
            if seen.insert(point) {
                stack.extend(self.next.get(&point).into_iter().flatten());
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::NativeDevice;
    use crate::plugin::{PluginDevice, PluginInput, PluginSettings};
    use crate::{Device, PluginFormat, TrackKind};

    /// A kick, and a bass with a compressor and an EQ after it, and a
    /// group with a reverb return.
    struct Mix {
        project: Project,
        kick: TrackId,
        bass: TrackId,
        group: TrackId,
        reverb: TrackId,
        compressor: DeviceId,
        eq: DeviceId,
    }

    fn device(project: &mut Project, kind: NativeKind) -> Device {
        project.new_device(DeviceKind::Native(NativeDevice::new(kind)))
    }

    fn mix() -> Mix {
        let mut project = Project::new();
        let kick = project.add_track("Kick", TrackKind::Audio);
        let bass = project.add_track("Bass", TrackKind::Audio);
        let group = project.add_track("Group", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        let compressor = device(&mut project, NativeKind::Compressor);
        let eq = device(&mut project, NativeKind::Eq);
        let (compressor_id, eq_id) = (compressor.id, eq.id);
        project.track_mut(bass).unwrap().devices = vec![compressor, eq];
        Mix {
            project,
            kick,
            bass,
            group,
            reverb,
            compressor: compressor_id,
            eq: eq_id,
        }
    }

    impl Mix {
        fn wire(&mut self, from: Tap, to: Port) -> WireId {
            assert_eq!(self.project.check_wire(from, to), Ok(()));
            let wire = self.project.new_wire(from, to);
            self.project.wires.push(wire);
            wire.id
        }
    }

    #[test]
    fn a_track_keys_another_from_any_of_its_taps() {
        let m = mix();
        let key = Port::Sidechain(m.compressor);
        for tap in m.project.taps(Chain::Track(m.kick)) {
            assert_eq!(m.project.check_wire(tap, key), Ok(()), "{tap:?}");
        }
        assert_eq!(
            m.project.taps(Chain::Master),
            [Tap::PostFx(Chain::Master), Tap::PostFader(Chain::Master)]
        );
    }

    #[test]
    fn taps_and_ports_have_names() {
        let m = mix();
        let names: Vec<String> = (m.project.taps(Chain::Track(m.bass)).into_iter())
            .filter_map(|tap| m.project.tap_name(tap))
            .collect();
        assert_eq!(
            names,
            [
                "Bass Pre FX",
                "Bass after Compressor",
                "Bass after EQ",
                "Bass Post FX",
                "Bass Post Fader"
            ]
        );
        let master = m.project.tap_name(Tap::PostFader(Chain::Master));
        assert_eq!(master.as_deref(), Some("Master Post Fader"));
        let key = Port::Sidechain(m.compressor);
        assert_eq!(
            m.project.port_name(key).as_deref(),
            Some("Compressor on Bass")
        );
        assert_eq!(m.project.port_name(key.on(DeviceId(99))), None);
        assert_eq!(key.on(m.eq), Port::Sidechain(m.eq));
    }

    #[test]
    fn only_compressors_and_gates_have_sidechains() {
        let mut m = mix();
        let gate = device(&mut m.project, NativeKind::Gate);
        let gate_id = gate.id;
        m.project.master.devices.push(gate);
        assert_eq!(
            m.project.ports(gate_id),
            [Port::Sidechain(gate_id)],
            "on the master too"
        );
        assert!(m.project.ports(m.eq).is_empty());
        let from = Tap::PostFx(Chain::Track(m.kick));
        assert_eq!(
            m.project.check_wire(from, Port::Sidechain(m.eq)),
            Err(WireError::NoPort)
        );
        assert_eq!(
            m.project.check_wire(from, Port::Sidechain(DeviceId(99))),
            Err(WireError::NoPort)
        );
        assert_eq!(
            m.project.check_wire(
                Tap::PostFx(Chain::Track(TrackId(99))),
                Port::Sidechain(m.compressor)
            ),
            Err(WireError::NoTap)
        );
    }

    #[test]
    fn a_midi_tracks_pre_fx_is_notes() {
        let mut m = mix();
        let keys = m.project.add_track("Keys", TrackKind::Midi);
        let key = Port::Sidechain(m.compressor);
        assert_eq!(
            m.project.check_wire(Tap::PreFx(keys), key),
            Err(WireError::Notes)
        );
        assert_eq!(
            m.project.check_wire(Tap::PostFx(Chain::Track(keys)), key),
            Ok(())
        );
    }

    #[test]
    fn a_tap_wires_to_a_port_once() {
        let mut m = mix();
        let (from, to) = (
            Tap::PostFx(Chain::Track(m.kick)),
            Port::Sidechain(m.compressor),
        );
        let wire = m.wire(from, to);
        assert_eq!(m.project.check_wire(from, to), Err(WireError::Wired(wire)));
        // Another source sums with it.
        m.wire(Tap::PreFx(m.kick), to);
    }

    #[test]
    fn keys_from_later_in_the_signal_would_loop() {
        let m = mix();
        let key = Port::Sidechain(m.compressor);
        let bass = Chain::Track(m.bass);
        // From before it in its own chain: no loop.
        assert_eq!(m.project.check_wire(Tap::PreFx(m.bass), key), Ok(()));
        // From itself, after it, or the master everything feeds: loops.
        for tap in [
            Tap::Device(m.compressor),
            Tap::Device(m.eq),
            Tap::PostFx(bass),
            Tap::PostFader(bass),
            Tap::PostFx(Chain::Master),
            Tap::PostFader(Chain::Master),
        ] {
            assert_eq!(
                m.project.check_wire(tap, key),
                Err(WireError::Loop),
                "{tap:?}"
            );
        }
    }

    #[test]
    fn loops_through_groups_and_sends_are_refused() {
        let mut m = mix();
        let key = Port::Sidechain(m.compressor);
        let group = Chain::Track(m.group);
        let reverb = Chain::Track(m.reverb);
        // Not yet in the bass's signal path.
        assert_eq!(m.project.check_wire(Tap::PostFx(group), key), Ok(()));
        assert_eq!(m.project.check_wire(Tap::PostFx(reverb), key), Ok(()));
        let send = m.project.new_send(m.reverb);
        let bass = m.project.track_mut(m.bass).unwrap();
        bass.output = Bus::Group(m.group);
        bass.sends.push(send);
        assert_eq!(
            m.project.check_wire(Tap::PostFx(group), key),
            Err(WireError::Loop)
        );
        assert_eq!(
            m.project.check_wire(Tap::PostFx(reverb), key),
            Err(WireError::Loop)
        );
        assert_eq!(
            m.project.check_wire(Tap::PreFx(m.reverb), key),
            Err(WireError::Loop)
        );
    }

    #[test]
    fn two_tracks_key_each_other_from_their_pre_fx() {
        let mut m = mix();
        let gate = device(&mut m.project, NativeKind::Gate);
        let gate_id = gate.id;
        m.project.track_mut(m.kick).unwrap().devices.push(gate);
        m.wire(Tap::PreFx(m.kick), Port::Sidechain(m.compressor));
        m.wire(Tap::PreFx(m.bass), Port::Sidechain(gate_id));
        // And one from after the other's device, but then not back.
        m.wire(Tap::Device(m.compressor), Port::Sidechain(gate_id));
        assert_eq!(
            m.project
                .check_wire(Tap::Device(gate_id), Port::Sidechain(m.compressor)),
            Err(WireError::Loop)
        );
        assert_eq!(m.project.wire_in_a_loop(), None);
        assert_eq!(m.project.live_wires().len(), 3);
    }

    #[test]
    fn an_output_that_closes_a_loop_finds_the_wire_in_it() {
        let mut m = mix();
        let key = m.wire(
            Tap::PostFx(Chain::Track(m.group)),
            Port::Sidechain(m.compressor),
        );
        let other = m.wire(Tap::PreFx(m.kick), Port::Sidechain(m.compressor));
        assert_eq!(m.project.wire_in_a_loop(), None);
        // The bass feeding the group closes the loop through the key.
        m.project.track_mut(m.bass).unwrap().output = Bus::Group(m.group);
        assert_eq!(m.project.wire_in_a_loop(), Some(key));
        // The engine leaves it out, and plays the rest.
        let live: Vec<WireId> = m.project.live_wires().iter().map(|w| w.id).collect();
        assert_eq!(live, [other]);
    }

    #[test]
    fn moving_a_device_past_its_key_closes_a_loop() {
        let mut m = mix();
        let gate = device(&mut m.project, NativeKind::Gate);
        let gate_id = gate.id;
        m.project.track_mut(m.bass).unwrap().devices.push(gate);
        // The gate, last, keyed from the compressor, first.
        let key = m.wire(Tap::Device(m.compressor), Port::Sidechain(gate_id));
        let devices = &mut m.project.track_mut(m.bass).unwrap().devices;
        let gate = devices.pop().unwrap();
        devices.insert(0, gate);
        assert_eq!(m.project.wire_in_a_loop(), Some(key));
    }

    #[test]
    fn wires_belong_to_the_tracks_and_devices_at_their_ends() {
        let mut m = mix();
        let gate = device(&mut m.project, NativeKind::Gate);
        let gate_id = gate.id;
        m.project.track_mut(m.kick).unwrap().devices.push(gate);
        // From before the kick's gate, so the gate can key from the bass.
        let kick_to_bass = m.wire(Tap::PreFx(m.kick), Port::Sidechain(m.compressor));
        let bass_to_kick = m.wire(Tap::Device(m.eq), Port::Sidechain(gate_id));
        let group_to_bass = m.wire(
            Tap::PostFader(Chain::Track(m.group)),
            Port::Sidechain(m.compressor),
        );
        assert_eq!(
            m.project.wires_of_track(m.kick),
            [kick_to_bass, bass_to_kick]
        );
        assert_eq!(
            m.project.wires_of_track(m.bass),
            [kick_to_bass, bass_to_kick, group_to_bass]
        );
        assert_eq!(m.project.wires_of_track(m.group), [group_to_bass]);
        assert_eq!(m.project.wires_of_device(m.eq), [bass_to_kick]);
        assert_eq!(
            m.project.wires_of_device(m.compressor),
            [kick_to_bass, group_to_bass]
        );
        assert_eq!(
            m.project
                .wires_into(Port::Sidechain(m.compressor))
                .map(|w| w.id)
                .collect::<Vec<_>>(),
            [kick_to_bass, group_to_bass]
        );
    }

    #[test]
    fn wires_with_missing_ends_dont_play() {
        let mut m = mix();
        let key = m.wire(
            Tap::PostFx(Chain::Track(m.kick)),
            Port::Sidechain(m.compressor),
        );
        assert_eq!(m.project.live_wires().len(), 1);
        m.project.tracks.retain(|t| t.id != m.kick);
        assert!(m.project.live_wires().is_empty());
        assert_eq!(m.project.wire_in_a_loop(), None);
        assert_eq!(m.project.wire(key).map(|w| w.gain), Some(1.0));
    }

    #[test]
    fn plugins_side_inputs_are_ports() {
        let mut m = mix();
        let input = |id, name: &str| PluginInput {
            id,
            name: name.into(),
        };
        let vocoder = m.project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Vst3,
            id: "0123456789ABCDEF0123456789ABCDEF".into(),
            name: "Vocoder".into(),
            instrument: false,
            inputs: vec![input(1, "Carrier"), input(3, "Aux")],
            settings: PluginSettings::default(),
        }));
        let vocoder_id = vocoder.id;
        m.project.track_mut(m.bass).unwrap().devices.push(vocoder);
        let carrier = Port::PluginInput(vocoder_id, 1);
        assert_eq!(
            m.project.ports(vocoder_id),
            [carrier, Port::PluginInput(vocoder_id, 3)],
            "in the plugin's order"
        );
        assert_eq!(m.project.input_name(carrier), Some("Carrier"));
        assert_eq!(
            m.project.port_name(carrier).as_deref(),
            Some("Vocoder Carrier on Bass")
        );
        assert_eq!(carrier.on(m.eq), Port::PluginInput(m.eq, 1));
        let compressor = Port::Sidechain(m.compressor);
        assert_eq!(m.project.input_name(compressor), Some("Sidechain"));
        for missing in [
            Port::PluginInput(vocoder_id, 2),
            Port::Sidechain(vocoder_id),
            Port::PluginInput(m.compressor, 1),
            Port::Sidechain(m.eq),
        ] {
            assert_eq!(m.project.input_name(missing), None, "{missing:?}");
            let from = Tap::PostFx(Chain::Track(m.kick));
            assert_eq!(m.project.check_wire(from, missing), Err(WireError::NoPort));
        }
        // A side input the plugin hasn't has no name, but a device without
        // a Sidechain is still the device.
        assert_eq!(m.project.port_name(Port::PluginInput(vocoder_id, 2)), None);
        assert_eq!(
            m.project.port_name(Port::Sidechain(m.eq)).as_deref(),
            Some("EQ on Bass")
        );
        m.wire(Tap::PostFx(Chain::Track(m.kick)), carrier);
        // What comes out of the vocoder reaches the bass's Post FX, but not
        // what goes into it.
        assert_eq!(
            m.project
                .check_wire(Tap::PostFx(Chain::Track(m.bass)), carrier),
            Err(WireError::Loop)
        );
        assert_eq!(m.project.check_wire(Tap::Device(m.eq), carrier), Ok(()));
        let vocal = m.project.add_track("Vocal", TrackKind::Audio);
        m.wire(Tap::PostFader(Chain::Track(vocal)), carrier);
        assert_eq!(m.project.wires_into(carrier).count(), 2);
        assert_eq!(m.project.wires_of_device(vocoder_id).len(), 2);
        // The commands keep an input while wires go into it, but if the
        // plugin's list loses it, its wires don't play.
        let device = m.project.device_mut(vocoder_id).unwrap();
        device.kind.plugin_mut().unwrap().inputs = vec![input(3, "Aux")];
        assert!(m.project.live_wires().is_empty());
        assert_eq!(m.project.wire_in_a_loop(), None);
        // One the plugin names nothing still has a name.
        let device = m.project.device_mut(vocoder_id).unwrap();
        device.kind.plugin_mut().unwrap().inputs.push(input(5, " "));
        let unnamed = Port::PluginInput(vocoder_id, 5);
        assert_eq!(m.project.input_name(unnamed), Some("Side Input"));
        assert_eq!(
            m.project.port_name(unnamed).as_deref(),
            Some("Vocoder Side Input on Bass")
        );
    }
}
