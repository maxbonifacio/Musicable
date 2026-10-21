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

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// Linear gain, 1.0 = unity.
    pub gain: f32,
    pub muted: bool,
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
}
