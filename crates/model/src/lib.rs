//! Pure-data project model.
//!
//! Nothing in this crate touches audio devices, threads or the UI. The engine
//! compiles a [`Project`] into a processing graph; the UI edits it through
//! commands. [`time`] has musical time: beats, bars and the tempo map.
//! [`mixer`] has the mixer's rules: where tracks can send their signal, and
//! what mute and solo silence. [`clip`] has what tracks play: audio clips,
//! and MIDI clips of notes with per-note expression.
//!
//! Project files (`musicable_io::project`) save every field of the project,
//! so adding or changing one changes the file format. See
//! `docs/project-format.md` for how.

pub mod clip;
pub mod mixer;
pub mod time;

use clip::{Clip, ClipContent, ClipId};
use mixer::Bus;
use time::{BeatRange, Beats, TempoMap};

/// Stable identifier for a track within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Midi,
    /// A bus that other tracks' sends feed, usually for shared effects such
    /// as a reverb.
    Return,
    /// A bus that other tracks' outputs feed.
    Group,
}

impl TrackKind {
    /// Whether other tracks feed the track: a group or a return.
    pub fn is_bus(self) -> bool {
        matches!(self, Self::Return | Self::Group)
    }
}

/// A test oscillator: Tier 0's only sound source. It plays along with a
/// track's clips.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestTone {
    /// In Hz.
    pub frequency: f64,
    /// Linear amplitude, 1.0 = full scale.
    pub level: f32,
}

/// How a channel's clip stage clips. The engine mixes in floating point, so
/// nothing clips unless a channel asks for it. Tier 2 adds circuit-modelled
/// types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipType {
    /// Clean floating point.
    #[default]
    Off,
    /// Cuts everything beyond full scale.
    Hard,
    /// Rounds off peaks above half scale, so they get ever closer to full
    /// scale without reaching it.
    Soft,
}

/// A channel's console modelling settings. A placeholder until console mode
/// arrives (Tier 2 at the earliest), so project files have the field from
/// the start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConsoleSettings {}

/// Stable identifier for a device, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(pub u32);

/// A device in a channel's device chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: DeviceId,
    pub kind: DeviceKind,
}

/// What a device is. Only test devices exist so far: plugins and the
/// built-in devices come later.
#[derive(Debug, Clone, PartialEq)]
pub enum DeviceKind {
    /// Multiplies the signal by a linear gain, 1.0 = unity. It stands in for
    /// real devices until there are some.
    TestGain { gain: f32 },
    /// Plays the notes of its track's MIDI clips with a plain tone, added to
    /// what comes into it: a test instrument, standing in for instruments
    /// until there are plugins. `level` is the linear amplitude of a note
    /// at full velocity.
    TestInstrument { level: f32 },
}

impl DeviceKind {
    /// Whether the device plays notes. A MIDI track's notes go to its first
    /// instrument.
    pub fn is_instrument(&self) -> bool {
        matches!(self, Self::TestInstrument { .. })
    }
}

/// Stable identifier for a send, unique within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SendId(pub u32);

/// A send: some of a track's signal, fed to a return track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Send {
    pub id: SendId,
    /// The return track it feeds.
    pub to: TrackId,
    /// Linear gain, 1.0 = unity.
    pub level: f32,
    /// Whether it takes the signal before the track's fader and pan, rather
    /// than after them.
    pub pre_fader: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    /// Balance, from -1.0 (left) to 1.0 (right). At 0.0, the centre, both
    /// channels pass unchanged. See [`mixer::pan_gains`].
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    /// Whether other tracks' solo leaves the track playing. Return tracks
    /// start solo-safe.
    pub solo_safe: bool,
    /// Whether the track is armed for recording.
    pub armed: bool,
    /// Where the track's output goes. See [`Project::can_feed`] for which
    /// buses a track can feed.
    pub output: Bus,
    /// At most one to each return track.
    pub sends: Vec<Send>,
    pub tone: Option<TestTone>,
    /// Audio clips on audio tracks, MIDI clips on MIDI tracks, and none on
    /// buses. In any order: the arrangement places them by position.
    pub clips: Vec<Clip>,
    /// In signal order.
    pub devices: Vec<Device>,
    pub clip_type: ClipType,
    pub console: ConsoleSettings,
}

impl Track {
    /// Whether the track can hold `content`: audio clips go on audio tracks
    /// and MIDI clips on MIDI tracks.
    pub fn holds(&self, content: &ClipContent) -> bool {
        matches!(
            (self.kind, content),
            (TrackKind::Audio, ClipContent::Audio(_)) | (TrackKind::Midi, ClipContent::Midi(_))
        )
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Where the clip with `id` is in the track's clip list.
    pub fn clip_index(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// The first of the track's devices that plays notes.
    pub fn instrument(&self) -> Option<&Device> {
        self.devices.iter().find(|d| d.kind.is_instrument())
    }
}

/// The master bus's channel: every track that doesn't feed a group feeds
/// the master, and the master feeds the audio device.
#[derive(Debug, Clone, PartialEq)]
pub struct Master {
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    /// Balance, as for a track.
    pub pan: f32,
    /// In signal order.
    pub devices: Vec<Device>,
    pub clip_type: ClipType,
    pub console: ConsoleSettings,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
            devices: Vec::new(),
            clip_type: ClipType::Off,
            console: ConsoleSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Tempo and time signature.
    pub tempo: TempoMap,
    /// The region playback loops over. Kept while looping is off.
    pub loop_region: BeatRange,
    pub looping: bool,
    /// Whether the metronome clicks while the transport plays.
    pub metronome: bool,
    pub master: Master,
    pub tracks: Vec<Track>,
    /// The id [`add_track`](Self::add_track) gives the next track. Ids are
    /// never reused, so it stays above the id of every track the project has
    /// had, deleted ones included.
    pub next_track_id: u32,
    /// The same for sends.
    pub next_send_id: u32,
    /// The same for devices, on tracks and on the master.
    pub next_device_id: u32,
    /// The same for clips.
    pub next_clip_id: u32,
}

impl Default for Project {
    /// 120 BPM in 4/4, with the loop region on the first four bars.
    fn default() -> Self {
        let tempo = TempoMap::default();
        Self {
            loop_region: BeatRange::new(Beats::ZERO, tempo.bar_start(4)),
            tempo,
            looping: false,
            metronome: false,
            master: Master::default(),
            tracks: Vec::new(),
            next_track_id: 0,
            next_send_id: 0,
            next_device_id: 0,
            next_clip_id: 0,
        }
    }
}

impl Project {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a track at the end, and returns its id.
    pub fn add_track(&mut self, name: impl Into<String>, kind: TrackKind) -> TrackId {
        let track = self.new_track(name, kind);
        let id = track.id;
        self.tracks.push(track);
        id
    }

    /// A track with the next track id, for the caller to add: unity gain,
    /// centred, feeding the master. Return tracks start solo-safe.
    ///
    /// # Panics
    ///
    /// If the project has used up every track id.
    pub fn new_track(&mut self, name: impl Into<String>, kind: TrackKind) -> Track {
        let id = TrackId(next_id(&mut self.next_track_id, "track"));
        Track {
            id,
            name: name.into(),
            kind,
            gain: 1.0,
            pan: 0.0,
            muted: false,
            solo: false,
            solo_safe: kind == TrackKind::Return,
            armed: false,
            output: Bus::Master,
            sends: Vec::new(),
            tone: None,
            clips: Vec::new(),
            devices: Vec::new(),
            clip_type: ClipType::Off,
            console: ConsoleSettings::default(),
        }
    }

    /// A send from a track to the return track `to`, with the next send id,
    /// for the caller to add. It starts silent, after the fader.
    pub fn new_send(&mut self, to: TrackId) -> Send {
        Send {
            id: SendId(next_id(&mut self.next_send_id, "send")),
            to,
            level: 0.0,
            pre_fader: false,
        }
    }

    /// A device with the next device id, for the caller to add.
    pub fn new_device(&mut self, kind: DeviceKind) -> Device {
        Device {
            id: DeviceId(next_id(&mut self.next_device_id, "device")),
            kind,
        }
    }

    /// A clip with the next clip id, for the caller to put on a track.
    pub fn new_clip(
        &mut self,
        name: impl Into<String>,
        range: BeatRange,
        content: ClipContent,
    ) -> Clip {
        Clip {
            id: ClipId(next_id(&mut self.next_clip_id, "clip")),
            name: name.into(),
            start: range.start,
            length: range.len(),
            content,
        }
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Where the track with `id` is in the track list.
    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }
}

/// Takes the next id from a counter in the project. An id is only handed out if
/// the counter can move past it, so `u32::MAX` is never an id.
pub(crate) fn next_id(counter: &mut u32, what: &str) -> u32 {
    let id = *counter;
    *counter = id
        .checked_add(1)
        .unwrap_or_else(|| panic!("the project has used every {what} id"));
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ids_are_unique() {
        let mut p = Project::new();
        let a = p.add_track("Drums", TrackKind::Audio);
        let b = p.add_track("Bass", TrackKind::Midi);
        assert_ne!(a, b);
        assert_eq!(p.track(b).unwrap().name, "Bass");
        assert_eq!(p.track_index(b), Some(1));
    }

    #[test]
    fn new_tracks_dont_clip() {
        let mut p = Project::new();
        let id = p.add_track("Vocals", TrackKind::Audio);
        assert_eq!(p.track(id).unwrap().clip_type, ClipType::Off);
    }

    #[test]
    fn new_tracks_are_centred_on_the_master_and_only_returns_are_solo_safe() {
        let mut p = Project::new();
        for kind in [
            TrackKind::Audio,
            TrackKind::Midi,
            TrackKind::Return,
            TrackKind::Group,
        ] {
            let id = p.add_track("", kind);
            let track = p.track(id).unwrap();
            assert_eq!((track.gain, track.pan), (1.0, 0.0));
            assert_eq!(track.output, Bus::Master);
            assert_eq!(track.solo_safe, kind == TrackKind::Return);
        }
    }

    #[test]
    fn ids_are_never_reused() {
        let mut p = Project::new();
        let first = p.add_track("A", TrackKind::Audio);
        p.tracks.clear();
        assert_ne!(p.add_track("B", TrackKind::Audio), first);
        let send = p.new_send(TrackId(9));
        assert_ne!(p.new_send(TrackId(9)).id, send.id);
        let device = p.new_device(DeviceKind::TestGain { gain: 1.0 });
        assert_ne!(
            p.new_device(DeviceKind::TestGain { gain: 1.0 }).id,
            device.id
        );
        let range = BeatRange::new(Beats::ZERO, Beats::new(1));
        let midi = || ClipContent::Midi(clip::MidiClip::default());
        let clip = p.new_clip("", range, midi());
        assert_ne!(p.new_clip("", range, midi()).id, clip.id);
    }

    #[test]
    fn tracks_hold_clips_of_their_kind() {
        let mut p = Project::new();
        let audio = ClipContent::Audio(clip::AudioClip::new("take.wav"));
        let midi = ClipContent::Midi(clip::MidiClip::default());
        let holds = |p: &mut Project, kind| {
            let id = p.add_track("", kind);
            let track = p.track(id).unwrap();
            (track.holds(&audio), track.holds(&midi))
        };
        assert_eq!(holds(&mut p, TrackKind::Audio), (true, false));
        assert_eq!(holds(&mut p, TrackKind::Midi), (false, true));
        assert_eq!(holds(&mut p, TrackKind::Return), (false, false));
        assert_eq!(holds(&mut p, TrackKind::Group), (false, false));
    }

    #[test]
    fn a_tracks_notes_go_to_its_first_instrument() {
        let mut p = Project::new();
        let id = p.add_track("Keys", TrackKind::Midi);
        let gain = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        let first = p.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let second = p.new_device(DeviceKind::TestInstrument { level: 0.25 });
        let track = p.track_mut(id).unwrap();
        assert!(track.instrument().is_none());
        track.devices = vec![gain, first.clone(), second];
        assert_eq!(track.instrument(), Some(&first));
    }

    #[test]
    #[should_panic(expected = "every track id")]
    fn the_last_id_is_never_used() {
        let mut p = Project::new();
        p.next_track_id = u32::MAX;
        p.add_track("One too many", TrackKind::Audio);
    }
}
