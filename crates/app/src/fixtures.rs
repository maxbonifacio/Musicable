//! The projects scenarios start from by name, such as `project =
//! "kick-and-bass"`, made here rather than kept as files: test material,
//! with the built-in devices, the same on every system.

use musicable_model::clip::{ClipContent, MidiClip};
use musicable_model::native::{NativeDevice, NativeKind, synth};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackId, TrackKind};

/// The fixtures, by name, with what each is.
pub const FIXTURES: [(&str, &str); 3] = [
    ("empty", "A new project, with no tracks."),
    (
        "demo",
        "The Tier 0 demo: a dozen test tones at 124 BPM, looping the first four bars.",
    ),
    (
        "kick-and-bass",
        "Two bars at 120 BPM: Kick, a short sine on every beat, and Bass, a held saw an \
         octave below, each a MIDI track playing the built-in Synth.",
    ),
];

/// The fixture called `name`, if there's one.
pub fn fixture(name: &str) -> Option<Project> {
    Some(match name {
        "empty" => Project::new(),
        "demo" => crate::demo_project(),
        "kick-and-bass" => kick_and_bass(),
        _ => return None,
    })
}

/// Kick, a short sine on each of eight beats, and Bass, a saw held for all
/// of them, at 120 BPM: what a sidechain ducks, and what does the ducking.
fn kick_and_bass() -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(120.0);
    let kick = synth_track(&mut project, "Kick", |synth| {
        synth.set(synth::OSC1_WAVE, 0.0);
        synth.set(synth::OSC1_OCTAVE, -1.0);
        synth.set(synth::FILTER_ENV, 0.0);
        synth.set(synth::AMP_DECAY, 150.0);
        synth.set(synth::AMP_SUSTAIN, 0.0);
        synth.set(synth::AMP_RELEASE, 40.0);
    });
    let bass = synth_track(&mut project, "Bass", |synth| {
        synth.set(synth::CUTOFF, 600.0);
        synth.set(synth::AMP_SUSTAIN, 100.0);
        synth.set(synth::AMP_RELEASE, 40.0);
    });
    let bars = BeatRange::new(Beats::ZERO, Beats::new(8));
    let mut kicks = MidiClip::default();
    for beat in 0..8 {
        kicks.add_note(Beats::new(beat), Beats::from_ratio(1, 4), 48, 1.0);
    }
    let mut held = MidiClip::default();
    held.add_note(Beats::ZERO, Beats::new(8), 33, 0.8);
    for (track, name, notes) in [(kick, "Kicks", kicks), (bass, "Held", held)] {
        let clip = project.new_clip(name, bars, ClipContent::Midi(notes));
        let track = project.track_mut(track).expect("the track was just added");
        track.clips.push(clip);
    }
    project
}

/// A MIDI track called `name` playing the built-in Synth, set up by `set`.
fn synth_track(project: &mut Project, name: &str, set: impl FnOnce(&mut NativeDevice)) -> TrackId {
    let id = project.add_track(name, TrackKind::Midi);
    let mut synth = NativeDevice::new(NativeKind::Synth);
    set(&mut synth);
    let device = project.new_device(DeviceKind::Native(synth));
    let track = project.track_mut(id).expect("the track was just added");
    track.devices.push(device);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_fixture_is_made_by_its_name() {
        for (name, _) in FIXTURES {
            assert!(fixture(name).is_some(), "{name}");
        }
        assert!(fixture("heavy").is_none());
        let project = fixture("kick-and-bass").unwrap();
        let names: Vec<&str> = project.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Kick", "Bass"]);
        let ClipContent::Midi(kicks) = &project.tracks[0].clips[0].content else {
            panic!("the kicks are notes");
        };
        assert_eq!(kicks.notes.len(), 8);
        // It saves and loads as any project does.
        let text = musicable_io::project::to_string(&project).unwrap();
        assert_eq!(musicable_io::project::from_str(&text).unwrap(), project);
    }
}
