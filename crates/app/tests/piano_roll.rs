//! Editing notes and controllers through the app, as the piano roll's
//! gestures and keys do: each change reaches the engine, rendering offline,
//! which plays the notes as they are now. Keys whose commands don't apply,
//! such as the arrow keys with no notes selected, are left to the window.

use musicable_app::Session;
use musicable_commands::{Args, Focus, KeyChord, Keymap, Value, controllers, notes};
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

    /// Adds a note to the clip, at full velocity: the A at 440 Hz.
    fn add_note(&mut self, start: Beats, length: Beats) -> Value {
        let add = Args::new()
            .with("start", start)
            .with("key", 69)
            .with("length", length)
            .with("velocity", 1.0)
            .with("clip", self.clip);
        self.session.run_with(&notes::ADD, &add).unwrap()
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
    let Value::Note(note) = rig.add_note(Beats::ZERO, Beats::new(1)) else {
        panic!("Add Note returns the note");
    };
    let editor = rig.session.editor();
    assert_eq!(editor.focus, Focus::PIANO_ROLL);
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
    rig.session.editor_mut().focus = Focus::ARRANGEMENT;
    assert!(!rig.takes("Up"));
    let stats = rig.session.stats();
    assert_eq!(stats.events_late, 0);
    assert_eq!(stats.events_dropped, 0);
}

#[test]
fn a_sustain_pedal_drawn_in_the_lane_holds_the_notes() {
    let mut rig = Rig::new();
    // A sixteenth: it's died away by the second half of the beat.
    rig.add_note(Beats::ZERO, Beats::from_f64(0.25));
    let half = (BEAT / 2) as usize;
    let played = rig.first_beat();
    assert!(peak(&played[..half]) > 0.4);
    assert!(peak(&played[half..]) < 1e-6);

    // Drawing the pedal down holds the note through the beat.
    let clip = rig.clip;
    let draw = |start: f64, value: f64| {
        Args::new()
            .with("start", Beats::from_f64(start))
            .with("end", Beats::new(1))
            .with("at", vec![Beats::from_f64(start)])
            .with("values", vec![value])
            .with("controller", 64)
            .with("clip", clip)
    };
    rig.session
        .run_with(&controllers::DRAW, &draw(0.0, 1.0))
        .unwrap();
    let held = rig.first_beat();
    assert!(peak(&held[3 * half / 2..]) > 0.4);
    // Stopping let it go, so it died away.
    assert!(peak(&rig.render(BEAT / 4)) < 1e-6);

    // Up halfway through: the note ends there.
    rig.session
        .run_with(&controllers::DRAW, &draw(0.5, 0.0))
        .unwrap();
    let played = rig.first_beat();
    assert!(peak(&played[half / 2..half]) > 0.4);
    assert!(peak(&played[5 * half / 4..]) < 1e-6);
    // Undoing holds it again, and clearing the pedal lets it end as it did.
    rig.press("Ctrl+Z");
    assert!(peak(&rig.first_beat()[3 * half / 2..]) > 0.4);
    let clear = Args::new().with("controller", 64).with("clip", rig.clip);
    rig.session.run_with(&controllers::CLEAR, &clear).unwrap();
    assert!(peak(&rig.first_beat()[half..]) < 1e-6);
    let stats = rig.session.stats();
    assert_eq!(stats.events_late, 0);
    assert_eq!(stats.events_dropped, 0);
}
