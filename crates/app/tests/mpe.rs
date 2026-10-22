//! N9's done test: a recorded MPE performance plays back identically into
//! an MPE synth.
//!
//! An MPE controller plays into an armed MIDI track while the transport
//! records. The track's instrument is a CLAP synth that takes only MPE
//! MIDI, and the track monitors its input, so the synth plays the
//! performance as it's recorded: notes on channels of their own, each bent,
//! pressed and slid its own way, the sustain pedal holding one, and the mod
//! wheel moving. Played back from the take, the synth gets the same MIDI
//! and plays the same, to the sample.

use std::collections::BTreeMap;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use musicable_app::Session;
use musicable_audio::{MidiBuffer, MidiWriter};
use musicable_commands::{Keymap, transport};
use musicable_engine::{AudioRef, Engine, EngineConfig, InputLayout, MidiMessage, rt};
use musicable_model::plugin::{PluginDevice, PluginSettings};
use musicable_model::{DeviceKind, PluginFormat, Project, TrackKind};
use musicable_plugins::Instance;
use musicable_plugins::scan;
use musicable_test_plugins::SYNTH_ID;
use musicable_test_plugins::testing::Library;

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 128,
};

/// How long the performance is, in frames, with room for the notes to end.
const LENGTH: u64 = 36_000;

/// A pitch bend on MIDI channel `channel`, from 0 to 15, to `value`, from
/// 0 to 16,383, 8,192 being at rest.
fn bend(channel: u8, value: u16) -> [u8; 3] {
    [0xE0 | channel, (value & 0x7F) as u8, (value >> 7) as u8]
}

/// What an MPE controller plays, by the engine frame it plays at: its zone
/// set up, as MPE controllers do, then three notes on member channels of
/// their own. The first starts pressed and slid and bends up as it plays,
/// and the sustain pedal holds it after it's let go. The second starts at
/// rest, then bends down, is pressed and slides. The third is pressed
/// hard. The mod wheel moves on the master channel in between.
fn performance() -> Vec<(u64, [u8; 3])> {
    let mut played = vec![
        // MPE's lower zone, with fifteen members.
        (0, [0xB0, 101, 0]),
        (0, [0xB0, 100, 6]),
        (0, [0xB0, 6, 15]),
        (2_000, [0xB1, 74, 40]),
        (2_000, [0xD1, 30, 0]),
        (2_000, [0x91, 60, 100]),
        (6_000, [0x92, 67, 80]),
        (8_000, [0xB0, 64, 127]),
        (10_000, bend(2, 8_192 - 2_000)),
        (10_000, [0xD2, 90, 0]),
        (12_000, [0x81, 60, 50]),
        (14_000, [0xB2, 74, 100]),
        (16_000, [0xB0, 1, 64]),
        (20_000, [0x93, 72, 120]),
        (22_000, [0x82, 67, 64]),
        (26_000, [0xB0, 64, 0]),
        (28_000, [0xD3, 127, 0]),
        (30_000, [0x83, 72, 0]),
    ];
    // The first note bends up a little more every 2,000 frames.
    for step in 1..=5u16 {
        played.push((2_000 + 2_000 * u64::from(step), bend(1, 8_192 + 300 * step)));
    }
    played.sort_by_key(|&(frame, _)| frame);
    played
}

/// The engine, a session on it with Keys, an armed MIDI track playing the
/// MPE copy of the test synth, and the MIDI input it records from.
struct Studio {
    engine: Engine,
    session: Session,
    library: Library,
    synth: musicable_model::DeviceId,
    buffer: MidiBuffer,
    /// The input's writer: dropping it would end the input.
    writer: MidiWriter,
    /// The engine's time.
    time: u64,
}

impl Studio {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("musicable-mpe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // A copy named "mpe..." takes only MPE MIDI.
        let library = Library::copy(&dir, "mpe-round-trip");
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let synth = project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: SYNTH_ID.to_str().unwrap().to_owned(),
            name: "Test Synth".to_owned(),
            instrument: true,
            settings: PluginSettings::default(),
        }));
        let id = synth.id;
        let track = project.track_mut(keys).unwrap();
        track.devices.push(synth);
        track.armed = true;
        let (engine, handle) = Engine::new(CONFIG);
        let mut session = Session::offline(project, None, Keymap::default_keymap(), handle);
        // The stream opened one MIDI input: the MPE controller.
        let (buffer, writer) = MidiBuffer::new(1024);
        let mut layout = InputLayout::new();
        layout.add_midi("Seaboard");
        session.set_inputs(layout, Vec::new(), vec![Some(buffer.clone())]);
        session.set_installed_plugins(scan::scan_here(library.path()).unwrap());
        session.wait_for_graph();
        assert!(session.plugin(id).is_some(), "the synth didn't load");
        Self {
            engine,
            session,
            library,
            synth: id,
            buffer,
            writer,
            time: 0,
        }
    }

    /// Plays a block, as a device would, with the input's `messages` that
    /// play in it, and returns its left channel. Ticks the session after,
    /// as the UI does, a little slower than real time, so the recorder
    /// keeps up.
    fn play(&mut self, messages: &[MidiMessage]) -> Vec<f32> {
        let frames = CONFIG.max_block;
        let mut output = vec![0.0; 2 * frames];
        self.engine.process_interleaved_with_input(
            AudioRef::empty(frames),
            messages,
            &mut output,
            2,
        );
        self.time += frames as u64;
        // As the stream says once it has played a frame.
        self.buffer.settle(self.time);
        self.session.tick();
        thread::sleep(Duration::from_millis(1));
        output.into_iter().step_by(2).collect()
    }

    /// The MIDI the synth got since this was last asked, between blocks.
    fn heard(&self) -> Vec<[u8; 3]> {
        let Some(Instance::Clap(plugin)) = self.session.plugin(self.synth) else {
            panic!("the synth is a CLAP plugin");
        };
        // SAFETY: the plugin is of this copy, and it's between blocks, on
        // the thread the session made it on.
        unsafe { self.library.midi(plugin.raw()) }
    }
}

/// What `heard` played on each channel, its member channels numbered in
/// the order notes first started on them: the channels a host gives notes
/// are its to choose, and what plays on different channels at once can
/// come in either order.
fn by_channel(heard: &[[u8; 3]]) -> BTreeMap<u8, Vec<[u8; 3]>> {
    let mut members = BTreeMap::new();
    for &[status, ..] in heard {
        let channel = status & 0x0F;
        if status & 0xF0 == 0x90 && channel != 0 {
            let next = members.len() as u8 + 1;
            members.entry(channel).or_insert(next);
        }
    }
    let mut channels = BTreeMap::<u8, Vec<[u8; 3]>>::new();
    for &[status, a, b] in heard {
        let channel = status & 0x0F;
        let channel = members.get(&channel).copied().unwrap_or(channel);
        channels
            .entry(channel)
            .or_default()
            .push([status & 0xF0, a, b]);
    }
    channels
}

#[test]
fn a_recorded_mpe_performance_plays_back_identically_into_an_mpe_synth() {
    assert!(rt::checker_installed());
    let mut studio = Studio::new();
    // What the synth heard as it activated: MPE's zone, set up.
    studio.play(&[]);
    let set_up = studio.heard();
    assert!(set_up.contains(&[0xB0, 6, 15]), "{set_up:?}");

    // Record the performance from the next block, hearing it as it plays.
    studio.session.run(&transport::RECORD).unwrap();
    let start = studio.time;
    let performance = performance();
    let mut live = Vec::new();
    while studio.time < start + LENGTH {
        let block = studio.time - start..studio.time - start + CONFIG.max_block as u64;
        let messages: Vec<MidiMessage> = (performance.iter())
            .filter(|(frame, _)| block.contains(frame))
            .map(|&(frame, bytes)| {
                // The stream gives the recorder and the engine each message,
                // stamped with the engine frame it's heard at.
                studio.writer.push(start + frame, bytes);
                MidiMessage {
                    time: start + frame,
                    input: 0,
                    bytes,
                }
            })
            .collect();
        live.extend(studio.play(&messages));
    }
    let heard_live = studio.heard();
    studio.session.run(&transport::STOP).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let keys = studio.session.project().tracks[0].id;
    let taken = |session: &Session| !session.project().track(keys).unwrap().clips.is_empty();
    while !taken(&studio.session) {
        assert!(Instant::now() < deadline, "no take after ten seconds");
        studio.play(&[]);
    }
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());

    // The synth played MPE: each note on a member channel of its own, with
    // its own bend, pressure and slide, and the pedal and the mod wheel on
    // the master channel.
    let starts: Vec<u8> = (heard_live.iter())
        .filter(|[status, _, velocity]| status & 0xF0 == 0x90 && *velocity > 0)
        .map(|[status, ..]| status & 0x0F)
        .collect();
    assert_eq!(starts.len(), 3, "{heard_live:?}");
    assert!(starts.iter().all(|&c| c != 0), "{starts:?}");
    assert!(starts[0] != starts[1] && starts[1] != starts[2] && starts[0] != starts[2]);
    for message in [[0xB0, 64, 127], [0xB0, 1, 64], [0xB0, 64, 0]] {
        assert!(
            heard_live.contains(&message),
            "{message:?} in {heard_live:?}"
        );
    }
    let kinds = |status: u8| {
        (heard_live.iter())
            .filter(|m| m[0] & 0xF0 == status)
            .count()
    };
    assert!(
        kinds(0xE0) > 5 && kinds(0xD0) >= 3 && kinds(0xB0) >= 5,
        "{heard_live:?}"
    );
    assert!(
        live.iter().any(|&sample| sample != 0.0),
        "the synth was silent"
    );

    // Back at the start, playing the take, once the graph with it is in.
    studio.play(&[]);
    studio.session.wait_for_graph();
    studio.session.run(&transport::GO_TO_START).unwrap();
    studio.session.run(&transport::PLAY_STOP).unwrap();
    let mut played = Vec::new();
    while played.len() < live.len() {
        played.extend(studio.play(&[]));
    }
    let heard_back = studio.heard();
    studio.session.run(&transport::STOP).unwrap();

    assert_eq!(by_channel(&heard_back), by_channel(&heard_live));
    let first_difference = (live.iter().zip(&played)).position(|(a, b)| a != b);
    assert_eq!(first_difference, None, "the take plays otherwise");
    let stats = studio.session.stats();
    assert_eq!((stats.events_late, stats.events_dropped), (0, 0));
    drop(studio.session);
    studio.library.assert_alright();
}
