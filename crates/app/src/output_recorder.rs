//! Recording what the audio device plays, for checks: `debug.record_output`
//! and `debug.stop_recording_output`.
//!
//! The engine copies every frame it hands the device into a ring, and logs
//! what it applies ([`musicable_engine::output_recording`]). A thread here
//! drains them every few milliseconds into a 32-bit float WAV file, so the
//! audio thread only copies. When the recording stops, the timeline goes
//! beside the file as JSON: each event the engine applied, with the frame
//! it applied it on and the frame playing when the app sent it, and each
//! dropout and short block, all as frames of the file.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use musicable_engine::compile::keys::{self, Owner};
use musicable_engine::nodes::Gain;
use musicable_engine::output_recording::{Applied, AppliedAt, Callback};
use musicable_engine::switch::Switch;
use musicable_engine::{
    EngineHandle, Event, NodeKey, NoteEvent, OutputRecording, ParamId, Sent, TransportCommand,
};
use musicable_io::audio::WavWriter;
use musicable_model::{Chain, DeviceKind, Project};
use serde_json::{Value as Json, json};

/// How often the thread drains the rings.
const EVERY: Duration = Duration::from_millis(5);

/// How long a recording that was told to stop waits for the engine to say
/// it has, as one with no device playing never will.
const LONGEST_STOP: Duration = Duration::from_secs(2);

/// A recording of the output under way.
pub(crate) struct OutputRecorder {
    file: PathBuf,
    sample_rate: u32,
    /// Set once the recording is to stop, so the thread gives up waiting
    /// for the engine after a while.
    stopping: Arc<AtomicBool>,
    thread: JoinHandle<Drained>,
}

/// What the thread drained.
#[derive(Default)]
struct Drained {
    callbacks: Vec<Callback>,
    applied: Vec<AppliedAt>,
    /// Frames written to the file.
    frames: u64,
    channels: u16,
    /// Applied events the engine couldn't log, and callbacks it couldn't
    /// note, frames and all.
    lost: u64,
    lost_callbacks: u64,
    /// Whether the engine said it had stopped writing.
    ended: bool,
    error: Option<String>,
}

impl OutputRecorder {
    /// Starts recording what the engine `handle` drives plays to `file`, a
    /// WAV file. Says why it can't.
    pub(crate) fn start(handle: &mut EngineHandle, file: &Path) -> Result<Self, String> {
        if let Some(folder) = file
            .parent()
            .filter(|folder| !folder.as_os_str().is_empty())
        {
            std::fs::create_dir_all(folder).map_err(|error| {
                format!("The folder {} can't be made: {error}", folder.display())
            })?;
        }
        let out = File::create(file)
            .map_err(|error| format!("{} can't be written: {error}", file.display()))?;
        let reader = handle
            .record_output()
            .ok_or("The engine is too busy to start recording: try again.")?;
        let sample_rate = handle.config().sample_rate;
        let stopping = Arc::new(AtomicBool::new(false));
        let thread = {
            let stopping = stopping.clone();
            thread::Builder::new()
                .name("musicable-output-recording".into())
                .spawn(move || drain(reader, out, sample_rate, &stopping))
                .map_err(|error| format!("The recording didn't start: {error}"))?
        };
        Ok(Self {
            file: file.to_owned(),
            sample_rate,
            stopping,
            thread,
        })
    }

    /// The file it records to.
    pub(crate) fn file(&self) -> &Path {
        &self.file
    }

    /// Stops recording, waits for the thread to write what the engine
    /// played up to then, and writes the timeline beside the file, naming
    /// what each event was for from `project`. Returns what it wrote, as
    /// JSON for an agent.
    pub(crate) fn stop(self, handle: &mut EngineHandle, project: &Project) -> Result<Json, String> {
        let sent = handle.stop_recording_output().unwrap_or_default();
        self.stopping.store(true, Relaxed);
        let drained = self
            .thread
            .join()
            .map_err(|_| "The recording's thread failed.".to_owned())?;
        if let Some(error) = &drained.error {
            return Err(format!("The recording failed: {error}"));
        }
        let timeline = timeline(&drained, &sent, self.sample_rate, project);
        let path = timeline_path(&self.file);
        let mut out = BufWriter::new(
            File::create(&path)
                .map_err(|error| format!("{} can't be written: {error}", path.display()))?,
        );
        serde_json::to_writer_pretty(&mut out, &timeline)
            .map_err(std::io::Error::from)
            .and_then(|()| out.flush())
            .map_err(|error| format!("{} can't be written: {error}", path.display()))?;
        let count = |key: &str| timeline[key].as_array().map_or(0, Vec::len);
        Ok(json!({
            "file": self.file,
            "timeline": path,
            "frames": drained.frames,
            "seconds": seconds(drained.frames, self.sample_rate),
            "channels": drained.channels,
            "events": count("events"),
            "dropouts": count("dropouts"),
            "short_blocks": count("short_blocks"),
            "complete": drained.ended,
        }))
    }
}

/// Where the timeline of a recording to `file` goes: beside it, with
/// `.timeline.json` in place of its extension.
pub(crate) fn timeline_path(file: &Path) -> PathBuf {
    file.with_extension("timeline.json")
}

/// Drains `reader` into a WAV file at `out` until the engine says it has
/// stopped writing, or a while after `stopping`.
fn drain(
    mut reader: OutputRecording,
    out: File,
    sample_rate: u32,
    stopping: &AtomicBool,
) -> Drained {
    let mut drained = Drained::default();
    let mut out = Some(BufWriter::new(out));
    let mut writer: Option<WavWriter<BufWriter<File>>> = None;
    let (mut samples, mut callbacks) = (Vec::new(), Vec::new());
    let mut stop_seen: Option<Instant> = None;
    loop {
        samples.clear();
        callbacks.clear();
        let read = reader.read(&mut samples, &mut callbacks, &mut drained.applied);
        let mut at = 0;
        for callback in &callbacks {
            if drained.error.is_some() {
                break;
            }
            let channels = callback.channels.max(1);
            if writer.is_none()
                && let Some(out) = out.take()
            {
                match WavWriter::new(out, sample_rate, channels) {
                    Ok(made) => writer = Some(made),
                    Err(error) => drained.error = Some(error.to_string()),
                }
                drained.channels = channels;
            }
            let Some(writer) = writer.as_mut() else {
                break;
            };
            if channels != drained.channels {
                drained.error = Some("the audio device's channels changed".into());
                break;
            }
            let length = callback.frames as usize * channels as usize;
            let written = match callback.kept {
                true => {
                    let block = &samples[at..at + length];
                    at += length;
                    writer.write(block)
                }
                // Silence in place of what the ring had no room for, so the
                // frames after it stay where they played.
                false => writer.write(&vec![0.0; length]),
            };
            if let Err(error) = written {
                drained.error = Some(error.to_string());
            }
            drained.frames += u64::from(callback.frames);
        }
        drained.callbacks.extend_from_slice(&callbacks);
        drained.lost = read.lost;
        drained.lost_callbacks = read.lost_callbacks;
        if read.ended {
            drained.ended = true;
            break;
        }
        if stopping.load(Relaxed) {
            let since = *stop_seen.get_or_insert_with(Instant::now);
            if since.elapsed() > LONGEST_STOP {
                break;
            }
        }
        if drained.error.is_some() {
            break;
        }
        thread::sleep(EVERY);
    }
    let finished = match (writer, out) {
        (Some(mut writer), _) => writer.flush(),
        // Nothing played: an empty stereo file.
        (None, Some(out)) => WavWriter::new(out, sample_rate, 2).and_then(|mut w| w.flush()),
        (None, None) => Ok(()),
    };
    if let Err(error) = finished
        && drained.error.is_none()
    {
        drained.error = Some(error.to_string());
    }
    drained
}

/// The timeline of `drained`, with the events `sent` while it recorded, as
/// JSON: see the module's documentation.
fn timeline(drained: &Drained, sent: &[Sent], sample_rate: u32, project: &Project) -> Json {
    let start = drained.callbacks.first().map_or(0, |c| c.time);
    let end = start + drained.frames;
    let frame = |time: u64| time.saturating_sub(start);
    let millis = |frames: i64| frames as f64 * 1000.0 / f64::from(sample_rate);
    // Each event sent, matched with the first time the engine applied one
    // like it after: the engine applies what's sent in order.
    let mut unmatched: Vec<Option<&Sent>> = sent.iter().map(Some).collect();
    let mut applied = drained.applied.clone();
    applied.sort_by_key(|a| a.time);
    let mut events = Vec::new();
    for at in applied.iter().filter(|a| (start..end).contains(&a.time)) {
        let mut event = describe(at.applied, project);
        event["frame"] = json!(frame(at.time));
        event["seconds"] = json!(seconds(frame(at.time), sample_rate));
        if let Applied::Event(applied) = at.applied
            && let Some(slot) = (unmatched.iter_mut())
                .find(|s| s.is_some_and(|s| s.event == applied && s.time <= at.time))
            && let Some(sent) = slot.take()
        {
            let sent_frame = sent.time.saturating_sub(start);
            event["sent_frame"] = json!(sent_frame);
            event["late_ms"] = json!(round(millis(at.time as i64 - sent.time as i64)));
        }
        events.push(event);
    }
    // Dropouts and short blocks, at the callback that counted them.
    let mut dropouts = Vec::new();
    let mut short_blocks = Vec::new();
    let mut last = drained.callbacks.first().copied().unwrap_or_default();
    for callback in &drained.callbacks {
        for (now, before, out) in [
            (callback.xruns, last.xruns, &mut dropouts),
            (callback.short_blocks, last.short_blocks, &mut short_blocks),
        ] {
            if now > before {
                out.push(json!({
                    "frame": frame(callback.time),
                    "seconds": seconds(frame(callback.time), sample_rate),
                    "count": now - before,
                }));
            }
        }
        last = *callback;
    }
    let lost: Vec<Json> = (drained.callbacks.iter())
        .filter(|c| !c.kept)
        .map(|c| json!({ "frame": frame(c.time), "frames": c.frames }))
        .collect();
    let lead: Vec<u64> = drained.callbacks.iter().map(|c| c.lead).collect();
    json!({
        "sample_rate": sample_rate,
        "channels": drained.channels,
        "frames": drained.frames,
        "seconds": seconds(drained.frames, sample_rate),
        "complete": drained.ended,
        "events": events,
        "dropouts": dropouts,
        "short_blocks": short_blocks,
        "lead_frames": {
            "least": lead.iter().min(),
            "most": lead.iter().max(),
        },
        "silenced": lost,
        "events_lost": drained.lost,
        "callbacks_lost": drained.lost_callbacks,
    })
}

/// What the engine applied, as JSON, naming what an event was for from
/// `project` where it can.
fn describe(applied: Applied, project: &Project) -> Json {
    let event = match applied {
        Applied::Event(event) => event,
        Applied::Graph => return json!({ "event": "graph" }),
        Applied::Tempo => return json!({ "event": "tempo_map" }),
        Applied::Wrap => return json!({ "event": "loop_wrap" }),
    };
    match event {
        Event::Param { node, param, value } => {
            let mut json = json!({
                "event": "param",
                "node": format!("{:016x}", node.0),
                "param": param.0,
                "value": value,
            });
            if let Some(name) = param_name(project, node, param) {
                json["name"] = json!(name);
            }
            json
        }
        Event::Note { node, note } => {
            let mut json = json!({ "event": "note", "node": format!("{:016x}", node.0) });
            match note {
                NoteEvent::On { key, velocity, .. } => {
                    json["on"] = json!(key);
                    json["velocity"] = json!(velocity);
                }
                NoteEvent::Off { .. } => json["off"] = json!(true),
                _ => {}
            }
            json
        }
        Event::Transport(command) => match command {
            TransportCommand::Play => json!({ "event": "play" }),
            TransportCommand::Stop => json!({ "event": "stop" }),
            TransportCommand::Locate(at) => json!({ "event": "locate", "beats": at.to_f64() }),
            TransportCommand::Loop(range) => json!({
                "event": "loop",
                "range": range.map(|r| [r.start.to_f64(), r.end.to_f64()]),
            }),
            TransportCommand::Record(on) => json!({ "event": "record", "on": on }),
            TransportCommand::Punch { start, end } => json!({
                "event": "punch",
                "start": start.map(|b| b.to_f64()),
                "end": end.map(|b| b.to_f64()),
            }),
        },
    }
}

/// What parameter `param` of the node with `key` is, as the automation
/// lanes would name it, if it's a channel's volume or pan, or a device's.
fn param_name(project: &Project, key: NodeKey, param: ParamId) -> Option<String> {
    let chain_name = |chain: Chain| match chain {
        Chain::Master => Some("Master".to_owned()),
        Chain::Track(track) => project.track(track).map(|t| t.name.clone()),
    };
    match keys::owner(key)? {
        Owner::Strip(chain) => {
            let fader = match chain {
                Chain::Master => keys::master_gain(),
                Chain::Track(track) => keys::track_gain(track),
            };
            let name = match param {
                _ if key != fader => return None,
                Gain::GAIN => "Volume",
                Gain::PAN => "Pan",
                _ => return None,
            };
            Some(format!("{}: {name}", chain_name(chain)?))
        }
        Owner::Device(device) => {
            let (_, _, device) = project.find_device(device)?;
            let name = match (&device.kind, param) {
                (_, Switch::BYPASS) => "Bypass".to_owned(),
                (DeviceKind::Native(native), ParamId(n)) => {
                    let info = native.params().get(usize::try_from(n).ok()?)?;
                    match info.group {
                        "" => info.name.to_owned(),
                        group => format!("{group} {}", info.name),
                    }
                }
                (_, ParamId(n)) => format!("parameter {n}"),
            };
            Some(format!("{}: {name}", device.kind.name()))
        }
        _ => None,
    }
}

fn seconds(frames: u64, sample_rate: u32) -> f64 {
    round(frames as f64 / f64::from(sample_rate) * 1000.0) / 1000.0
}

/// To a hundredth.
fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}
