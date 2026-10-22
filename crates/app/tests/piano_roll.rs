//! Editing notes through the app, as the piano roll's gestures and keys do:
//! each change reaches the engine, rendering offline, which plays the notes
//! as they are now. Keys whose commands don't apply, such as the arrow keys
//! with no notes selected, are left to the window.

use musicable_app::Session;
use musicable_commands::{Args, Focus, KeyChord, Keymap, Value, notes};
use musicable_engine::{Engine, EngineConfig, rt};
use musicable_model::clip::{ClipContent, ClipId, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceKind, Project, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// Frames to the beat at 120 BPM, the default tempo.
const BEAT: u64 = 24_000;

/// A session, and an engine that renders when told to, standing in for the
/// audio device.
struct Rig {
    engine: Engine,
    session: Session,
    clip: ClipId,
}

impl Rig {
    /// Keys, a MIDI track with the test instrument and an empty clip over
    /// the first four beats, open in the piano roll.
    fn new() -> Self {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let clip = project.new_clip("Notes", range, ClipContent::Midi(MidiClip::default()));
        let id = clip.id;
        let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let track = project.track_mut(keys).unwrap();
        track.clips.push(clip);
        track.devices.push(instrument);
        let (engine, handle) = Engine::new(CONFIG);
        let mut session = Session::new(project, None, Keymap::default_keymap(), handle);
        session.wait_for_graph();
        let open = Args::new().with("clip", id);
        session.run_with(&notes::OPEN, &open).unwrap();
        Self {
            engine,
            session,
            clip: id,
        }
    }

    fn chord(chord: &str) -> KeyChord {
        chord.parse().unwrap()
    }

    /// Whether a press of `chord` would run a command now.
    fn takes(&self, chord: &str) -> bool {
        self.session.takes_key(Self::chord(chord))
    }

    /// Presses `chord`, as the window does when the session takes it.
    fn press(&mut self, chord: &str) {
        assert!(self.takes(chord), "{chord} runs a command");
        assert!(self.session.key_pressed(Self::chord(chord)));
    }

    /// Renders `frames` frames as the audio thread would, then ticks the
    /// session as the UI would. Returns the left channel.
    fn render(&mut self, frames: u64) -> Vec<f32> {
        let mut output = vec![0.0; 2 * frames as usize];
        self.engine.process_interleaved(&mut output, 2);
        self.session.tick();
        output.into_iter().step_by(2).collect()
    }

    /// Plays the song's first beat, once the engine has the notes as they
    /// are now, and returns it.
    fn first_beat(&mut self) -> Vec<f32> {
        self.session.wait_for_graph();
        self.press("Home");
        self.press("Shift+Space");
        let played = self.render(BEAT);
        // Long enough for the notes to die away.
        self.press("Space");
        self.render(BEAT / 4);
        played
    }
}

fn peak(signal: &[f32]) -> f32 {
    signal
        .iter()
        .fold(0.0, |peak, sample| peak.max(sample.abs()))
}

/// How many times `signal` goes from below zero to zero or above.
fn cycles(signal: &[f32]) -> usize {
    signal
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count()
}

#[test]
fn notes_edited_as_the_piano_roll_does_play_as_they_are_now() {
    assert!(rt::checker_installed());
    let mut rig = Rig::new();
    assert!(peak(&rig.first_beat()) < 1e-6);
    // Live's keys for notes act on the selected ones: with none, the arrow
    // keys go to the window, which moves the keyboard focus with them.
    assert!(!rig.takes("Up"));
    assert!(!rig.takes("Delete"));

    // A double click adds a note, selected: the A at 440 Hz, for a beat.
    let add = Args::new()
        .with("start", Beats::ZERO)
        .with("key", 69)
        .with("length", Beats::new(1))
        .with("velocity", 1.0)
        .with("clip", rig.clip);
    let Value::Note(note) = rig.session.run_with(&notes::ADD, &add).unwrap() else {
        panic!("Add Note returns the note");
    };
    let editor = rig.session.editor();
    assert_eq!(editor.focus, Focus::PianoRoll);
    assert_eq!(editor.piano_roll.notes.iter().collect::<Vec<_>>(), [&note]);
    let played = rig.first_beat();
    // Half a second of 440 Hz.
    assert!(cycles(&played).abs_diff(220) <= 2, "{}", cycles(&played));
    let loud = peak(&played);
    assert!(loud > 0.4, "{loud}");

    // Shift+Up takes it up an octave.
    rig.press("Shift+Up");
    let played = rig.first_beat();
    assert!(cycles(&played).abs_diff(440) <= 2, "{}", cycles(&played));

    // Ctrl+Down makes it softer, and undoing makes it louder again.
    for _ in 0..5 {
        rig.press("Ctrl+Down");
    }
    let softer = peak(&rig.first_beat());
    assert!(softer < 0.8 * loud, "{softer} against {loud}");
    rig.press("Ctrl+Z");
    let louder = peak(&rig.first_beat());
    assert!(louder > softer, "{louder} against {softer}");

    // A drag that moves it a beat later leaves the first beat silent.
    let moved = Args::new()
        .with("notes", vec![note])
        .with("by", Beats::new(1))
        .with("clip", rig.clip);
    rig.session.run_with(&notes::MOVE, &moved).unwrap();
    assert!(peak(&rig.first_beat()) < 1e-6);
    rig.press("Ctrl+Z");

    // Delete deletes it, and undoing brings it back.
    rig.press("Delete");
    assert!(peak(&rig.first_beat()) < 1e-6);
    assert!(!rig.takes("Up"));
    rig.press("Ctrl+Z");
    let played = rig.first_beat();
    assert!(cycles(&played).abs_diff(440) <= 2, "{}", cycles(&played));

    // With the arrangement's focus, the arrow keys go to the window again.
    rig.session.editor_mut().focus = Focus::Arrangement;
    assert!(!rig.takes("Up"));
    let stats = rig.session.stats();
    assert_eq!(stats.events_late, 0);
    assert_eq!(stats.events_dropped, 0);
}
