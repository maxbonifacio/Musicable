//! Pure-data project model.
//!
//! Nothing in this crate touches audio devices, threads or the UI. The engine
//! compiles a [`Project`] into a processing graph; the UI edits it through
//! commands.

/// Stable identifier for a track within a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Midi,
    Return,
    Group,
}

/// A test oscillator: Tier 0's only sound source, standing in for clips and
/// instruments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TestTone {
    /// In Hz.
    pub frequency: f64,
    /// Linear amplitude, 1.0 = full scale.
    pub level: f32,
}

/// How a track's clip stage clips. The engine mixes in floating point, so
/// nothing clips unless a track asks for it. Tier 1 adds hard and soft
/// clipping, and Tier 2 circuit-modelled types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipType {
    /// Clean floating point.
    #[default]
    Off,
}

/// A track's console modelling settings. A placeholder until console mode
/// arrives (Tier 2 at the earliest), so project files have the field from
/// the start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConsoleSettings {}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    pub muted: bool,
    pub tone: Option<TestTone>,
    pub clip_type: ClipType,
    pub console: ConsoleSettings,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tempo {
    pub bpm: f64,
    pub numerator: u8,
    pub denominator: u8,
}

impl Default for Tempo {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            numerator: 4,
            denominator: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Project {
    pub tempo: Tempo,
    pub tracks: Vec<Track>,
    next_track_id: u32,
}

impl Project {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_track(&mut self, name: impl Into<String>, kind: TrackKind) -> TrackId {
        let id = TrackId(self.next_track_id);
        self.next_track_id += 1;
        self.tracks.push(Track {
            id,
            name: name.into(),
            kind,
            gain: 1.0,
            muted: false,
            tone: None,
            clip_type: ClipType::Off,
            console: ConsoleSettings::default(),
        });
        id
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }
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
    }

    #[test]
    fn new_tracks_dont_clip() {
        let mut p = Project::new();
        let id = p.add_track("Vocals", TrackKind::Audio);
        assert_eq!(p.track(id).unwrap().clip_type, ClipType::Off);
    }
}
