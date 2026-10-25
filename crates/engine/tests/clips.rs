//! Clips: audio clips loaded whole and streaming from the disk thread,
//! resampled, fading and crossfading, warped, and MIDI clips whose notes,
//! with their expression, play a test instrument. A project of them plays the same with
//! the parallel renderer as with the reference renderer, looping, locating
//! and in blocks of any size. Audio clips play what the song position says,
//! however the playhead got there. Streams don't allocate on the audio
//! thread, waiting for the disk or not.

use std::f64::consts::TAU;
use std::io;
use std::sync::Arc;
use std::thread;

use musicable_engine::media::{AudioData, AudioReader, Media, Source, Stream, Streamer};
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Event, Meters, ParallelOptions, ParallelRenderer,
    ReferenceRenderer, Renderer, TransportCommand, compile_with_media, rt,
};
use musicable_model::clip::{AudioClip, Clip, ClipContent, MidiClip, Offset, Point};
use musicable_model::param::ClipParam;
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{Warp, WarpMarker};
use musicable_model::{DeviceKind, Project, TestTone, TrackId, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 256,
};

/// The song's frames to a beat, at 120 BPM.
const BEAT: usize = 24_000;

/// An audio file in memory: a sine in each channel, each a different one,
/// with a click every 1,000 frames, so a frame out of place shows.
#[derive(Debug, Clone, Copy)]
struct File {
    channels: usize,
    sample_rate: u32,
    frames: u64,
}

impl File {
    fn sample(&self, frame: u64, channel: usize) -> f32 {
        let frequency = 110.0 * (channel + 2) as f64 + f64::from(self.sample_rate) / 4_800.0;
        let t = frame as f64 / f64::from(self.sample_rate);
        let click = if frame.is_multiple_of(1_000) {
            0.25
        } else {
            0.0
        };
        (TAU * frequency * t).sin() as f32 * 0.5 + click
    }
}

impl AudioReader for File {
    fn channels(&self) -> usize {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn frames(&self) -> u64 {
        self.frames
    }

    fn read(&mut self, start: u64, out: &mut [f32]) -> io::Result<usize> {
        let frames = (out.len() / self.channels).min(self.frames.saturating_sub(start) as usize);
        for (i, frame) in out.chunks_exact_mut(self.channels).take(frames).enumerate() {
            for (c, sample) in frame.iter_mut().enumerate() {
                *sample = self.sample(start + i as u64, c);
            }
        }
        Ok(frames)
    }
}

/// How a clip's file reaches its player.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Load {
    Whole,
    Stream,
}

/// A project's audio clips: their files, and how each is loaded.
struct Files(Vec<(Clip, File, Load)>);

impl Files {
    /// The clips' sources, streaming from `streamer`. Each run gets its own.
    fn media(&self, streamer: &Streamer) -> (Media, Vec<Arc<Stream>>) {
        let mut media = Media::new();
        let mut streams = Vec::new();
        for &(ref clip, mut file, load) in &self.0 {
            let source = match load {
                Load::Whole => Source::Whole(Arc::new(AudioData::read(&mut file).unwrap())),
                Load::Stream => {
                    let stream = streamer.stream(Box::new(file));
                    streams.push(stream.clone());
                    Source::Stream(stream)
                }
            };
            media.insert(clip.id, source);
        }
        (media, streams)
    }
}

fn beats(numerator: i64, denominator: i64) -> Beats {
    Beats::from_ratio(numerator, denominator)
}

/// Adds an audio clip of `file` to `track`, from beat `start` for `length`.
#[allow(clippy::too_many_arguments)]
fn audio_clip(
    project: &mut Project,
    files: &mut Files,
    track: TrackId,
    (start, length): (Beats, Beats),
    file: File,
    load: Load,
    edit: impl FnOnce(&mut AudioClip),
) {
    let mut audio = AudioClip::new("in memory");
    edit(&mut audio);
    let range = BeatRange::new(start, start + length);
    let clip = project.new_clip("", range, ClipContent::Audio(audio));
    files.0.push((clip.clone(), file, load));
    project.track_mut(track).unwrap().clips.push(clip);
}

/// Three audio tracks: on the first, a clip loaded whole crossfading into
/// one that streams, on the second, a test tone under two clips of files at
/// other sample rates, one streaming, and on the third, a warped clip that
/// streams, its file playing at twice the song's speed, then at it, then at
/// a third of it, as its gain rises.
fn audio_tracks(project: &mut Project, files: &mut Files) {
    let drums = project.add_track("Drums", TrackKind::Audio);
    let kick = File {
        channels: 2,
        sample_rate: 48_000,
        frames: (BEAT * 3 / 2) as u64,
    };
    audio_clip(
        project,
        files,
        drums,
        (beats(0, 1), beats(2, 1)),
        kick,
        Load::Whole,
        |a| {
            a.fade_out = beats(1, 2);
        },
    );
    let take = File {
        channels: 1,
        sample_rate: 48_000,
        frames: (BEAT * 10) as u64,
    };
    let span = (beats(3, 2), beats(6, 1));
    audio_clip(project, files, drums, span, take, Load::Stream, |a| {
        a.offset = Offset::Frames(1_000);
        a.gain = 0.8;
        a.fade_in = beats(1, 2);
        a.fade_out = beats(1, 4);
    });

    let pad = project.add_track("Pad", TrackKind::Audio);
    project.track_mut(pad).unwrap().tone = Some(TestTone {
        frequency: 97.0,
        level: 0.05,
    });
    let strings = File {
        channels: 2,
        sample_rate: 44_100,
        frames: 44_100 * 4,
    };
    let span = (beats(1, 1), beats(5, 1));
    audio_clip(project, files, pad, span, strings, Load::Stream, |a| {
        a.offset = Offset::Frames(500);
        a.gain = 0.5;
        a.fade_in = beats(1, 1);
    });
    let hit = File {
        channels: 1,
        sample_rate: 96_000,
        frames: 4_000,
    };
    let span = (beats(13, 4), beats(1, 2));
    audio_clip(project, files, pad, span, hit, Load::Whole, |_| {});

    let groove = project.add_track("Groove", TrackKind::Audio);
    let breaks = File {
        channels: 2,
        sample_rate: 48_000,
        frames: 72_000,
    };
    let span = (beats(2, 1), beats(3, 1));
    audio_clip(project, files, groove, span, breaks, Load::Stream, |a| {
        let marker = |frame, beat| WarpMarker { frame, beat };
        a.warp = Some(Warp {
            markers: vec![
                marker(0, beats(0, 1)),
                marker(24_000, beats(1, 2)),
                marker(48_000, beats(3, 2)),
                marker(60_000, beats(3, 1)),
            ],
            ..Warp::new(0, Beats::ZERO, 120.0)
        });
        a.offset = Offset::Beats(beats(1, 8));
        a.fade_out = beats(1, 4);
        let point = musicable_model::automation::Point::new;
        a.set_lane(ClipParam::Gain, vec![point(beats(1, 4), 0.2), point(beats(5, 4), 1.0)]);
    });
}

/// A MIDI track playing a test instrument: chords with per-note expression
/// in a clip that starts part way into its notes, and a bent lead note.
fn midi_track(project: &mut Project) {
    let keys = project.add_track("Keys", TrackKind::Midi);
    let mut chords = MidiClip {
        offset: beats(1, 4),
        ..MidiClip::default()
    };
    // Before the offset, so it doesn't play.
    chords.add_note(beats(0, 1), beats(1, 1), 48, 1.0);
    chords.add_note(beats(1, 4), beats(1, 1), 60, 0.5);
    let bent = chords.add_note(beats(1, 2), beats(3, 2), 64, 0.75);
    bent.expression.pitch_bend = vec![
        Point::new(beats(0, 1), 0.0),
        Point::new(beats(1, 4), 2.0),
        Point::new(beats(1, 2), -1.0),
    ];
    bent.expression.pressure = vec![Point::new(beats(0, 1), 0.2), Point::new(beats(1, 2), 0.9)];
    bent.expression.timbre = vec![Point::new(beats(1, 4), 0.7)];
    // Still sounding when the clip ends, which stops it.
    chords.add_note(beats(2, 1), beats(3, 1), 67, 1.0);
    let range = BeatRange::new(beats(1, 2), beats(9, 2));
    let chords = project.new_clip("Chords", range, ClipContent::Midi(chords));

    let mut lead = MidiClip::default();
    let note = lead.add_note(beats(0, 1), beats(3, 2), 72, 0.9);
    note.release_velocity = 0.2;
    note.expression.pitch_bend = vec![Point::new(beats(1, 8), 0.5), Point::new(beats(1, 1), 12.0)];
    let range = BeatRange::new(beats(4, 1), beats(6, 1));
    let lead = project.new_clip("Lead", range, ClipContent::Midi(lead));

    let gain = project.new_device(DeviceKind::TestGain { gain: 0.5 });
    let instrument = project.new_device(DeviceKind::TestInstrument { level: 0.3 });
    let track = project.track_mut(keys).unwrap();
    track.clips = vec![lead, chords];
    track.devices = vec![gain, instrument];
}

/// Plays `project` from the start through `renderer`, in device callbacks of
/// uneven sizes, looping over beats 1 to 5, with a locate into the
/// crossfade, another just before the loop's end, and a stop and a start,
/// and returns what it played. Streams wait for the disk.
fn session(project: &Project, files: &Files, renderer: Box<dyn Renderer>) -> Vec<f32> {
    let streamer = Streamer::spawn(true);
    let (media, streams) = files.media(&streamer);
    let graph = compile_with_media(project, &Meters::new(), &media)
        .prepare(&CONFIG)
        .unwrap();
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, renderer);
    handle.swap_graph(graph).unwrap();
    let transport = |command| Event::Transport(command);
    let looping = BeatRange::new(beats(1, 1), beats(5, 1));
    handle
        .send(transport(TransportCommand::Loop(Some(looping))))
        .unwrap();
    handle.send(transport(TransportCommand::Play)).unwrap();
    let mut played = Vec::new();
    let mut device = [0.0; 2 * 500];
    let sizes = [500, 64, 333, 7, 128, 500, 250, 41, 256];
    for (callback, &frames) in sizes.iter().cycle().take(1_400).enumerate() {
        let command = match callback {
            40 => Some(TransportCommand::Locate(beats(7, 4))),
            300 => Some(TransportCommand::Stop),
            303 => Some(TransportCommand::Play),
            500 => Some(TransportCommand::Locate(beats(49, 10))),
            _ => None,
        };
        if let Some(command) = command {
            handle.send(transport(command)).unwrap();
        }
        let device = &mut device[..2 * frames];
        engine.process(&mut AudioMut::from_planar(device, 2));
        played.extend_from_slice(device);
    }
    // It played for more than three times round the loop, and it's still
    // in it.
    assert!(played.len() / 2 > 3 * 4 * BEAT);
    assert!((BEAT..5 * BEAT).contains(&(handle.stats().position as usize)));
    for stream in &streams {
        assert_eq!((stream.missed(), stream.failed()), (0, false));
    }
    played
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn clips_play_the_same_with_the_parallel_renderer() {
    let mut project = Project::new();
    let mut files = Files(Vec::new());
    audio_tracks(&mut project, &mut files);
    midi_track(&mut project);
    let expected = session(&project, &files, Box::new(ReferenceRenderer));
    assert!(expected.iter().any(|&s| s.abs() > 0.1), "a quiet test");
    for threads in [1, 4] {
        let renderer = ParallelRenderer::new(ParallelOptions {
            threads,
            inline_below: None,
            realtime: false,
            ..ParallelOptions::default()
        });
        let played = session(&project, &files, Box::new(renderer));
        assert!(same(&played, &expected), "{threads} workers");
    }
    // Streaming plays what loading the files whole would.
    let whole = Files(
        files
            .0
            .iter()
            .map(|(c, f, _)| (c.clone(), *f, Load::Whole))
            .collect(),
    );
    let played = session(&project, &whole, Box::new(ReferenceRenderer));
    assert!(same(&played, &expected), "loaded whole");
    // And the notes and the warped clip play: without them, it's another
    // song.
    for name in ["Keys", "Groove"] {
        let mut unplayed = project.clone();
        let track = unplayed.tracks.iter_mut().find(|t| t.name == name).unwrap();
        track.clips.clear();
        let played = session(&unplayed, &files, Box::new(ReferenceRenderer));
        assert!(!same(&played, &expected), "without {name}");
    }
}

/// Renders `frames` frames of `engine` into a vector, a channel after the
/// other, in callbacks of at most 300 frames.
fn render(engine: &mut Engine, frames: usize) -> [Vec<f32>; 2] {
    let mut channels = [Vec::new(), Vec::new()];
    let mut device = [0.0; 2 * 300];
    let mut done = 0;
    while done < frames {
        let block = (frames - done).min(300);
        let device = &mut device[..2 * block];
        engine.process(&mut AudioMut::from_planar(device, 2));
        let (left, right) = device.split_at(block);
        channels[0].extend_from_slice(left);
        channels[1].extend_from_slice(right);
        done += block;
    }
    channels
}

#[test]
fn audio_clips_play_their_song_position_however_the_playhead_got_there() {
    let mut project = Project::new();
    let mut files = Files(Vec::new());
    audio_tracks(&mut project, &mut files);
    let streamer = Streamer::spawn(true);
    let engine = || {
        let (media, _) = files.media(&streamer);
        let graph = compile_with_media(&project, &Meters::new(), &media)
            .prepare(&CONFIG)
            .unwrap();
        let (engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        (engine, handle)
    };
    // Straight through, from the start of the song to beat 6.
    let (mut straight, _handle) = engine();
    let song = render(&mut straight, 6 * BEAT);
    assert!(
        song[0][7 * BEAT / 4..2 * BEAT]
            .iter()
            .any(|&s| s.abs() > 0.1)
    );

    // Located into the crossfade, a clip's start, the end of the file loaded
    // whole and a third of a beat: the same samples.
    for at in [beats(7, 4), beats(1, 1), beats(3, 2), beats(10, 3)] {
        let (mut located, mut handle) = engine();
        handle
            .send(Event::Transport(TransportCommand::Locate(at)))
            .unwrap();
        let frame = project.tempo.frame_at(at, CONFIG.sample_rate) as usize;
        let played = render(&mut located, BEAT);
        for c in 0..2 {
            assert!(same(&played[c], &song[c][frame..frame + BEAT]), "at {at:?}");
        }
    }

    // Looping from beat 4.5 to 5, then from 1: each time round the same.
    let (mut looped, mut handle) = engine();
    let region = BeatRange::new(beats(1, 1), beats(5, 1));
    handle
        .send(Event::Transport(TransportCommand::Loop(Some(region))))
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Locate(beats(9, 2))))
        .unwrap();
    let played = render(&mut looped, BEAT / 2 + 2 * 4 * BEAT);
    for c in 0..2 {
        let (before, after) = played[c].split_at(BEAT / 2);
        assert!(same(before, &song[c][9 * BEAT / 2..5 * BEAT]));
        for lap in after.chunks(4 * BEAT) {
            assert!(same(lap, &song[c][BEAT..5 * BEAT]));
        }
    }
}

#[test]
fn real_time_streams_play_what_the_disk_has_loaded_without_allocating() {
    let mut project = Project::new();
    let mut files = Files(Vec::new());
    audio_tracks(&mut project, &mut files);
    let play = |wait: bool| {
        let streamer = Streamer::spawn(wait);
        let (media, streams) = files.media(&streamer);
        let graph = compile_with_media(&project, &Meters::new(), &media)
            .prepare(&CONFIG)
            .unwrap();
        let (mut engine, mut handle) = Engine::new(CONFIG);
        handle.swap_graph(graph).unwrap();
        // The players say where they'll read before the transport plays.
        render(&mut engine, 1);
        handle
            .send(Event::Transport(TransportCommand::Play))
            .unwrap();
        let mut played = [Vec::new(), Vec::new()];
        for _ in 0..300 {
            // As a device would, leaving the disk thread time between
            // callbacks: here as much as it needs for the next one.
            while !wait && !files.loaded(&streams, handle.stats().position, 256) {
                thread::yield_now();
            }
            let block = render(&mut engine, 256);
            for c in 0..2 {
                played[c].extend_from_slice(&block[c]);
            }
        }
        let missed: u64 = streams.iter().map(|s| s.missed()).sum();
        (played, missed, streams, engine, handle)
    };
    let (waited, _, _, _, _) = play(true);
    let (real_time, missed, streams, mut engine, mut handle) = play(false);
    assert_eq!(missed, 0);
    for c in 0..2 {
        assert!(same(&real_time[c], &waited[c]));
    }
    assert!(waited[0].iter().any(|&s| s.abs() > 0.1), "a quiet test");
    // A locate the disk hasn't caught up with may play silence for a
    // moment, and still doesn't allocate.
    handle
        .send(Event::Transport(TransportCommand::Locate(beats(5, 1))))
        .unwrap();
    render(&mut engine, 256);
    assert!(streams.iter().map(|s| s.missed()).sum::<u64>() >= missed);
}

impl Files {
    /// Whether the streams, in the order of the clips that stream, have the
    /// frames their clips read for the `frames` song frames from `position`,
    /// with a margin for the resampler.
    fn loaded(&self, streams: &[Arc<Stream>], position: u64, frames: u64) -> bool {
        let rate = f64::from(CONFIG.sample_rate);
        let streaming = self.0.iter().filter(|(_, _, load)| *load == Load::Stream);
        streaming.zip(streams).all(|((clip, file, _), stream)| {
            let ClipContent::Audio(audio) = &clip.content else {
                unreachable!("an audio clip");
            };
            let frame = |at: Beats| (at.to_f64() * BEAT as f64) as u64;
            let (start, end) = (frame(clip.start), frame(clip.end()));
            if position >= end {
                return true;
            }
            // The file's frame on a song frame of the clip.
            let file_frame = |song: u64| match (&audio.warp, audio.offset) {
                (Some(warp), Offset::Beats(offset)) => {
                    let own = offset.to_f64() + song as f64 / BEAT as f64 - clip.start.to_f64();
                    warp.frame_at(own, file.sample_rate)
                }
                (_, offset) => {
                    let step = f64::from(file.sample_rate) / rate;
                    offset.frames().unwrap() as f64 + (song - start) as f64 * step
                }
            };
            let from = position.max(start);
            let first = (file_frame(from) - 64.0).max(0.0);
            let last = file_frame(from + frames) + 64.0;
            stream.is_loaded(first as u64, (last - first) as u64)
        })
    }
}
