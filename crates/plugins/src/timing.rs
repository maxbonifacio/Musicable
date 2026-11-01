//! Finding out when the notes a plugin with a latency plays come, which
//! neither CLAP nor VST3 says: with its sound, its latency late, as a MIDI
//! effect that looks ahead holds them back, or as it plays them, on its
//! own clock ([`NoteTiming`]).
//!
//! [`measure`] plays the plugin something and sees when its first note
//! comes: a MIDI effect a note, held, an instrument the same, with the
//! transport playing from its start, and an audio effect, such as a note
//! detector, a tone. A note that comes its latency after what made it
//! comes with its sound, and one that comes at once, as it plays it.
//! Anything else says nothing, and nor does a plugin whose latency is too
//! short to tell the two apart, or that plays no note.
//!
//! Making and playing a plugin runs its code, so the app measures in a
//! process of its own ([`measure_isolated`]), Musicable's binary running
//! [`child`], as the scan does.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musicable_engine::switch::{NoteSwitch, Switch};
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Event, Graph, Inputs, Node, NodeEventKind, NodeKey, NodeMaker,
    NoteEvent, NoteId, Notes, ProcessContext, TransportCommand,
};
use musicable_model::PluginFormat;
use musicable_model::plugin::{NoteTiming, PluginKind};
use serde::{Deserialize, Serialize};

use crate::scan::{last_result, print_result, run_isolated};
use crate::{Error, Instance};

/// How long a plugin gets to load and play before it counts as hung.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// What the plugin plays at.
const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 512,
};

/// The blocks it plays in.
const BLOCK: usize = 256;

/// How long it plays for, at most: two seconds.
const LENGTH: u64 = 2 * CONFIG.sample_rate as u64;

/// The shortest latency it tells the two timings apart at, in frames:
/// shorter, they're a few milliseconds apart, too close to matter.
pub const SHORTEST: usize = 128;

/// Where an audio effect's tone starts, in frames: a tenth of a second in,
/// so a detector that looks back has silence to look at first.
const TONE_AT: u64 = CONFIG.sample_rate as u64 / 10;

/// What the measuring process prints, for [`measure_isolated`] to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Measured {
    /// `"with_sound"`, `"as_played"`, or `None` if playing it said nothing.
    timing: Option<Timing>,
}

/// [`NoteTiming`], as the measuring process prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Timing {
    WithSound,
    AsPlayed,
}

/// When the notes plugin `id` in the library at `path`, in `format`, plays
/// come, if playing it says: see the [module](self). `kind` is what it
/// does with notes, as the app has it.
pub fn measure(
    format: PluginFormat,
    path: &Path,
    id: &str,
    kind: PluginKind,
) -> Result<Option<NoteTiming>, Error> {
    let mut instance = Instance::new(format, path, id, None)?;
    let timing = measure_instance(&mut instance, kind);
    instance.deactivate()?;
    timing
}

/// When the notes `instance` plays come, as it's set, if playing it says:
/// see [`measure`].
pub(crate) fn measure_instance(
    instance: &mut Instance,
    kind: PluginKind,
) -> Result<Option<NoteTiming>, Error> {
    instance.activate(CONFIG)?;
    let latency = instance.latency();
    let makes_notes = kind != PluginKind::MidiEffect && instance.note_output();
    if latency < SHORTEST || !(kind == PluginKind::MidiEffect || makes_notes) {
        return Ok(None);
    }
    let node = instance.node_source().make();
    let caught = Arc::new(Mutex::new(Caught::default()));
    let mut g = Graph::new();
    // In the switch the engine puts it in, which plays a MIDI effect in the
    // notes pass alone.
    let plugin = match kind {
        PluginKind::MidiEffect => g.add(NodeKey(0), NoteSwitch::new(node, false)),
        _ => g.add(NodeKey(0), Switch::new(node, false)),
    };
    let sink = g.add(NodeKey(1), Catch(Arc::clone(&caught)));
    let held = || Held(Arc::clone(&caught));
    match kind {
        // A note it plays on, held, from the start.
        PluginKind::MidiEffect => {
            let held = g.add(NodeKey(2), held());
            g.send_notes(held, plugin).map_err(graph_error)?;
            g.send_notes(plugin, sink).map_err(graph_error)?;
        }
        // A note it plays, held, which it plays notes out from.
        PluginKind::Instrument => {
            let held = g.add(NodeKey(2), held());
            g.send_notes(held, plugin).map_err(graph_error)?;
            g.send_made_notes(plugin, sink).map_err(graph_error)?;
        }
        // A tone, which it hears notes in.
        PluginKind::AudioEffect => {
            if let Ok(mut caught) = caught.lock() {
                caught.cause = Some(TONE_AT);
            }
            let tone = g.add(NodeKey(2), Tone);
            g.connect(tone, plugin).map_err(graph_error)?;
            g.send_made_notes(plugin, sink).map_err(graph_error)?;
        }
    }
    g.set_output(sink).map_err(graph_error)?;
    let graph = g.prepare(&CONFIG).map_err(graph_error)?;
    let (mut engine, mut handle) = Engine::new(CONFIG);
    let refused = |what: &str| Error::new(format!("the engine wouldn't take {what}"));
    handle.swap_graph(graph).map_err(|_| refused("the graph"))?;
    // An arpeggiator steps with the transport.
    handle
        .send(Event::Transport(TransportCommand::Play))
        .map_err(|_| refused("Play"))?;
    let mut buffer = vec![0.0; 2 * BLOCK];
    let mut played = 0;
    let first = || caught.lock().ok().and_then(|caught| caught.first);
    while played < LENGTH && first().is_none() {
        engine.process(&mut AudioMut::from_planar(&mut buffer, 2));
        played += BLOCK as u64;
    }
    drop((engine, handle));
    let caught = *caught
        .lock()
        .map_err(|_| Error::new("playing it panicked"))?;
    Ok(match (caught.first, caught.cause) {
        (Some(at), Some(cause)) => timing_of(at, cause, latency),
        _ => None,
    })
}

/// What a plugin's first note coming at engine time `at`, for something
/// that played at `cause`, says of its timing, with its `latency`: with
/// its sound if it comes about its latency late, as it plays it if about
/// at once, and nothing otherwise.
fn timing_of(at: u64, cause: u64, latency: usize) -> Option<NoteTiming> {
    let late = i128::from(at) - i128::from(cause);
    let latency = latency as i128;
    let near = |frames: i128| (late - frames).abs() <= (latency / 4).max(32);
    match (near(latency), near(0)) {
        (true, false) => Some(NoteTiming::WithSound),
        (false, true) => Some(NoteTiming::AsPlayed),
        _ => None,
    }
}

fn graph_error(e: musicable_engine::GraphError) -> Error {
    Error::new(format!("couldn't build the graph to play it in: {e}"))
}

/// The measuring process's work: measures plugin `id` in the library at
/// `path`, in `format`, of `kind`, and prints what it found, for
/// [`measure_isolated`] to read. A plugin that fails to load or play
/// prints nothing to read.
pub fn child(format: PluginFormat, path: &Path, id: &str, kind: PluginKind) {
    if let Ok(timing) = measure(format, path, id, kind) {
        let timing = timing.map(|timing| match timing {
            NoteTiming::WithSound => Timing::WithSound,
            NoteTiming::AsPlayed => Timing::AsPlayed,
        });
        print_result(&Measured { timing });
    }
}

/// Measures a plugin in a process of its own: `command` runs the
/// measuring process, which calls [`child`] for it. Gives up on it after
/// `timeout`. `Ok(None)` if playing it said nothing, and `Err` if it
/// didn't load, crashed or hung.
pub fn measure_isolated(command: Command, timeout: Duration) -> Result<Option<NoteTiming>, String> {
    let (out, status) = run_isolated(command, timeout).map_err(|e| format!("didn't start: {e}"))?;
    match (last_result::<Measured>(&out), status) {
        (Some(measured), _) => Ok(measured.timing.map(|timing| match timing {
            Timing::WithSound => NoteTiming::WithSound,
            Timing::AsPlayed => NoteTiming::AsPlayed,
        })),
        (None, Some(status)) => Err(format!("it didn't load or play ({status})")),
        (None, None) => Err(format!(
            "it was still playing after {} seconds",
            timeout.as_secs()
        )),
    }
}

/// When what the plugin played came, and when its first note did, in
/// engine time.
#[derive(Debug, Clone, Copy, Default)]
struct Caught {
    cause: Option<u64>,
    first: Option<u64>,
}

/// Plays one note, held, from the first frame it plays, and says when.
struct Held(Arc<Mutex<Caught>>);

impl Node for Held {
    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let Ok(mut caught) = self.0.lock() else {
            return;
        };
        if caught.cause.is_none() {
            caught.cause = Some(ctx.time);
            let on = NoteEvent::On {
                note: NoteId(1),
                key: 60,
                velocity: 1.0,
                channel: None,
            };
            notes.send(0, on);
        }
    }
}

/// A tone, from [`TONE_AT`] on: A below middle C, at half scale.
struct Tone;

impl Node for Tone {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        let rate = f64::from(ctx.sample_rate);
        for c in 0..output.channels() {
            for (i, sample) in output.channel_mut(c).iter_mut().enumerate() {
                let t = ctx.time + i as u64;
                *sample = match t >= TONE_AT {
                    true => {
                        let phase = (t - TONE_AT) as f64 * 220.0 / rate;
                        (0.5 * (phase * std::f64::consts::TAU).sin()) as f32
                    }
                    false => 0.0,
                };
            }
        }
    }
}

/// Plays silence, and keeps when the first note that comes to it starts.
struct Catch(Arc<Mutex<Caught>>);

impl Node for Catch {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        let started = ctx.events.iter().find_map(|event| match event.kind {
            NodeEventKind::Note(NoteEvent::On { .. }) => Some(ctx.time + event.offset as u64),
            _ => None,
        });
        if let (Some(at), Ok(mut caught)) = (started, self.0.lock())
            && caught.first.is_none()
        {
            caught.first = Some(at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_about_a_latency_late_comes_with_its_sound() {
        assert_eq!(timing_of(1000, 0, 1000), Some(NoteTiming::WithSound));
        assert_eq!(timing_of(5800, 4800, 960), Some(NoteTiming::WithSound));
        assert_eq!(timing_of(4810, 4800, 960), Some(NoteTiming::AsPlayed));
        assert_eq!(timing_of(0, 0, 200), Some(NoteTiming::AsPlayed));
        // Halfway, or long after, says nothing.
        assert_eq!(timing_of(500, 0, 1000), None);
        assert_eq!(timing_of(3000, 0, 1000), None);
    }
}
