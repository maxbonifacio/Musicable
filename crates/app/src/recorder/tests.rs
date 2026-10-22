use std::fs;

use musicable_audio::InputWriter;
use musicable_io::audio::Decoder;

use super::*;

const RATE: u32 = 48_000;

/// A folder of the test's own, empty.
fn folder(name: &str) -> PathBuf {
    let folder =
        std::env::temp_dir().join(format!("musicable-recorder-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// What the input captured as engine frame `w` was heard, on channel `c`: a
/// ramp, so where each sample of a recording came from shows.
fn captured(w: u64, c: usize) -> f32 {
    (w * 2 + c as u64) as f32
}

/// A stereo input that captured from engine frame `start` up to `end`.
fn input(start: u64, end: u64) -> (InputBuffer, InputWriter) {
    let (buffer, mut writer) = InputBuffer::new(2, 1 << 16);
    writer.begin(start);
    capture(&mut writer, start, end);
    (buffer, writer)
}

fn capture(writer: &mut InputWriter, from: u64, to: u64) {
    let samples: Vec<f32> = (from..to)
        .flat_map(|w| [captured(w, 0), captured(w, 1)])
        .collect();
    writer.append(&samples);
}

/// Lead, track 1, recording the default input in stereo, which is `offset`
/// frames late.
fn lead(offset: i64) -> Armed {
    Armed {
        track: TrackId(1),
        name: "Lead".to_owned(),
        input: AudioInput::default(),
        offset,
    }
}

/// Where a project saved in `folder` records.
fn place(folder: &Path) -> Option<Place> {
    Some(Place {
        folder: folder.join("Recordings"),
        project: Some(folder.to_owned()),
    })
}

/// A tape recording `armed` from the stereo input `mic`, the default, into
/// the project folder `folder`.
fn tape(folder: &Path, buffer: &InputBuffer, armed: Vec<Armed>) -> Tape {
    let mut tape = Tape::new();
    let mut layout = InputLayout::new();
    layout.add("mic", 0, 2);
    layout.set_default("mic");
    tape.set_inputs(layout, vec![Some(buffer.clone())]);
    tape.set_tracks(RATE, armed, place(folder), 1);
    tape
}

fn stopped(time: u64, position: u64) -> Segment {
    Segment {
        time,
        position,
        ..Segment::default()
    }
}

/// Playing and recording, with the graph `latency` frames late.
fn recording(time: u64, position: u64, latency: u64) -> Segment {
    Segment {
        time,
        position,
        playing: true,
        record: true,
        recording: true,
        latency,
    }
}

/// Playing with recording on, outside the punch range.
fn punched_out(time: u64, position: u64) -> Segment {
    Segment {
        recording: false,
        ..recording(time, position, 0)
    }
}

/// Gives `tape` the timeline's `segments`, which go up to engine time
/// `until`, lets it write, and returns what it reported.
fn run(tape: &mut Tape, segments: &[Segment], until: u64) -> Vec<Report> {
    tape.add(segments, Read { until, lost: false });
    tape.step();
    tape.take_reports()
}

/// What a recording that finished recorded, and its files.
type Finished = (Vec<Recorded>, Vec<(String, AudioFile)>);

/// The recordings that finished, and the problems, in `reports`.
fn split(reports: Vec<Report>) -> (Vec<Finished>, Vec<String>) {
    let mut finished = Vec::new();
    let mut problems = Vec::new();
    for report in reports {
        match report {
            Report::Finished {
                project,
                rate,
                recorded,
                files,
            } => {
                assert_eq!((project, rate), (1, RATE));
                finished.push((recorded, files));
            }
            Report::Problem(problem) => problems.push(problem),
        }
    }
    (finished, problems)
}

/// The one recording that finished, if nothing went wrong.
fn finished(reports: Vec<Report>) -> Finished {
    let (mut finished, problems) = split(reports);
    assert_eq!(problems, Vec::<String>::new());
    assert_eq!(finished.len(), 1, "{finished:?}");
    finished.remove(0)
}

/// Lead's passes, in the one file it recorded.
fn passes(file: &str, passes: &[(u64, u64, u64)]) -> Vec<Recorded> {
    let passes = passes
        .iter()
        .map(|&(start, frames, offset)| Pass {
            start,
            frames,
            file: file.to_owned(),
            offset,
        })
        .collect();
    vec![Recorded {
        track: TrackId(1),
        passes,
    }]
}

/// A stereo file `frames` frames long.
fn stereo(name: &str, frames: u64) -> Vec<(String, AudioFile)> {
    let file = AudioFile {
        sample_rate: RATE,
        frames,
        channels: 2,
    };
    vec![(name.to_owned(), file)]
}

/// A recorded file's frames, each with the input frame it holds, or `None`
/// for silence.
fn frames(path: &Path) -> Vec<Option<u64>> {
    let channels = Decoder::open(path).unwrap().read_all().unwrap();
    assert_eq!(channels.len(), 2);
    (0..channels[0].len())
        .map(|i| {
            let (left, right) = (channels[0][i], channels[1][i]);
            if (left, right) == (0.0, 0.0) {
                return None;
            }
            let w = (left / 2.0) as u64;
            assert_eq!((left, right), (captured(w, 0), captured(w, 1)));
            Some(w)
        })
        .collect()
}

/// `count` input frames from `w` on.
fn from(w: u64, count: u64) -> Vec<Option<u64>> {
    (w..w + count).map(Some).collect()
}

fn silence(count: u64) -> Vec<Option<u64>> {
    vec![None; count as usize]
}

#[test]
fn a_recording_lands_where_it_was_played() {
    let folder = folder("lands");
    let (buffer, _writer) = input(0, 4000);
    let mut tape = tape(&folder, &buffer, vec![lead(5)]);
    // Song positions 5,000 to 7,000 played from engine time 1,000, were
    // heard 64 frames later, and the input has them 5 frames after that.
    let segments = [
        stopped(0, 0),
        recording(1000, 5000, 64),
        stopped(3000, 7000),
    ];
    let (recorded, files) = finished(run(&mut tape, &segments, 3100));
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(5000, 2000, 0)]));
    assert_eq!(files, stereo(name, 2000));
    assert_eq!(frames(&folder.join(name)), from(1069, 2000));
    assert!(tape.live().is_empty());
}

#[test]
fn each_time_round_the_loop_is_a_pass() {
    let folder = folder("loop");
    let (buffer, _writer) = input(0, 2000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let segments = [
        recording(0, 100, 0),
        recording(500, 100, 0),
        stopped(900, 500),
    ];
    let (recorded, files) = finished(run(&mut tape, &segments, 1000));
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(100, 500, 0), (100, 400, 500)]));
    assert_eq!(files, stereo(name, 900));
    assert_eq!(frames(&folder.join(name)), from(0, 900));
}

#[test]
fn punching_in_and_out_records_the_punch_range() {
    let folder = folder("punch");
    let (buffer, _writer) = input(0, 2000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    // Twice round a loop from 0 to 1,000, punching in at 300 and out at
    // 600.
    let segments = [
        punched_out(0, 0),
        recording(300, 300, 0),
        punched_out(600, 600),
        punched_out(1000, 0),
        recording(1300, 300, 0),
        punched_out(1600, 600),
        stopped(1700, 700),
    ];
    let (recorded, _) = finished(run(&mut tape, &segments, 1800));
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(300, 300, 0), (300, 300, 300)]));
    let mut expected = from(300, 300);
    expected.extend(from(1300, 300));
    assert_eq!(frames(&folder.join(name)), expected);
}

#[test]
fn a_recording_waits_for_its_input() {
    let folder = folder("waits");
    let (buffer, mut writer) = input(0, 500);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let segments = [recording(0, 0, 0), stopped(1000, 1000)];
    assert!(run(&mut tape, &segments, 1100).is_empty());
    let live = Recording {
        track: TrackId(1),
        start: 0,
        frames: 500,
    };
    assert_eq!(tape.live(), [live]);
    capture(&mut writer, 500, 1000);
    let (recorded, _) = finished(run(&mut tape, &[], 1200));
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(0, 1000, 0)]));
    assert_eq!(frames(&folder.join(name)), from(0, 1000));
}

#[test]
fn what_an_input_never_captured_is_silent() {
    let folder = folder("never");
    // It started late and stopped early.
    let (buffer, writer) = input(100, 600);
    drop(writer);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let segments = [recording(0, 0, 0), stopped(1000, 1000)];
    let (mut finished, problems) = split(run(&mut tape, &segments, 1100));
    let silent = "0.01 seconds of Lead's recording are silent: its input didn't have them.";
    assert_eq!(problems, [silent]);
    let (recorded, _) = finished.remove(0);
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(0, 1000, 0)]));
    let mut expected = silence(100);
    expected.extend(from(100, 500));
    expected.extend(silence(400));
    assert_eq!(frames(&folder.join(name)), expected);
}

#[test]
fn an_input_that_stalls_is_given_up_on() {
    let folder = folder("stalls");
    // It captures nothing after frame 1,000, and doesn't end.
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let segments = [recording(0, 0, 0), stopped(2000, 2000)];
    let patience = u64::from(RATE) * PATIENCE_SECONDS;
    assert!(run(&mut tape, &segments, 1000 + patience).is_empty());
    let (finished, problems) = split(run(&mut tape, &[], 1001 + patience));
    assert_eq!(finished.len(), 1);
    assert_eq!(problems.len(), 1, "{problems:?}");
    let mut expected = from(0, 1000);
    expected.extend(silence(1000));
    assert_eq!(frames(&folder.join("Recordings/Lead 1.wav")), expected);
}

#[test]
fn recordings_one_after_another_have_files_of_their_own() {
    let folder = folder("after");
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let segments = [
        recording(0, 0, 0),
        stopped(100, 100),
        recording(200, 0, 0),
        stopped(300, 100),
    ];
    let (finished, problems) = split(run(&mut tape, &segments, 400));
    assert!(problems.is_empty());
    let names: Vec<_> = finished
        .iter()
        .map(|(_, files)| files[0].0.as_str())
        .collect();
    assert_eq!(names, ["Recordings/Lead 1.wav", "Recordings/Lead 2.wav"]);
    assert_eq!(finished[1].0, passes(names[1], &[(0, 100, 0)]));
    assert_eq!(frames(&folder.join(names[1])), from(200, 100));
}

#[test]
fn a_full_file_goes_on_in_another() {
    let folder = folder("full");
    let (buffer, _writer) = input(0, 2000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    // Files that fill up at 400 frames, as at WAV's 4 GB.
    tape.file_frames = 400;
    let segments = [recording(0, 0, 0), stopped(1000, 1000)];
    let (recorded, files) = finished(run(&mut tape, &segments, 1100));
    let parts = [(0, 400), (400, 400), (800, 200)];
    let names = parts.map(|(start, _)| format!("Recordings/Lead {}.wav", start / 400 + 1));
    let passes = parts
        .iter()
        .zip(&names)
        .map(|(&(start, frames), name)| Pass {
            start,
            frames,
            file: name.clone(),
            offset: 0,
        });
    let expected = Recorded {
        track: TrackId(1),
        passes: passes.collect(),
    };
    assert_eq!(recorded, [expected]);
    let sizes: Vec<_> = files
        .iter()
        .map(|(name, file)| (name, file.frames))
        .collect();
    assert_eq!(
        sizes,
        [(&names[0], 400), (&names[1], 400), (&names[2], 200)]
    );
    for (&(start, count), name) in parts.iter().zip(&names) {
        assert_eq!(frames(&folder.join(name)), from(start, count));
    }
}

#[test]
fn a_recording_without_tracks_doesnt_hold_up_the_next() {
    let folder = folder("trackless");
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, Vec::new());
    let segments = [recording(0, 0, 0), stopped(100, 100)];
    assert!(run(&mut tape, &segments, 150).is_empty());
    tape.set_tracks(RATE, vec![lead(0)], place(&folder), 1);
    let segments = [recording(200, 0, 0), stopped(300, 100)];
    let (recorded, _) = finished(run(&mut tape, &segments, 350));
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(0, 100, 0)]));
    assert_eq!(frames(&folder.join(name)), from(200, 100));
}

#[test]
fn an_armed_track_whose_input_isnt_open_records_nothing() {
    let folder = folder("closed");
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    // A layout without the default input.
    let mut layout = InputLayout::new();
    layout.add("mic", 0, 2);
    tape.set_inputs(layout, vec![Some(buffer)]);
    let segments = [recording(0, 0, 0), stopped(100, 100)];
    let (finished, problems) = split(run(&mut tape, &segments, 150));
    assert!(finished.is_empty());
    assert_eq!(problems, ["Lead records nothing: its input isn't open."]);
}

#[test]
fn cutting_the_timeline_stops_a_recording_where_it_got_to() {
    let folder = folder("cut");
    let (buffer, _writer) = input(0, 300);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    assert!(run(&mut tape, &[recording(0, 0, 0)], 500).is_empty());
    // Such as when the engine is replaced: the frames the input hasn't
    // captured are silent.
    tape.cut();
    let (mut finished, problems) = split(tape.take_reports());
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (recorded, _) = finished.remove(0);
    let name = "Recordings/Lead 1.wav";
    assert_eq!(recorded, passes(name, &[(0, 500, 0)]));
    let mut expected = from(0, 300);
    expected.extend(silence(200));
    assert_eq!(frames(&folder.join(name)), expected);
    // The next timeline starts at 0.
    assert!(run(&mut tape, &[stopped(0, 0)], 100).is_empty());
}

#[test]
fn losing_track_of_the_transport_stops_recording() {
    let folder = folder("lost");
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    assert!(run(&mut tape, &[recording(0, 0, 0)], 300).is_empty());
    tape.add(
        &[stopped(400, 400)],
        Read {
            until: 500,
            lost: true,
        },
    );
    let (mut finished, problems) = split(tape.take_reports());
    let lost = "Recording stopped: the recorder lost track of the transport.";
    assert_eq!(problems, [lost]);
    let (recorded, _) = finished.remove(0);
    assert_eq!(recorded, passes("Recordings/Lead 1.wav", &[(0, 300, 0)]));
}

#[test]
fn a_project_that_isnt_saved_names_its_recordings_by_their_paths() {
    let folder = folder("unsaved");
    let (buffer, _writer) = input(0, 1000);
    let mut tape = tape(&folder, &buffer, vec![lead(0)]);
    let place = Place {
        folder: folder.clone(),
        project: None,
    };
    tape.set_tracks(RATE, vec![lead(0)], Some(place), 1);
    let segments = [recording(0, 0, 0), stopped(100, 100)];
    let (_, files) = finished(run(&mut tape, &segments, 150));
    let path = std::path::absolute(folder.join("Lead 1.wav")).unwrap();
    assert_eq!(files[0].0, path.to_string_lossy());
}

#[test]
fn recordings_are_named_after_their_tracks() {
    assert_eq!(file_stem("Lead"), "Lead");
    assert_eq!(file_stem("Vox: take/2?"), "Vox_ take_2_");
    assert_eq!(file_stem(" Pad. "), "Pad");
    assert_eq!(file_stem("..."), "Recording");
    assert_eq!(file_stem(""), "Recording");
}

#[test]
fn the_recorders_thread_stops_cleanly() {
    let recorder = Recorder::spawn();
    recorder.set_tracks(RATE, vec![lead(0)], None, 1);
    assert!(recorder.try_recv().is_none());
    assert!(recorder.live().is_empty());
    drop(recorder);
}
