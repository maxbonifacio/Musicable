//! Recording through the app: an armed track records its input while the
//! transport records, and plays it as it does, and the recording becomes a
//! take on the track, which undo takes back. A recording offset makes up
//! for an input later than its driver says. An armed MIDI track records
//! the notes its MIDI input plays the same way. Capture makes what armed
//! tracks, and those monitoring In, played without recording into takes,
//! audio and MIDI.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use musicable_app::Session;
use musicable_audio::{InputBuffer, InputWriter, MidiBuffer};
use musicable_commands::editor::AudioFile;
use musicable_commands::{Args, Keymap, edit, mixer, track, transport};
use musicable_engine::{AudioRef, Engine, EngineConfig, InputLayout, rt};
use musicable_io::audio::Decoder;
use musicable_model::clip::ClipContent;
use musicable_model::time::Beats;
use musicable_model::{Monitor, Project, TrackId, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 128,
};

/// What the input captured as engine frame `w` was heard, on channel `c`: a
/// ramp, so where each sample of a recording came from shows.
fn captured(w: u64, c: usize) -> f32 {
    (w * 2 + c as u64) as f32
}

/// The engine, a session on it with an armed audio track, Lead, whose
/// recordings go in a folder of the test's own, `name`, and the input Lead
/// records from.
struct Studio {
    engine: Engine,
    session: Session,
    lead: TrackId,
    writer: InputWriter,
    folder: PathBuf,
    /// The engine's time.
    time: u64,
    /// What the engine played, as of its last block.
    output: Vec<f32>,
}

impl Studio {
    fn new(name: &str) -> Self {
        let folder =
            std::env::temp_dir().join(format!("musicable-recording-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        let mut project = Project::new();
        let lead = project.add_track("Lead", TrackKind::Audio);
        project.track_mut(lead).unwrap().armed = true;
        let (engine, handle) = Engine::new(CONFIG);
        let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
        session.set_unsaved_recordings(Some(folder.clone()));
        // The stream opened one input, the default, in stereo.
        let (buffer, mut writer) = InputBuffer::new(2, 1 << 17);
        writer.begin(0);
        let mut layout = InputLayout::new();
        layout.add("mic", 0, 2);
        layout.set_default("mic");
        session.set_inputs(layout, vec![Some(buffer)], Vec::new());
        Self {
            engine,
            session,
            lead,
            writer,
            folder,
            time: 0,
            output: vec![0.0; 2 * CONFIG.max_block],
        }
    }

    /// Plays a block, as a device would, with what the input captures as
    /// it plays, a little slower than real time, so the recorder keeps up.
    fn play(&mut self) {
        let frames = CONFIG.max_block as u64;
        let range = self.time..self.time + frames;
        let planar: Vec<f32> = (0..2)
            .flat_map(|c| range.clone().map(move |w| captured(w, c)))
            .collect();
        let input = AudioRef::from_planar(&planar, 2);
        self.engine
            .process_interleaved_with_input(input, &[], &mut self.output, 2);
        let interleaved: Vec<f32> = range
            .flat_map(|w| [captured(w, 0), captured(w, 1)])
            .collect();
        self.writer.append(&interleaved);
        self.time += frames;
        self.session.tick();
        thread::sleep(Duration::from_millis(1));
    }

    /// Plays blocks until `done` says so, failing after ten seconds.
    fn play_until(&mut self, what: &str, done: impl Fn(&Session) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(&self.session) {
            assert!(
                Instant::now() < deadline,
                "no {what} after ten seconds: {:?}",
                self.session.take_problems()
            );
            self.play();
        }
    }
}

#[test]
fn armed_tracks_record_takes_where_they_were_played() {
    let mut studio = Studio::new("record");
    studio.session.run(&transport::RECORD).unwrap();
    assert!(studio.session.is_recording());
    // Recording starts with the first block, once the graph is there.
    studio.session.wait_for_graph();
    let mut heard = false;
    while studio.time < 48_000 {
        studio.play();
        heard |= studio.output.iter().any(|&sample| sample != 0.0);
    }
    assert!(heard, "Lead doesn't play its input while it records");
    studio.play_until("recording to show", |s| !s.recordings().is_empty());
    let shown = studio.session.recordings();
    assert_eq!((shown[0].track, shown[0].start), (studio.lead, 0));

    studio.session.run(&transport::STOP).unwrap();
    assert!(!studio.session.is_recording());
    let lead = studio.lead;
    studio.play_until("take", |s| {
        !s.project().track(lead).unwrap().takes.is_empty()
    });
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());

    // A second of the input, from the first frame, in a take of its own,
    // which plays in the arrangement.
    let path = std::path::absolute(studio.folder.join("Lead 1.wav")).unwrap();
    let name = path.to_string_lossy().into_owned();
    let track = studio.session.project().track(lead).unwrap();
    assert_eq!(track.takes.len(), 1);
    assert_eq!(track.takes[0].name, "Take 1");
    assert_eq!(track.clips.len(), 1);
    let clip = &track.clips[0];
    // Two beats at 120 BPM.
    assert_eq!((clip.start, clip.end()), (Beats::ZERO, Beats::new(2)));
    let ClipContent::Audio(audio) = &clip.content else {
        panic!("{clip:?}");
    };
    assert_eq!((audio.file.as_str(), audio.offset), (name.as_str(), 0));
    let file = AudioFile {
        sample_rate: 48_000,
        frames: 48_000,
        channels: 2,
    };
    assert_eq!(studio.session.editor().files.get(&name), Some(&file));
    let channels = Decoder::open(&path).unwrap().read_all().unwrap();
    for (c, channel) in channels.iter().enumerate() {
        let expected: Vec<f32> = (0..48_000).map(|w| captured(w, c)).collect();
        assert!(*channel == expected, "channel {c} isn't the input");
    }

    // The take goes in one step.
    assert_eq!(studio.session.undo_name(), Some("Record"));
    studio.session.run(&edit::UNDO).unwrap();
    let track = studio.session.project().track(lead).unwrap();
    assert!(track.takes.is_empty() && track.clips.is_empty());
    let _ = fs::remove_dir_all(&studio.folder);
}

#[test]
fn a_recording_offset_makes_up_for_a_late_input() {
    // The input's buffer has each frame 100 frames after it was heard, as
    // through a device that doesn't report that much of its latency.
    let mut studio = Studio::new("offset");
    studio.session.set_recording_offset(100);
    assert_eq!(studio.session.recording_offset(), 100);
    studio.session.run(&transport::RECORD).unwrap();
    studio.session.wait_for_graph();
    while studio.time < 48_000 {
        studio.play();
    }
    studio.session.run(&transport::STOP).unwrap();
    let lead = studio.lead;
    studio.play_until("take", |s| {
        !s.project().track(lead).unwrap().takes.is_empty()
    });
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());
    // The take starts 100 frames into what the input captured.
    let path = studio.folder.join("Lead 1.wav");
    let channels = Decoder::open(&path).unwrap().read_all().unwrap();
    for (c, channel) in channels.iter().enumerate() {
        assert_eq!(channel.len(), 48_000);
        let expected: Vec<f32> = (100..48_100).map(|w| captured(w, c)).collect();
        assert!(
            *channel == expected,
            "channel {c} isn't the input, 100 later"
        );
    }
    let _ = fs::remove_dir_all(&studio.folder);
}

#[test]
fn armed_midi_tracks_record_the_notes_played() {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    project.track_mut(keys).unwrap().armed = true;
    let (mut engine, handle) = Engine::new(CONFIG);
    let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
    // The stream opened one MIDI input, which plays a note from a quarter
    // of a second in to three quarters.
    let (buffer, mut writer) = MidiBuffer::new(64);
    let mut layout = InputLayout::new();
    layout.add_midi("Keystep");
    session.set_inputs(layout, Vec::new(), vec![Some(buffer.clone())]);
    writer.push(12_000, [0x90, 60, 100]);
    writer.push(36_000, [0x80, 60, 64]);
    session.run(&transport::RECORD).unwrap();
    session.wait_for_graph();
    let mut output = vec![0.0; 2 * CONFIG.max_block];
    let mut time = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut shown = false;
    while session.project().track(keys).unwrap().takes.is_empty() {
        assert!(
            Instant::now() < deadline,
            "no take after ten seconds: {:?}",
            session.take_problems()
        );
        if time == 48_000 {
            session.run(&transport::STOP).unwrap();
        }
        engine.process_interleaved(&mut output, 2);
        time += CONFIG.max_block as u64;
        // As the stream says once it has played a frame.
        buffer.settle(time);
        session.tick();
        shown |= session.recordings().iter().any(|r| r.track == keys);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(shown, "the recording didn't show as it recorded");
    assert_eq!(session.take_problems(), Vec::<String>::new());

    // A clip of the two beats recorded, with the note half a beat in, a
    // beat long.
    let track = session.project().track(keys).unwrap();
    assert_eq!(track.takes.len(), 1);
    assert_eq!(track.clips.len(), 1);
    let clip = &track.clips[0];
    assert_eq!((clip.start, clip.end()), (Beats::ZERO, Beats::new(2)));
    let ClipContent::Midi(midi) = &clip.content else {
        panic!("{clip:?}");
    };
    let notes: Vec<_> = (midi.notes.iter())
        .map(|note| (note.start, note.length, note.key, note.velocity))
        .collect();
    let half = Beats::from_f64(0.5);
    assert_eq!(notes, [(half, Beats::new(1), 60, 100.0 / 127.0)]);

    assert_eq!(session.undo_name(), Some("Record"));
    session.run(&edit::UNDO).unwrap();
    let track = session.project().track(keys).unwrap();
    assert!(track.takes.is_empty() && track.clips.is_empty());
}

#[test]
fn capture_takes_what_tracks_played_without_recording() {
    let mut studio = Studio::new("capture");
    // A new track, which isn't armed, and plays its input, the same as
    // Lead's, always.
    studio.session.run(&track::NEW_AUDIO_TRACK).unwrap();
    let pad = studio.session.project().tracks.last().unwrap().id;
    let args = Args::new().with("track", pad).with("monitor", "In");
    studio.session.run_with(&mixer::SET_MONITOR, &args).unwrap();
    assert_eq!(
        studio.session.project().track(pad).unwrap().monitor,
        Monitor::In
    );
    studio.session.set_capture_seconds(10);
    studio.session.run(&transport::PLAY).unwrap();
    studio.session.wait_for_graph();
    while studio.time < 48_000 {
        studio.play();
    }
    studio.session.run(&transport::STOP).unwrap();
    studio.play();
    studio.session.capture();
    let lead = studio.lead;
    studio.play_until("capture", |s| {
        !s.project().track(lead).unwrap().takes.is_empty()
    });
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());

    // The second played, from the first frame, on both tracks, in files
    // named after them.
    for track in [lead, pad] {
        let track = studio.session.project().track(track).unwrap();
        assert_eq!(
            (track.takes.len(), track.clips.len()),
            (1, 1),
            "{}",
            track.name
        );
        let clip = &track.clips[0];
        assert_eq!((clip.start, clip.end()), (Beats::ZERO, Beats::new(2)));
        let file = format!("{} 1.wav", track.name);
        let path = studio.folder.join(&file);
        let channels = Decoder::open(&path).unwrap().read_all().unwrap();
        for (c, channel) in channels.iter().enumerate() {
            let expected: Vec<f32> = (0..48_000).map(|w| captured(w, c)).collect();
            assert!(
                *channel == expected,
                "channel {c} of {file} isn't the input"
            );
        }
    }

    assert_eq!(studio.session.undo_name(), Some("Capture"));
    studio.session.run(&edit::UNDO).unwrap();
    let track = studio.session.project().track(lead).unwrap();
    assert!(track.takes.is_empty() && track.clips.is_empty());
    let _ = fs::remove_dir_all(&studio.folder);
}

#[test]
fn capture_takes_the_notes_midi_tracks_played() {
    let mut project = Project::new();
    let keys = project.add_track("Keys", TrackKind::Midi);
    project.track_mut(keys).unwrap().armed = true;
    let (mut engine, handle) = Engine::new(CONFIG);
    let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
    session.set_capture_seconds(10);
    // The note armed_midi_tracks_record_the_notes_played records, played
    // without recording.
    let (buffer, mut writer) = MidiBuffer::new(64);
    let mut layout = InputLayout::new();
    layout.add_midi("Keystep");
    session.set_inputs(layout, Vec::new(), vec![Some(buffer.clone())]);
    writer.push(12_000, [0x90, 60, 100]);
    writer.push(36_000, [0x80, 60, 64]);
    session.run(&transport::PLAY).unwrap();
    session.wait_for_graph();
    let mut output = vec![0.0; 2 * CONFIG.max_block];
    let mut time = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while session.project().track(keys).unwrap().takes.is_empty() {
        assert!(Instant::now() < deadline, "no capture after ten seconds");
        if time == 48_000 {
            session.run(&transport::STOP).unwrap();
        } else if time == 48_000 + CONFIG.max_block as u64 {
            session.capture();
        }
        engine.process_interleaved(&mut output, 2);
        time += CONFIG.max_block as u64;
        buffer.settle(time);
        session.tick();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(session.take_problems(), Vec::<String>::new());

    let track = session.project().track(keys).unwrap();
    assert_eq!((track.takes.len(), track.clips.len()), (1, 1));
    let clip = &track.clips[0];
    assert_eq!((clip.start, clip.end()), (Beats::ZERO, Beats::new(2)));
    let ClipContent::Midi(midi) = &clip.content else {
        panic!("{clip:?}");
    };
    let notes: Vec<_> = (midi.notes.iter())
        .map(|note| (note.start, note.length, note.key, note.velocity))
        .collect();
    let half = Beats::from_f64(0.5);
    assert_eq!(notes, [(half, Beats::new(1), 60, 100.0 / 127.0)]);

    assert_eq!(session.undo_name(), Some("Capture"));
    session.run(&edit::UNDO).unwrap();
    let track = session.project().track(keys).unwrap();
    assert!(track.takes.is_empty() && track.clips.is_empty());
}
