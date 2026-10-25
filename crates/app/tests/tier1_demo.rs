//! The Tier 1 demo from the plan (N13), offline, with the test plugins:
//! record a MIDI part into a synth plugin and an audio take through an
//! input, comp the best take, arrange the clips, mix with an EQ and a
//! compressor and a reverb send, automate a fader, undo and redo freely,
//! save and reopen, and export a WAV. It runs one command at a time, as the
//! app runs them, once with a CLAP synth whose voices play on the
//! renderer's threads and once with a VST3 synth.
//!
//! Along the way it checks what listening can't: the takes hold what the
//! inputs played, on the frame; the comp plays the parts picked from them,
//! to the sample; undoing every step leaves the project empty, and redoing
//! them plays the song as a fresh open of it does; the project comes back
//! from its file as it was saved; the parallel scheduler plays it exactly
//! as the reference renderer does; and the exported WAV is what the app
//! plays.

use std::collections::BTreeSet;
use std::f64::consts::TAU;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use musicable_app::Session;
use musicable_app::export::{
    Bits, Export, ExportSettings, FileFormat, Job, Report, Span, files_for,
};
use musicable_app::null_test::{self, Player, Render};
use musicable_audio::{InputBuffer, InputWriter, MidiBuffer, MidiWriter};
use musicable_commands::{
    Args, CommandId, Editor, Keymap, Value, arrange, automation, device, edit, mixer, take, track,
    transport,
};
use musicable_engine::{
    AudioRef, Engine, EngineConfig, InputLayout, MidiMessage, ParallelOptions, rt,
};
use musicable_io::audio::Decoder;
use musicable_io::project;
use musicable_model::clip::{Clip, ClipContent, ClipId, Offset};
use musicable_model::native::{compressor, eq, reverb};
use musicable_model::param::ParamAddress;
use musicable_model::time::Beats;
use musicable_model::{Chain, DeviceId, PluginFormat, Project, TrackId};
use musicable_plugins::{Instance, PluginInfo, scan};
use musicable_test_plugins::testing::Library;
use musicable_test_plugins::vst3::VST3_SYNTH_ID;
use musicable_test_plugins::{SYNTH_ID, THREADS};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

/// Blocks as small as the demo plays in on Windows.
const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 128,
};

/// A bar at 120 BPM, a new project's tempo: four beats, two seconds.
const BAR: u64 = 96_000;

/// The song, once it's arranged: two bars.
const SONG: u64 = 2 * BAR;

/// Half a second past the song, for the reverb's tail.
const TAIL: u64 = 24_000;

fn beats(beats: i64) -> Beats {
    Beats::new(beats)
}

/// What the singer sings at frame `f` of the recording, on channel `c`: a
/// phrase a bar long, twice round the loop, with a cough over the second
/// half of the first time round and the first half of the second. Comping
/// the first half of the first take with the second half of the second
/// leaves the coughs out.
fn sung(f: u64, c: usize) -> f32 {
    let half = (f % BAR) / (BAR / 2);
    let coughed = matches!((f / BAR, half), (0, 1) | (1, 0));
    if coughed {
        // Loud and rough: a ramp up and down every 37 frames.
        let step = (f + 11 * c as u64) % 37;
        return (step as f32 / 18.0 - 1.0) * 0.9;
    }
    let t = f as f64 / f64::from(CONFIG.sample_rate);
    let pitch = [220.0, 330.0][c];
    (0.3 * (TAU * pitch * t).sin() * (0.5 + 0.5 * (TAU * 0.5 * t).cos())) as f32
}

/// What the comp plays at song frame `f` of the first bar: the first take's
/// first half and the second take's second half.
fn comped(f: u64, c: usize) -> f32 {
    match f < BAR / 2 {
        true => sung(f, c),
        false => sung(BAR + f, c),
    }
}

/// A note on, or off with velocity 0, on MIDI channel 1.
fn note(key: u8, velocity: u8) -> [u8; 3] {
    match velocity {
        0 => [0x80, key, 64],
        _ => [0x90, key, velocity],
    }
}

/// What's played on the keyboard, by the frame of the recording it's
/// played at: an arpeggio over the first time round the loop, a note a
/// beat, each half a beat long, and nothing the second time round.
fn played() -> Vec<(u64, [u8; 3])> {
    let beat = BAR / 4;
    [60, 64, 67, 72]
        .into_iter()
        .enumerate()
        .flat_map(|(n, key)| {
            let on = n as u64 * beat;
            [(on, note(key, 100)), (on + beat / 2, note(key, 0))]
        })
        .collect()
}

/// The demo's tracks, devices and takes, as it makes them.
#[derive(Debug, Clone, Copy)]
struct Made {
    keys: TrackId,
    vocal: TrackId,
    synth: DeviceId,
}

/// The app as it plays the demo: the engine, standing in for the audio
/// device with an input for the singer and a MIDI input for the keyboard,
/// and the session on it, in the project's folder.
struct Studio {
    format: PluginFormat,
    engine: Engine,
    session: Session,
    folder: PathBuf,
    /// The test plugins, copied for this test, and what a scan found of
    /// them.
    library: Library,
    installed: Vec<PluginInfo>,
    audio: InputWriter,
    midi: MidiBuffer,
    /// The MIDI input's writer: dropping it would end the input.
    keyboard: MidiWriter,
    /// The engine's time.
    time: u64,
    /// The engine's time when recording started, while it records.
    recording: Option<u64>,
    /// What the engine played in its last block.
    output: Vec<f32>,
}

impl Studio {
    /// The app with a new project saved as `Demo.musicable` in a folder of
    /// its own, and the test plugins in `format` installed.
    fn new(format: PluginFormat) -> Self {
        let name = match format {
            PluginFormat::Clap => "clap",
            PluginFormat::Vst3 => "vst3",
        };
        let folder = std::env::temp_dir().join(format!(
            "musicable-tier1-demo-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let copy = format!("tier1-demo-{name}");
        let library = match format {
            PluginFormat::Clap => Library::copy(&folder, &copy),
            PluginFormat::Vst3 => Library::copy_vst3(&folder, &copy),
        };
        let installed = scan::scan_here(library.path()).unwrap();
        let project = Project::new();
        project::save(folder.join("Demo.musicable"), &project).unwrap();

        let (engine, handle, _) = musicable_app::engine(CONFIG);
        let mut session = Session::new(project, Some(&folder), Keymap::default_keymap(), handle);
        session.set_installed_plugins(installed.clone());
        // The stream opened the interface's stereo input, the default, and
        // the keyboard.
        let (buffer, mut audio) = InputBuffer::new(2, 1 << 18);
        audio.begin(0);
        let (midi, keyboard) = MidiBuffer::new(1024);
        let mut layout = InputLayout::new();
        layout.add("Interface", 0, 2);
        layout.set_default("Interface");
        layout.add_midi("Keyboard");
        session.set_inputs(layout, vec![Some(buffer)], vec![Some(midi.clone())]);
        session.wait_for_graph();
        Self {
            format,
            engine,
            session,
            folder,
            library,
            installed,
            audio,
            midi,
            keyboard,
            time: 0,
            recording: None,
            output: vec![0.0; 2 * CONFIG.max_block],
        }
    }

    /// Runs the command `id` with `args`, as a click or a key would, and
    /// waits for the graph it needs, if any, as the app's next frames do.
    fn run(&mut self, id: &CommandId, args: Args) -> Value {
        let value = self
            .session
            .run_with(id, &args)
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        self.session.wait_for_graph();
        self.play();
        value
    }

    /// Runs `id`, which gives back a device.
    fn add_device(&mut self, id: &CommandId, args: Args) -> DeviceId {
        match self.run(id, args) {
            Value::Device(device) => device,
            other => panic!("{id} gave back {other:?}, not a device"),
        }
    }

    /// Sets parameter `param` of the built-in device `device` to `value`.
    fn set(&mut self, device: DeviceId, param: usize, value: f64) {
        let args = Args::new()
            .with("device", device)
            .with("param", param as i64)
            .with("value", value);
        self.run(&device::SET_PARAM, args);
    }

    /// Adds a track with `new`, and names it `name`.
    fn new_track(&mut self, new: &CommandId, name: &str) -> TrackId {
        self.session.run(new).unwrap();
        let id = self.session.project().tracks.iter().map(|t| t.id).max();
        let id = id.expect("a new track");
        let args = Args::new().with("track", id).with("name", name);
        self.run(&track::RENAME, args);
        id
    }

    /// Plays a block, as the audio device would, with what the singer sings
    /// and the keyboard plays in it while recording, then ticks the session,
    /// as the UI does, and pauses a moment for the threads that compile
    /// graphs and write recordings.
    fn play(&mut self) {
        let frames = CONFIG.max_block as u64;
        let range = self.time..self.time + frames;
        let recording = self.recording;
        let at = move |w: u64| recording.map(|start| w.wrapping_sub(start));
        let sings =
            move |w: u64, c: usize| at(w).filter(|&f| f < 2 * BAR).map_or(0.0, |f| sung(f, c));
        let planar: Vec<f32> = (0..2)
            .flat_map(|c| range.clone().map(move |w| sings(w, c)))
            .collect();
        let mut messages = Vec::new();
        if let Some(start) = self.recording {
            for (f, bytes) in played() {
                if range.contains(&(start + f)) {
                    // The stream gives the recorder and the engine each
                    // message, stamped with the engine frame it's heard at.
                    self.keyboard.push(start + f, bytes);
                    messages.push(MidiMessage {
                        time: start + f,
                        input: 0,
                        bytes,
                    });
                }
            }
        }
        let input = AudioRef::from_planar(&planar, 2);
        self.engine
            .process_interleaved_with_input(input, &messages, &mut self.output, 2);
        let interleaved: Vec<f32> = range.flat_map(|w| [sings(w, 0), sings(w, 1)]).collect();
        self.audio.append(&interleaved);
        self.time += frames;
        // As the stream says once it has played a frame.
        self.midi.settle(self.time);
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

    /// Plays `frames` frames from the start of the song, as Space does from
    /// the start marker, and returns them, interleaved.
    fn play_song(&mut self, frames: u64) -> Vec<f32> {
        self.session.run(&transport::GO_TO_START).unwrap();
        self.session.run(&transport::PLAY_STOP).unwrap();
        self.session.wait_for_graph();
        let mut played = Vec::new();
        while (played.len() as u64) < 2 * frames {
            self.play();
            played.extend_from_slice(&self.output);
        }
        self.session.run(&transport::STOP).unwrap();
        self.play();
        played.truncate(2 * frames as usize);
        played
    }

    /// Whether the test synth's voices have run as thread pool tasks. Only
    /// the CLAP synth runs them so.
    fn pool_used(&self, synth: DeviceId) -> bool {
        let Some(Instance::Clap(plugin)) = self.session.plugin(synth) else {
            return false;
        };
        // SAFETY: the plugin is of this copy, and it's between blocks, on
        // the thread the session made it on.
        let (requests, _) = unsafe { self.library.pool_use(plugin.raw()) };
        requests > 0
    }
}

/// A player of `project`, from `studio`'s folder with its plugins, that
/// renders with `render`.
fn player(studio: &Studio, project: &Project, render: &Render) -> Player {
    let folder = Some(studio.folder.as_path());
    let installed = studio.installed.clone();
    Player::with_plugins(project.clone(), folder, render, CONFIG, installed)
}

/// What `player` plays over its first `frames` frames, interleaved.
fn render(player: &mut Player, frames: u64) -> Vec<f32> {
    let mut played = Vec::new();
    while (played.len() as u64) < 2 * frames {
        let block = (frames - played.len() as u64 / 2).min(CONFIG.max_block as u64);
        played.extend_from_slice(player.render(block as usize));
    }
    played
}

/// The parallel scheduler with three workers, sharing every block, however
/// many cores the machine has.
fn shared() -> Render {
    Render::Parallel(ParallelOptions {
        threads: 3,
        inline_below: None,
        realtime: false,
        ..ParallelOptions::default()
    })
}

/// Where `got` first differs from `expected`, to the bit, by frame.
#[track_caller]
fn assert_same(got: &[f32], expected: &[f32], what: &str) {
    assert_eq!(got.len(), expected.len(), "{what}: the lengths differ");
    let first = (got.iter().zip(expected)).position(|(a, b)| a.to_bits() != b.to_bits());
    if let Some(i) = first {
        panic!(
            "{what}: frame {} differs, channel {}: {} where it should be {}",
            i / 2,
            i % 2,
            got[i],
            expected[i]
        );
    }
}

/// The project as it is, but for the ids it would give new things, which
/// it never gives twice, so undo leaves them.
fn without_ids(project: &Project) -> Project {
    Project {
        next_track_id: 0,
        next_send_id: 0,
        next_device_id: 0,
        next_clip_id: 0,
        next_locator_id: 0,
        next_take_id: 0,
        ..project.clone()
    }
}

/// A track's clips, in the order they play.
fn clips(project: &Project, track: TrackId) -> Vec<Clip> {
    let mut clips = project.track(track).unwrap().clips.clone();
    clips.sort_by_key(|clip| clip.start);
    clips
}

fn ids(value: Value) -> Vec<ClipId> {
    match value {
        Value::List(values) => (values.into_iter())
            .map(|value| match value {
                Value::Clip(clip) => clip,
                other => panic!("{other:?} isn't a clip"),
            })
            .collect(),
        other => panic!("{other:?} isn't a list of clips"),
    }
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn the_demo_with_a_clap_synth() {
    demo(PluginFormat::Clap);
}

#[test]
fn the_demo_with_a_vst3_synth() {
    demo(PluginFormat::Vst3);
}

fn demo(format: PluginFormat) {
    let mut studio = Studio::new(format);
    let made = set_up(&mut studio);
    record(&mut studio, made);
    comp(&mut studio, made);
    arrange(&mut studio, made);
    mix(&mut studio, made);
    let finished = studio.session.project().clone();
    undo_and_redo(&mut studio, &finished);
    let reopened = save_and_reopen(&mut studio);
    play_alike(&studio, &reopened);
    export(&studio, &reopened);
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());
    let stats = studio.session.stats();
    assert_eq!((stats.events_late, stats.events_dropped), (0, 0));
    // The engine goes before the session, and the plugins with it.
    let Studio {
        engine,
        session,
        library,
        folder,
        ..
    } = studio;
    drop(engine);
    drop(session);
    library.assert_alright();
    drop(library);
    let _ = fs::remove_dir_all(folder);
}

/// A MIDI track with the test synth, and an audio track, both armed, and
/// the loop on over the first bar.
fn set_up(studio: &mut Studio) -> Made {
    let keys = studio.new_track(&track::NEW_MIDI_TRACK, "Keys");
    let (format, id) = match studio.format {
        PluginFormat::Clap => ("clap", SYNTH_ID.to_str().unwrap()),
        PluginFormat::Vst3 => ("vst3", VST3_SYNTH_ID),
    };
    let args = Args::new()
        .with("format", format)
        .with("plugin", id)
        .with("name", "Test Synth")
        .with("instrument", true)
        .with("track", keys);
    let synth = studio.add_device(&device::ADD_PLUGIN, args);
    assert!(
        studio.session.plugin(synth).is_some(),
        "the synth didn't load"
    );
    if studio.format == PluginFormat::Clap {
        // Its voices play on the renderer's threads.
        let args = Args::new()
            .with("device", synth)
            .with("param", i64::from(THREADS))
            .with("value", 1.0);
        studio.run(&device::SET_PARAM, args);
    }
    let vocal = studio.new_track(&track::NEW_AUDIO_TRACK, "Vocal");
    for track in [keys, vocal] {
        let args = Args::new().with("track", track).with("armed", true);
        studio.run(&mixer::SET_ARM, args);
    }
    let args = Args::new()
        .with("start", Beats::ZERO)
        .with("end", beats(4))
        .with("looping", true);
    studio.run(&transport::SET_LOOP, args);
    Made { keys, vocal, synth }
}

/// Records twice round the loop: the keyboard the first time, the singer
/// both times, hearing both as they play.
fn record(studio: &mut Studio, made: Made) {
    let Made { keys, vocal, synth } = made;
    studio.session.run(&transport::RECORD).unwrap();
    assert!(studio.session.is_recording());
    // Recording starts with the next block, once the graph is in.
    studio.session.wait_for_graph();
    let start = studio.time;
    studio.recording = Some(start);
    let mut heard = false;
    while studio.time < start + 2 * BAR {
        studio.play();
        heard |= studio.output.iter().any(|&sample| sample != 0.0);
    }
    assert!(heard, "the tracks don't play their inputs as they record");
    studio.session.run(&transport::STOP).unwrap();
    studio.recording = None;
    studio.play_until("takes", |session| {
        let project = session.project();
        let takes = |track| project.track(track).unwrap().takes.len();
        takes(vocal) == 2 && takes(keys) == 1
    });
    assert_eq!(studio.session.take_problems(), Vec::<String>::new());
    assert_eq!(studio.session.undo_name(), Some("Record"));
    if studio.format == PluginFormat::Clap {
        assert!(
            studio.pool_used(synth),
            "the synth's voices didn't run as tasks"
        );
    }

    // A take for each time round the loop, one after the other in one file,
    // each a bar long from the start of the song: the last on the track.
    let project = studio.session.project();
    let track = project.track(vocal).unwrap();
    let file = "Recordings/Vocal 1.wav";
    for (n, take) in track.takes.iter().enumerate() {
        assert_eq!(take.name, format!("Take {}", n + 1));
        let [clip] = &take.clips[..] else {
            panic!("{:?}", take.clips);
        };
        assert_eq!((clip.start, clip.end()), (Beats::ZERO, beats(4)));
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("{clip:?}");
        };
        let played = (audio.file.as_str(), audio.offset);
        assert_eq!(played, (file, Offset::Frames(n as u64 * BAR)));
    }
    let [clip] = &track.clips[..] else {
        panic!("{:?}", track.clips);
    };
    assert_eq!(clip.content, track.takes[1].clips[0].content);
    // The file holds what was sung, from the first frame.
    let channels = Decoder::open(&studio.folder.join(file))
        .unwrap()
        .read_all()
        .unwrap();
    for (c, channel) in channels.iter().enumerate() {
        let expected: Vec<f32> = (0..2 * BAR).map(|f| sung(f, c)).collect();
        assert!(*channel == expected, "channel {c} isn't what was sung");
    }

    // The keyboard's take: the arpeggio, each note where it was played.
    let track = project.track(keys).unwrap();
    assert_eq!(track.takes.len(), 1);
    let [clip] = &track.clips[..] else {
        panic!("{:?}", track.clips);
    };
    assert_eq!((clip.start, clip.end()), (Beats::ZERO, beats(4)));
    let ClipContent::Midi(midi) = &clip.content else {
        panic!("{clip:?}");
    };
    let notes: Vec<_> = (midi.notes.iter())
        .map(|note| (note.start, note.length, note.key, note.velocity))
        .collect();
    let half = Beats::from_ratio(1, 2);
    let velocity = 100.0 / 127.0;
    let expected: Vec<_> = [60, 64, 67, 72]
        .into_iter()
        .enumerate()
        .map(|(n, key)| (beats(n as i64), half, key, velocity))
        .collect();
    assert_eq!(notes, expected);

    for track in [keys, vocal] {
        let args = Args::new().with("track", track).with("armed", false);
        studio.run(&mixer::SET_ARM, args);
    }
}

/// Picks the first half of the first take, which the track then plays with
/// the second half of the second.
fn comp(studio: &mut Studio, made: Made) {
    let Made { keys, vocal, .. } = made;
    let first = studio.session.project().track(vocal).unwrap().takes[0].id;
    let args = Args::new()
        .with("take", first)
        .with("start", Beats::ZERO)
        .with("end", beats(2));
    studio.run(&take::COMP, args);
    let comp: Vec<_> = clips(studio.session.project(), vocal)
        .into_iter()
        .map(|clip| {
            let ClipContent::Audio(audio) = &clip.content else {
                panic!("{clip:?}");
            };
            (clip.start, clip.end(), audio.offset)
        })
        .collect();
    assert_eq!(
        comp,
        [
            (Beats::ZERO, beats(2), Offset::Frames(0)),
            (beats(2), beats(4), Offset::Frames(BAR + BAR / 2))
        ]
    );

    // It plays the parts picked, to the sample, and none of the coughs.
    let mut alone = studio.session.project().clone();
    alone.track_mut(keys).unwrap().muted = true;
    let mut player = player(studio, &alone, &Render::Reference);
    let played = render(&mut player, BAR);
    let expected: Vec<f32> = (0..BAR)
        .flat_map(|f| [comped(f, 0), comped(f, 1)])
        .collect();
    let first = (played.iter().zip(&expected)).position(|(a, b)| a != b);
    assert_eq!(first, None, "the comp doesn't play what was picked");
}

/// Copies the first bar to the second, and leaves the last two notes out
/// there.
fn arrange(studio: &mut Studio, made: Made) {
    let Made { keys, vocal, .. } = made;
    let project = studio.session.project();
    let bar: Vec<ClipId> = (clips(project, vocal).iter())
        .chain(&clips(project, keys))
        .map(|clip| clip.id)
        .collect();
    let args = Args::new()
        .with("clips", bar)
        .with("by", beats(4))
        .with("copy", true);
    let copies = ids(studio.run(&arrange::MOVE, args));
    assert_eq!(copies.len(), 3);
    let copy = clips(studio.session.project(), keys)[1].id;
    let args = Args::new().with("clips", vec![copy]).with("at", beats(6));
    let parts = ids(studio.run(&arrange::SPLIT, args));
    let args = Args::new().with("clips", parts);
    studio.run(&arrange::DELETE, args);
    let args = Args::new()
        .with("start", Beats::ZERO)
        .with("end", beats(8))
        .with("looping", false);
    studio.run(&transport::SET_LOOP, args);

    let project = studio.session.project();
    let ranges = |track| -> Vec<(Beats, Beats)> {
        (clips(project, track).iter())
            .map(|clip| (clip.start, clip.end()))
            .collect()
    };
    let (two, four, six, eight) = (beats(2), beats(4), beats(6), beats(8));
    assert_eq!(
        ranges(vocal),
        [(Beats::ZERO, two), (two, four), (four, six), (six, eight)]
    );
    assert_eq!(ranges(keys), [(Beats::ZERO, four), (four, six)]);
}

/// An EQ and a compressor on the vocal, a reverb on a return track that
/// both send to, the keys turned down, and the vocal's fader automated to
/// dip in the second half of the second bar.
fn mix(studio: &mut Studio, made: Made) {
    let Made { keys, vocal, .. } = made;
    let add = |kind: &str, track: TrackId| Args::new().with("kind", kind).with("track", track);
    let band = |band: usize, param: usize| band * eq::PER_BAND + param;
    let equaliser = studio.add_device(&device::ADD, add("eq", vocal));
    // A low cut at 80 Hz, and 3 dB more at 2.5 kHz.
    studio.set(equaliser, band(0, eq::ON), 1.0);
    studio.set(equaliser, band(0, eq::FREQ), 80.0);
    studio.set(equaliser, band(4, eq::GAIN), 3.0);
    let squash = studio.add_device(&device::ADD, add("compressor", vocal));
    studio.set(squash, compressor::THRESHOLD, -18.0);
    studio.set(squash, compressor::RATIO, 3.0);
    let room = studio.new_track(&track::NEW_RETURN_TRACK, "Reverb");
    let reverb = studio.add_device(&device::ADD, add("reverb", room));
    studio.set(reverb, reverb::MIX, 100.0);
    for (track, level) in [(keys, 0.35), (vocal, 0.25)] {
        let args = Args::new()
            .with("track", track)
            .with("to", room)
            .with("level", level);
        studio.run(&mixer::SET_SEND_LEVEL, args);
    }
    let args = Args::new().with("track", keys).with("gain", 0.8);
    studio.run(&mixer::SET_VOLUME, args);
    let args = Args::new()
        .with("param", ParamAddress::Volume(Chain::Track(vocal)))
        .with("at", vec![Beats::ZERO, beats(4), beats(6), beats(8)])
        .with("values", vec![1.0, 1.0, 0.5, 1.0]);
    studio.run(&automation::SET_POINTS, args);

    let project = studio.session.project();
    let track = project.track(vocal).unwrap();
    let devices: Vec<DeviceId> = track.devices.iter().map(|device| device.id).collect();
    assert_eq!(devices, [equaliser, squash]);
    assert_eq!(project.track(room).unwrap().devices[0].id, reverb);
    for track in [keys, vocal] {
        let sends = &project.track(track).unwrap().sends;
        assert_eq!(sends.len(), 1, "{track:?} sends to the reverb");
    }
    assert_eq!(project.automation.len(), 1);
}

/// Undoes every step back to the empty project, which plays silence, then
/// redoes them all: the song is back, and plays as it did.
fn undo_and_redo(studio: &mut Studio, finished: &Project) {
    let mut steps = Vec::new();
    while let Some(step) = studio.session.undo_name() {
        steps.push(step.to_owned());
        studio.run(&edit::UNDO, Args::new());
    }
    assert!(steps.len() > 20, "{steps:?}");
    assert_eq!(steps.last().map(String::as_str), Some("New MIDI Track"));
    assert!(steps.iter().any(|step| step == "Record"), "{steps:?}");
    assert_eq!(without_ids(studio.session.project()), Project::new());
    let silent = studio.play_song(BAR);
    assert!(
        silent.iter().all(|&sample| sample == 0.0),
        "it isn't silent"
    );

    for _ in &steps {
        assert!(studio.session.redo_name().is_some());
        studio.run(&edit::REDO, Args::new());
    }
    assert_eq!(studio.session.redo_name(), None);
    assert_eq!(studio.session.project(), finished);
    // A tenth of a second for anything redoing ramped.
    for _ in 0..40 {
        studio.play();
    }
    let played = studio.play_song(SONG + TAIL);
    let mut fresh = player(studio, finished, &Render::Reference);
    let expected = render(&mut fresh, SONG + TAIL);
    assert_same(&played, &expected, "the song after redoing it");
}

/// Saves the project, as File > Save does, and opens it again: it's as it
/// was, and saving it again writes the same file.
fn save_and_reopen(studio: &mut Studio) -> Project {
    let path = studio.folder.join("Demo.musicable");
    let saved = studio.session.saved_project();
    project::save(&path, &saved).unwrap();
    studio.session.mark_saved();
    assert!(!studio.session.is_modified());
    let reopened = project::load(&path).unwrap();
    assert_eq!(reopened, saved);
    let again = studio.folder.join("Demo again.musicable");
    project::save(&again, &reopened).unwrap();
    assert_eq!(fs::read(&again).unwrap(), fs::read(&path).unwrap());
    reopened
}

/// The parallel scheduler, as the app plays and sharing every block, plays
/// the song as the reference renderer does, sample for sample.
fn play_alike(studio: &Studio, project: &Project) {
    let renders = [Render::Reference, Render::App, shared()];
    let mut players: Vec<Player> = (renders.iter())
        .map(|render| player(studio, project, render))
        .collect();
    let differences = null_test::compare(&mut players, SONG + TAIL);
    assert_eq!(differences, [None, None, None]);
    let reference = &mut players[0];
    assert_eq!(reference.take_problems(), Vec::<String>::new());
    assert!(reference.peak() > 0.1, "{}", reference.peak());
    let stats = reference.stats();
    assert_eq!((stats.events_late, stats.events_dropped), (0, 0));
    let shared = players[2].parallel().unwrap();
    assert!(shared.parallel_blocks > 0);
    assert_eq!(shared.inline_blocks, 0);
}

/// Exports the song to a WAV file of floats, which holds what the app
/// plays, sample for sample.
fn export(studio: &Studio, project: &Project) {
    let settings = ExportSettings {
        format: FileFormat::Wav,
        bits: Bits::Float32,
        dither: false,
        tail: false,
        ..ExportSettings::default()
    };
    let file = studio.folder.join("Demo.wav");
    let (mixdown, stems) = files_for(&file, project, &settings, &BTreeSet::new());
    let job = Job {
        project: project.clone(),
        folder: Some(studio.folder.clone()),
        range: Span::Song.range(project, &Editor::default()).unwrap(),
        settings,
        config: CONFIG,
        installed: studio.installed.clone(),
        waker: None,
        mixdown,
        stems,
    };
    let report = carry_out(job);
    assert_eq!((report.failed.as_deref(), report.cancelled), (None, false));
    assert_eq!(report.problems, Vec::<String>::new());
    let [written] = &report.files[..] else {
        panic!("one file: {:?}", report.files);
    };
    assert_eq!((&written.path, written.frames), (&file, SONG));
    let seconds = SONG as f64 / f64::from(CONFIG.sample_rate);
    eprintln!(
        "Exported {seconds} s in {:.2} s, {:.1}x real time",
        report.took.as_secs_f64(),
        seconds / report.took.as_secs_f64().max(1e-9)
    );

    let mut decoder = Decoder::open(&file).unwrap();
    assert_eq!(decoder.info().sample_rate, CONFIG.sample_rate);
    let planar = decoder.read_all().unwrap();
    let exported: Vec<f32> = (0..planar[0].len())
        .flat_map(|i| [planar[0][i], planar[1][i]])
        .collect();
    let mut app = player(studio, project, &Render::App);
    let played = render(&mut app, SONG);
    assert_same(&exported, &played, "the export");
    assert_eq!(
        written.peak,
        played.iter().fold(0.0, |peak, s| s.abs().max(peak))
    );
}

/// Carries `job` out, ticking as the UI would, and says how it went.
fn carry_out(job: Job) -> Report {
    let mut export = Export::start(job).unwrap();
    while !export.is_finished() {
        export.tick();
        thread::sleep(Duration::from_millis(1));
    }
    export.report().unwrap().clone()
}
