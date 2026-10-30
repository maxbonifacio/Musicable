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
//! A track's input can be a tap on another track too ([`Input::Tap`]): an
//! audio track records and monitors another's sound, and a MIDI track
//! another MIDI track's notes, at its Pre FX. The master's Post Fader is
//! [Resampling](Input::RESAMPLING), which a track records but never plays.
//!
//! Signal never flows in a loop. Outputs and sends can't make one on their
//! own (see [`mixer`](crate::mixer)), but an input or a wire can close one
//! with them, such as a compressor keyed from the group its own track
//! feeds, or from a device after it, or a track taking its input from a
//! group it feeds. The check follows points, not tracks, so two tracks can
//! key each other from their Pre FX. [`Project::check_wire`] and
//! [`Project::check_input`] say whether a wire or an input can join the
//! project, and [`Project::route_in_a_loop`] finds the route that closes a
//! loop, after an edit that moves devices, outputs or sends.

use std::collections::{HashMap, HashSet};

use crate::mixer::Bus;
use crate::native::NativeKind;
use crate::{Chain, DeviceId, DeviceKind, Input, Project, TrackId, TrackKind, next_id};

/// Stable identifier for a wire, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WireId(pub u32);

/// A point on a channel where routing can take its signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tap {
    /// What comes into a track's devices: its clips and its input, or on a
    /// group or a return track, what feeds it. A MIDI track's is its
    /// notes, which only another MIDI track's input takes. The master has
    /// none.
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
    /// The tap carries notes alone: a MIDI track's Pre FX, or the point
    /// after one of its MIDI effects.
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

/// Why a track can't take its input from a tap: see
/// [`Project::check_input`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    /// The project hasn't the track.
    NoTrack,
    /// The project hasn't the tap's track or device.
    NoTap,
    /// The track is a group or a return track, which takes what feeds it,
    /// not an input.
    Bus,
    /// The tap carries notes alone, a MIDI track's Pre FX or the point
    /// after one of its MIDI effects, and the track is an audio track.
    Notes,
    /// The tap carries sound alone, and the track is a MIDI track, which
    /// takes another MIDI track's notes alone.
    Sound,
    /// The tap is on the master, but not its Post Fader, which is the
    /// master's one tap for an input: Resampling.
    Master,
    /// Signal from the track reaches the tap already, so the input would
    /// close a loop.
    Loop,
}

/// A route between channels that can close a loop with the outputs and
/// the sends: a track's input from a tap, or a wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// The input of the track with this id.
    Input(TrackId),
    Wire(WireId),
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
        if self
            .live(None)
            .flow
            .reaches(Point::port(to), Point::tap(from))
        {
            return Err(WireError::Loop);
        }
        Ok(())
    }

    /// Whether `tap` carries notes, for a MIDI track to take: a MIDI
    /// track's Pre FX, its clips' notes and its input's, the point after
    /// one of its MIDI effects, what that plays, and its Post FX, what
    /// reaches its instrument, after them all.
    pub fn carries_notes(&self, tap: Tap) -> bool {
        let midi = |track| self.track(track).is_some_and(|t| t.kind == TrackKind::Midi);
        match tap {
            Tap::PreFx(track) | Tap::PostFx(Chain::Track(track)) => midi(track),
            Tap::Device(device) => self.is_midi_effect_tap(device),
            Tap::PostFx(Chain::Master) | Tap::PostFader(_) => false,
        }
    }

    /// Whether `tap` carries sound, for an audio track or a wire to take:
    /// every tap but those that carry notes alone, a MIDI track's Pre FX
    /// and the points after its MIDI effects. A MIDI track's Post FX
    /// carries both: what comes out of its last device is sound.
    pub fn carries_sound(&self, tap: Tap) -> bool {
        match tap {
            Tap::PreFx(track) => self.track(track).is_none_or(|t| t.kind != TrackKind::Midi),
            Tap::Device(device) => !self.is_midi_effect_tap(device),
            Tap::PostFx(_) | Tap::PostFader(_) => true,
        }
    }

    /// Whether the device with id `device` is a MIDI effect the notes
    /// reach, on a MIDI track before its first instrument, so the point
    /// after it carries the notes it plays. One they don't reach is out of
    /// their way, as if deactivated, and the point after it carries what
    /// comes into it.
    fn is_midi_effect_tap(&self, device: DeviceId) -> bool {
        self.find_device(device)
            .is_some_and(|(chain, index, found)| {
                let devices = self.devices(chain).unwrap_or_default();
                found.kind.is_midi_effect()
                    && self.has_notes(chain)
                    && !devices[..index].iter().any(|d| d.kind.is_instrument())
            })
    }

    /// Whether the project has the ends of a wire from `from` to `to`, and
    /// the tap carries sound.
    fn check_ends(&self, from: Tap, to: Port) -> Result<(), WireError> {
        if !self.carries_sound(from) {
            return Err(WireError::Notes);
        }
        if self.tap_chain(from).is_none() {
            return Err(WireError::NoTap);
        }
        if !self.has_port(to) {
            return Err(WireError::NoPort);
        }
        Ok(())
    }

    /// Whether the track with id `track` can take its input from `tap`:
    /// see [`check_input`](Self::check_input).
    pub fn can_take(&self, track: TrackId, tap: Tap) -> bool {
        self.check_input(track, tap).is_ok()
    }

    /// Whether the track with id `track` can take its input from `tap`, in
    /// place of the input it has, and if not, why not. An audio track takes
    /// sound ([`carries_sound`](Self::carries_sound)) from any tap on another
    /// track, or the master's Post Fader, which is
    /// [Resampling](Input::RESAMPLING). A MIDI track takes another MIDI
    /// track's notes ([`carries_notes`](Self::carries_notes)). Group and
    /// return tracks take no input. The signal from the track can't reach
    /// the tap, so the input makes no loop, but for Resampling, which the
    /// track records and never plays.
    pub fn check_input(&self, track: TrackId, tap: Tap) -> Result<(), InputError> {
        self.check_input_ends(track, tap)?;
        if Input::Tap(tap).is_resampling() {
            return Ok(());
        }
        let head = Point::Head(Chain::Track(track));
        let from = self.input_point(track, tap);
        if self.live(Some(track)).flow.reaches(head, from) {
            return Err(InputError::Loop);
        }
        Ok(())
    }

    /// Where the input of the track with id `track` from `tap` takes its
    /// signal in the flow: for a MIDI track's notes from another's Post
    /// FX, what reaches its instrument, after its last MIDI effect, rather
    /// than what comes out of its last device.
    fn input_point(&self, track: TrackId, tap: Tap) -> Point {
        let notes = self.track(track).is_some_and(|t| t.kind == TrackKind::Midi);
        match tap {
            Tap::PostFx(Chain::Track(on)) if notes => {
                let devices = self.track(on).map_or(&[][..], |t| &t.devices);
                (devices.iter())
                    .take_while(|device| !device.kind.is_instrument())
                    .filter(|device| device.kind.is_midi_effect())
                    .last()
                    .map_or(Point::Head(Chain::Track(on)), |effect| {
                        Point::Device(effect.id)
                    })
            }
            _ => Point::tap(tap),
        }
    }

    /// Whether the project has the track with id `track` and `tap`, and the
    /// tap carries what the track takes.
    fn check_input_ends(&self, track: TrackId, tap: Tap) -> Result<(), InputError> {
        let taker = self.track(track).ok_or(InputError::NoTrack)?;
        if taker.kind.is_bus() {
            return Err(InputError::Bus);
        }
        let chain = self.tap_chain(tap).ok_or(InputError::NoTap)?;
        match taker.kind {
            TrackKind::Midi if !self.carries_notes(tap) => Err(InputError::Sound),
            TrackKind::Midi => Ok(()),
            _ if !self.carries_sound(tap) => Err(InputError::Notes),
            _ if chain == Chain::Master && tap != Tap::PostFader(Chain::Master) => {
                Err(InputError::Master)
            }
            _ => Ok(()),
        }
    }

    /// The inputs from taps the engine plays, as each track with one and
    /// its tap, in track order: each whose ends the project has and whose
    /// tap carries what the track takes, unless it would close a loop with
    /// the outputs, the sends and the inputs before it. Resampling never
    /// closes one: the track records it but never plays it. That's all of
    /// them in a project the commands edit, which refuse an input that
    /// would make a loop.
    pub fn live_inputs(&self) -> Vec<(TrackId, Tap)> {
        self.live(None).inputs
    }

    /// The wires the engine plays, in the order the project lists them:
    /// each whose ends the project has and whose tap carries sound, unless
    /// it would close a loop with the outputs, the sends, the inputs the
    /// engine plays and the wires before it. That's all of them in a
    /// project the commands edit, which refuse a wire that would make a
    /// loop.
    pub fn live_wires(&self) -> Vec<&Wire> {
        self.live(None).wires
    }

    /// The first route that closes a loop with the outputs, the sends and
    /// the routes before it, if there's a loop: the tracks' inputs from
    /// taps first, in track order, then the wires, in the order the project
    /// lists them. Outputs and sends can't make one on their own, so any
    /// loop runs through an input or a wire. An edit that moves a device,
    /// an output or a send checks it, to take itself back if it made a
    /// loop.
    pub fn route_in_a_loop(&self) -> Option<Route> {
        self.live(None).in_a_loop
    }

    /// The inputs from taps and the wires the engine plays, with the flow
    /// through them, as [`live_inputs`](Self::live_inputs) and
    /// [`live_wires`](Self::live_wires) take them, and the first route that
    /// closes a loop, as [`route_in_a_loop`](Self::route_in_a_loop) finds
    /// it. Without the input of `skip`, the track whose input is checked,
    /// if there is one.
    fn live(&self, skip: Option<TrackId>) -> Live<'_> {
        let mut flow = Flow::new(self);
        let mut live = Live::default();
        for track in self.tracks.iter().filter(|t| Some(t.id) != skip) {
            let Some(tap) = track.input.as_ref().and_then(Input::tap) else {
                continue;
            };
            if self.check_input_ends(track.id, tap).is_err() {
                continue;
            }
            let from = self.input_point(track.id, tap);
            let to = Point::Head(Chain::Track(track.id));
            if Input::Tap(tap).is_resampling() {
                live.inputs.push((track.id, tap));
            } else if flow.reaches(to, from) {
                live.in_a_loop.get_or_insert(Route::Input(track.id));
            } else {
                flow.add(from, to);
                live.inputs.push((track.id, tap));
            }
        }
        for wire in &self.wires {
            if self.check_ends(wire.from, wire.to).is_err() {
                continue;
            }
            let (from, to) = (Point::tap(wire.from), Point::port(wire.to));
            if flow.reaches(to, from) {
                live.in_a_loop.get_or_insert(Route::Wire(wire.id));
            } else {
                flow.add(from, to);
                live.wires.push(wire);
            }
        }
        live.flow = flow;
        live
    }

    /// The tracks that take their input from a tap on the track with id
    /// `track`, or on one of its devices, in track order.
    pub fn inputs_from_track(&self, track: TrackId) -> Vec<TrackId> {
        let chain = Some(Chain::Track(track));
        let on_track = |tap: Tap| match tap {
            Tap::PreFx(t) => t == track,
            Tap::Device(_) | Tap::PostFx(_) | Tap::PostFader(_) => self.tap_chain(tap) == chain,
        };
        self.tracks_taking(on_track)
    }

    /// The tracks that take their input from what the device with id
    /// `device` puts out, in track order.
    pub fn inputs_from_device(&self, device: DeviceId) -> Vec<TrackId> {
        self.tracks_taking(|tap| tap == Tap::Device(device))
    }

    /// The tracks whose input is a tap where `taken` holds, in track order.
    fn tracks_taking(&self, taken: impl Fn(Tap) -> bool) -> Vec<TrackId> {
        (self.tracks.iter())
            .filter(|t| t.input.as_ref().and_then(Input::tap).is_some_and(&taken))
            .map(|t| t.id)
            .collect()
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
#[derive(Default)]
struct Flow {
    next: HashMap<Point, Vec<Point>>,
}

/// What [`Project::live`] finds.
#[derive(Default)]
struct Live<'a> {
    flow: Flow,
    inputs: Vec<(TrackId, Tap)>,
    wires: Vec<&'a Wire>,
    in_a_loop: Option<Route>,
}

impl Flow {
    /// The project's channels, from their heads through their devices to
    /// Post Fader, and its tracks' outputs and sends as the engine plays
    /// them.
    fn new(project: &Project) -> Self {
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
                Bus::Master => Some(Chain::Master),
                Bus::Group(group) => Some(Chain::Track(group)),
                Bus::SendsOnly => None,
            };
            if let Some(bus) = bus {
                flow.add(Point::PostFader(chain), Point::Head(bus));
            }
            for send in project.sends_of(track) {
                let from = if send.pre_fader {
                    Point::PostFx(chain)
                } else {
                    Point::PostFader(chain)
                };
                flow.add(from, Point::Head(Chain::Track(send.to)));
            }
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
    use crate::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings};
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

        /// Has the track take its input from `tap`.
        fn take(&mut self, track: TrackId, tap: Tap) {
            assert_eq!(self.project.check_input(track, tap), Ok(()), "{tap:?}");
            self.project.track_mut(track).unwrap().input = Some(Input::Tap(tap));
        }

        fn input(&self, track: TrackId, tap: Tap) -> Result<(), InputError> {
            self.project.check_input(track, tap)
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
    fn a_midi_tracks_pre_fx_is_notes_and_its_post_fx_both() {
        let mut m = mix();
        let keys = m.project.add_track("Keys", TrackKind::Midi);
        let (pre, post) = (Tap::PreFx(keys), Tap::PostFx(Chain::Track(keys)));
        assert!(m.project.carries_notes(pre) && !m.project.carries_sound(pre));
        // What reaches its instrument, and what comes out of its devices.
        assert!(m.project.carries_notes(post) && m.project.carries_sound(post));
        let kick = Tap::PreFx(m.kick);
        assert!(!m.project.carries_notes(kick) && m.project.carries_sound(kick));
        let key = Port::Sidechain(m.compressor);
        assert_eq!(m.project.check_wire(pre, key), Err(WireError::Notes));
        assert_eq!(m.project.check_wire(post, key), Ok(()));
    }

    /// A MIDI track, Keys, with an arpeggiator before its synth and a
    /// compressor after it, as `mix` has them, and their ids.
    fn arpeggiated(m: &mut Mix) -> (TrackId, [DeviceId; 3]) {
        let keys = m.project.add_track("Keys", TrackKind::Midi);
        let plugin = |m: &mut Mix, name: &str, kind| {
            m.project.new_device(DeviceKind::Plugin(PluginDevice {
                format: PluginFormat::Clap,
                id: format!("org.example.{name}"),
                name: name.into(),
                kind,
                inputs: Vec::new(),
                settings: PluginSettings::default(),
            }))
        };
        let arp = plugin(m, "arp", PluginKind::MidiEffect);
        let synth = plugin(m, "synth", PluginKind::Instrument);
        let compressor = device(&mut m.project, NativeKind::Compressor);
        let ids = [arp.id, synth.id, compressor.id];
        m.project.track_mut(keys).unwrap().devices = vec![arp, synth, compressor];
        (keys, ids)
    }

    #[test]
    fn the_points_after_a_midi_tracks_midi_effects_are_notes() {
        let mut m = mix();
        let (keys, [arp, synth, _]) = arpeggiated(&mut m);
        let pad = m.project.add_track("Pad", TrackKind::Midi);
        let (after_arp, after_synth) = (Tap::Device(arp), Tap::Device(synth));
        assert!(m.project.carries_notes(after_arp) && !m.project.carries_sound(after_arp));
        assert!(!m.project.carries_notes(after_synth) && m.project.carries_sound(after_synth));
        // A MIDI track takes what the arpeggiator plays, and an audio track
        // or a wire what the synth does.
        assert_eq!(m.input(pad, after_arp), Ok(()));
        assert_eq!(m.input(pad, after_synth), Err(InputError::Sound));
        assert_eq!(m.input(m.kick, after_arp), Err(InputError::Notes));
        assert_eq!(m.input(m.kick, after_synth), Ok(()));
        let key = Port::Sidechain(m.compressor);
        assert_eq!(m.project.check_wire(after_arp, key), Err(WireError::Notes));
        // Not round a loop.
        m.take(pad, after_arp);
        let pad_post = Tap::PostFx(Chain::Track(pad));
        assert_eq!(m.input(keys, pad_post), Err(InputError::Loop));
        assert_eq!(m.input(keys, Tap::PreFx(pad)), Err(InputError::Loop));
        // A file can have a MIDI effect after the instrument. No notes reach
        // it, so the point after it carries the synth's sound.
        m.project.track_mut(keys).unwrap().devices.swap(0, 1);
        assert!(!m.project.carries_notes(after_arp) && m.project.carries_sound(after_arp));
    }

    #[test]
    fn a_midi_tracks_notes_from_post_fx_are_what_reaches_its_instrument() {
        let mut m = mix();
        let (keys, [_, _, compressor]) = arpeggiated(&mut m);
        let pad = m.project.add_track("Pad", TrackKind::Midi);
        m.take(pad, Tap::PostFx(Chain::Track(keys)));
        // So the pad's sound can key the compressor after the synth that
        // plays the notes it takes: that makes no loop.
        let pad_out = Tap::PostFader(Chain::Track(pad));
        assert_eq!(
            m.project.check_wire(pad_out, Port::Sidechain(compressor)),
            Ok(())
        );
        // But an audio track taking the Keys' sound at Post FX can't key it.
        let print = m.project.add_track("Print", TrackKind::Audio);
        m.take(print, Tap::PostFx(Chain::Track(keys)));
        let print_out = Tap::PostFader(Chain::Track(print));
        assert_eq!(
            m.project.check_wire(print_out, Port::Sidechain(compressor)),
            Err(WireError::Loop)
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
        assert_eq!(m.project.route_in_a_loop(), None);
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
        assert_eq!(m.project.route_in_a_loop(), None);
        // The bass feeding the group closes the loop through the key.
        m.project.track_mut(m.bass).unwrap().output = Bus::Group(m.group);
        assert_eq!(m.project.route_in_a_loop(), Some(Route::Wire(key)));
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
        assert_eq!(m.project.route_in_a_loop(), Some(Route::Wire(key)));
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
        assert_eq!(m.project.route_in_a_loop(), None);
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
            kind: PluginKind::AudioEffect,
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
        assert_eq!(m.project.route_in_a_loop(), None);
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

    #[test]
    fn an_audio_track_takes_another_tracks_sound_at_any_tap() {
        let mut m = mix();
        let print = m.project.add_track("Print", TrackKind::Audio);
        let chains = [m.kick, m.bass, m.group, m.reverb].map(Chain::Track);
        for tap in chains.into_iter().flat_map(|chain| m.project.taps(chain)) {
            assert_eq!(m.input(print, tap), Ok(()), "{tap:?}");
        }
        // The master's Post Fader is Resampling, its one tap for an input.
        let resampling = Input::RESAMPLING.tap().unwrap();
        assert_eq!(m.input(print, resampling), Ok(()));
        assert_eq!(
            m.input(print, Tap::PostFx(Chain::Master)),
            Err(InputError::Master)
        );
        let limiter = device(&mut m.project, NativeKind::Compressor);
        let limiter_id = limiter.id;
        m.project.master.devices.push(limiter);
        assert_eq!(
            m.input(print, Tap::Device(limiter_id)),
            Err(InputError::Master)
        );
        let kick = Tap::PostFx(Chain::Track(m.kick));
        assert_eq!(
            m.input(print, Tap::PostFx(Chain::Track(TrackId(99)))),
            Err(InputError::NoTap)
        );
        assert_eq!(
            m.input(print, Tap::Device(DeviceId(99))),
            Err(InputError::NoTap)
        );
        assert_eq!(m.input(TrackId(99), kick), Err(InputError::NoTrack));
        // Buses take what feeds them.
        assert_eq!(m.input(m.group, kick), Err(InputError::Bus));
        assert_eq!(m.input(m.reverb, kick), Err(InputError::Bus));
        assert!(m.project.can_take(print, kick));
        assert!(!m.project.can_take(m.group, kick));
    }

    #[test]
    fn a_midi_track_takes_another_midi_tracks_notes() {
        let mut m = mix();
        let keys = m.project.add_track("Keys", TrackKind::Midi);
        let pad = m.project.add_track("Pad", TrackKind::Midi);
        assert_eq!(m.input(pad, Tap::PreFx(keys)), Ok(()));
        // What reaches its instrument.
        assert_eq!(m.input(pad, Tap::PostFx(Chain::Track(keys))), Ok(()));
        // Sound is for audio tracks.
        for tap in [
            Tap::PostFader(Chain::Track(keys)),
            Tap::PreFx(m.kick),
            Input::RESAMPLING.tap().unwrap(),
        ] {
            assert_eq!(m.input(pad, tap), Err(InputError::Sound), "{tap:?}");
        }
        assert_eq!(m.input(m.kick, Tap::PreFx(keys)), Err(InputError::Notes));
        assert_eq!(m.input(m.kick, Tap::PostFx(Chain::Track(keys))), Ok(()));
        // Not from itself, nor round a loop.
        assert_eq!(m.input(keys, Tap::PreFx(keys)), Err(InputError::Loop));
        m.take(pad, Tap::PreFx(keys));
        assert_eq!(m.input(keys, Tap::PreFx(pad)), Err(InputError::Loop));
        assert_eq!(m.project.live_inputs(), [(pad, Tap::PreFx(keys))]);
    }

    #[test]
    fn inputs_that_would_loop_are_refused() {
        let mut m = mix();
        let kick = Chain::Track(m.kick);
        // From itself.
        for tap in m.project.taps(kick) {
            assert_eq!(m.input(m.kick, tap), Err(InputError::Loop), "{tap:?}");
        }
        // From the group it feeds and the return it sends to.
        let send = m.project.new_send(m.reverb);
        let track = m.project.track_mut(m.kick).unwrap();
        track.output = Bus::Group(m.group);
        track.sends.push(send);
        for chain in [m.group, m.reverb].map(Chain::Track) {
            for tap in m.project.taps(chain) {
                assert_eq!(m.input(m.kick, tap), Err(InputError::Loop), "{tap:?}");
            }
        }
        // From a track that takes its own sound.
        m.take(m.bass, Tap::PostFx(kick));
        let bass = Chain::Track(m.bass);
        assert_eq!(m.input(m.kick, Tap::PostFx(bass)), Err(InputError::Loop));
        assert_eq!(m.input(m.kick, Tap::PreFx(m.bass)), Err(InputError::Loop));
        // Replacing the bass's input, which the check leaves out.
        m.take(m.bass, Tap::PreFx(m.kick));
        // Round a wire: a print of the bass can't key the bass's compressor,
        // nor can a track take what comes out of a compressor it keys.
        let print = m.project.add_track("Print", TrackKind::Audio);
        let key = Port::Sidechain(m.compressor);
        m.take(print, Tap::PostFx(bass));
        let from = Tap::PostFader(Chain::Track(print));
        assert_eq!(m.project.check_wire(from, key), Err(WireError::Loop));
        let vocal = m.project.add_track("Vocal", TrackKind::Audio);
        m.wire(Tap::PostFx(Chain::Track(vocal)), key);
        assert_eq!(
            m.input(vocal, Tap::Device(m.compressor)),
            Err(InputError::Loop)
        );
        assert_eq!(m.project.route_in_a_loop(), None);
    }

    #[test]
    fn resampling_takes_the_master_that_the_track_feeds() {
        let mut m = mix();
        let resampling = Input::RESAMPLING.tap().unwrap();
        m.take(m.kick, resampling);
        // The track records it but never plays it, so it closes no loop.
        assert_eq!(m.project.live_inputs(), [(m.kick, resampling)]);
        assert_eq!(m.project.route_in_a_loop(), None);
        assert!(Input::RESAMPLING.is_resampling());
        assert!(!Input::Tap(Tap::PostFx(Chain::Master)).is_resampling());
        // Nor does a key into the master from the track.
        let limiter = device(&mut m.project, NativeKind::Compressor);
        let limiter_id = limiter.id;
        m.project.master.devices.push(limiter);
        m.wire(
            Tap::PostFx(Chain::Track(m.kick)),
            Port::Sidechain(limiter_id),
        );
    }

    #[test]
    fn an_output_that_closes_a_loop_through_an_input_finds_it() {
        let mut m = mix();
        let print = m.project.add_track("Print", TrackKind::Audio);
        m.take(print, Tap::PostFader(Chain::Track(m.group)));
        let key = m.wire(Tap::PreFx(m.kick), Port::Sidechain(m.compressor));
        assert_eq!(m.project.route_in_a_loop(), None);
        // The print feeding the group closes a loop through its input,
        // which the engine leaves out.
        m.project.track_mut(print).unwrap().output = Bus::Group(m.group);
        assert_eq!(m.project.route_in_a_loop(), Some(Route::Input(print)));
        assert!(m.project.live_inputs().is_empty());
        let live: Vec<WireId> = m.project.live_wires().iter().map(|w| w.id).collect();
        assert_eq!(live, [key]);
        // Where an input and a wire close a loop together, the input plays:
        // the kick taking the bass, which its Pre FX keys.
        m.project.track_mut(print).unwrap().output = Bus::Master;
        let bass = Tap::PostFx(Chain::Track(m.bass));
        m.project.track_mut(m.kick).unwrap().input = Some(Input::Tap(bass));
        assert_eq!(m.project.route_in_a_loop(), Some(Route::Wire(key)));
        let inputs = m.project.live_inputs();
        assert_eq!(
            inputs,
            [
                (m.kick, bass),
                (print, Tap::PostFader(Chain::Track(m.group)))
            ]
        );
        assert!(m.project.live_wires().is_empty());
    }

    #[test]
    fn inputs_belong_to_the_tracks_and_devices_they_take_from() {
        let mut m = mix();
        let [a, b, c, d] =
            ["A", "B", "C", "D"].map(|name| m.project.add_track(name, TrackKind::Audio));
        m.take(a, Tap::Device(m.eq));
        m.take(b, Tap::PreFx(m.bass));
        m.take(c, Tap::PostFader(Chain::Track(m.kick)));
        m.take(d, Input::RESAMPLING.tap().unwrap());
        assert_eq!(m.project.inputs_from_track(m.bass), [a, b]);
        assert_eq!(m.project.inputs_from_track(m.kick), [c]);
        assert!(m.project.inputs_from_track(m.group).is_empty());
        assert_eq!(m.project.inputs_from_device(m.eq), [a]);
        assert!(m.project.inputs_from_device(m.compressor).is_empty());
        // An input from a track the project hasn't doesn't play.
        m.project.tracks.retain(|t| t.id != m.kick);
        let live: Vec<TrackId> = m.project.live_inputs().iter().map(|&(t, _)| t).collect();
        assert_eq!(live, [a, b, d]);
    }

    #[test]
    fn a_sends_only_track_feeds_nothing_downstream() {
        let mut m = mix();
        let key = Port::Sidechain(m.compressor);
        let master = Tap::PostFx(Chain::Master);
        // The bass feeds the master, so the master can't key it, until it
        // goes to Sends Only.
        assert_eq!(m.project.check_wire(master, key), Err(WireError::Loop));
        m.project.track_mut(m.bass).unwrap().output = Bus::SendsOnly;
        assert_eq!(m.project.check_wire(master, key), Ok(()));
        let print = m.project.add_track("Print", TrackKind::Audio);
        m.take(print, Tap::PostFader(Chain::Track(m.bass)));
    }
}
