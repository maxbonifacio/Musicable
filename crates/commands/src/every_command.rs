//! Undo and redo for every command.
//!
//! Each command the app registers has an example here, saying what it does
//! to the project. One that changes it must do so as one step, which undo
//! takes back exactly and redo makes again. One that doesn't, such as
//! selecting or playing, must leave the project and its undo history alone.
//! [`every_command_has_an_example`] fails for a command without one, so a
//! new command can't go untested.
//!
//! The examples run on [`setup`]'s project one at a time, then in random
//! orders, with undo and redo in between, each acting on what the ones
//! before left, and on what they made. Each project a step makes must save,
//! and load back as it was. Undo and redo must lead back to those projects
//! exactly, but for the counters that hand out ids, which never go back, so
//! that no id is handed out twice.
//!
//! Each example also runs as an agent runs it, with its arguments and its
//! result as JSON, and must do just what it does from the UI, unless it's
//! left to the user. And in random orders, the examples run as one batch,
//! which must be one step, or nothing at all once it's cancelled.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::Arc;

use musicable_analysis::Analysis;
use musicable_model::automation::Lane;
use musicable_model::clip::{
    AudioClip, Clip, ClipContent, ClipId, Controller, MidiClip, NoteId, Offset, Point,
};
use musicable_model::mixer::Bus;
use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::param::{ClipParam, ParamAddress};
use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings, PluginState};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::warp::{
    Onset, Onsets, Origin, Preserve, TransientLoop, Warp, WarpMarker, WarpMode,
};
use musicable_model::{
    Chain, ClipType, DeviceId, DeviceKind, Monitor, PluginFormat, Project, Take, Track, TrackId,
    TrackKind,
};
use proptest::prelude::*;

use crate::editor::AudioFile;
use crate::testing::Setup;
use crate::{
    Args, CommandError, CommandId, Committed, Document, Value, arrange, automation, browser,
    controllers, device, edit, file, history, json, locator, mixer, notes, options, rewarp, take,
    track, transient, transport, view, warp, warp_mode, workspace,
};

/// An audio file the editor has read, which clips can be inserted from.
const LOOP: &str = "loop.wav";
/// What the takes recorded.
const RECORDING: &str = "Recordings/Audio 1.wav";
/// What the warped clip plays.
const GROOVE: &str = "groove.wav";
/// A sample in the browser's library.
const KICK: &str = "/samples/kick.wav";
/// A preset in the library, which holds an EQ.
const PRESET: &str = "/presets/Bright.musicable-preset";

/// What a command does to the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Does {
    /// It changes it, as one step in the undo history.
    Edits,
    /// It leaves it, and the undo history, alone: it changes what the
    /// editor shows or has selected, drives the transport, or asks the app
    /// or the library for something.
    Leaves,
    /// It steps through the undo history: undo, redo, or going to a step.
    Steps,
}

/// What the examples run on: the editor, its project, and which of the
/// things an example could act on it picks. Run one at a time, examples
/// pick the first; in random orders, any, so that they act on what the
/// ones before made too.
struct Rig {
    setup: Setup,
    pick: usize,
}

impl Deref for Rig {
    type Target = Setup;

    fn deref(&self) -> &Setup {
        &self.setup
    }
}

impl DerefMut for Rig {
    fn deref_mut(&mut self) -> &mut Setup {
        &mut self.setup
    }
}

/// A way to run a command.
struct Example {
    command: CommandId,
    does: Does,
    /// Gets the editor ready for it, such as by selecting what it acts on,
    /// and gives the arguments to run it with, or `None` if the project has
    /// nothing for it to act on.
    prepare: fn(&mut Rig) -> Option<Args>,
}

fn edits(command: CommandId, prepare: fn(&mut Rig) -> Option<Args>) -> Example {
    Example {
        command,
        does: Does::Edits,
        prepare,
    }
}

fn leaves(command: CommandId, prepare: fn(&mut Rig) -> Option<Args>) -> Example {
    Example {
        command,
        does: Does::Leaves,
        prepare,
    }
}

/// Undo, redo or going to a step, which the tests run between the other
/// examples.
fn steps(command: CommandId, prepare: fn(&mut Rig) -> Option<Args>) -> Example {
    Example {
        command,
        does: Does::Steps,
        prepare,
    }
}

/// No arguments, and nothing to get ready.
fn bare(_: &mut Rig) -> Option<Args> {
    Some(Args::new())
}

/// Every command, by what it does to the project, with arguments that act
/// on what's there. Some have more than one example, for the ways they
/// work: on clips or on notes, with arguments or without them.
fn examples() -> Vec<Example> {
    let switches = workspace::SWITCH.map(|id| leaves(id, bare));
    let mut examples = vec![
        edits(automation::ADD_POINT, |s| {
            let track = find_track(s, |_| true)?.id;
            let param = ParamAddress::Volume(Chain::Track(track));
            Some(
                Args::new()
                    .with("param", param)
                    .with("at", Beats::new(2))
                    .with("value", 0.25),
            )
        }),
        edits(automation::CLEAR, |s| {
            let lane = pick(s, project(s).automation.iter())?;
            Some(Args::new().with("param", lane.param))
        }),
        edits(automation::DELETE_POINTS, |s| {
            let lanes = project(s).automation.iter();
            let lane = pick(s, lanes.filter(|l| !l.points.is_empty()))?;
            let at = pick(s, lane.points.iter())?.at;
            Some(
                Args::new()
                    .with("param", lane.param)
                    .with("start", at)
                    .with("end", at),
            )
        }),
        edits(automation::RE_ENABLE, |s| {
            let lanes = project(s).automation.iter();
            let lane = pick(s, lanes.filter(|l| l.overridden))?;
            Some(Args::new().with("param", lane.param))
        }),
        // Without arguments: every lane someone overrode.
        edits(automation::RE_ENABLE, |s| {
            let overridden = project(s).automation.iter().any(|l| l.overridden);
            overridden.then(Args::new)
        }),
        // A clip's curve, in its own time.
        edits(automation::CLEAR, |s| {
            let clip = find_clip(s, |c| curve(c).is_some())?.id;
            let param = ParamAddress::Clip(clip, ClipParam::Gain);
            Some(Args::new().with("param", param))
        }),
        edits(automation::SET_POINTS, |s| {
            let clip = find_clip(s, is_audio)?.id;
            let param = ParamAddress::Clip(clip, ClipParam::Gain);
            let at = vec![Beats::new(-1), Beats::new(2)];
            let values = match curve(project(s).find_clip(clip)?.1) {
                Some(_) => vec![2.0, 0.25],
                None => vec![0.25, 2.0],
            };
            Some(
                Args::new()
                    .with("param", param)
                    .with("at", at)
                    .with("values", values),
            )
        }),
        edits(automation::SET_POINTS, |s| {
            let lane = pick(s, project(s).automation.iter());
            let param = lane.map_or(ParamAddress::Volume(Chain::Master), |l| l.param);
            Some(
                Args::new()
                    .with("param", param)
                    .with("at", vec![Beats::ZERO, Beats::new(4)])
                    .with("values", vec![0.5, 1.0]),
            )
        }),
        leaves(automation::HIDE_LANE, |s| {
            let track = find_track(s, |_| true)?.id;
            let param = ParamAddress::Volume(Chain::Track(track));
            Some(Args::new().with("param", param))
        }),
        leaves(automation::SHOW, |_| Some(Args::new().with("master", true))),
        leaves(automation::SHOW, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("track", track))
        }),
        leaves(automation::SHOW_LANE, |_| {
            let param = ParamAddress::Volume(Chain::Master);
            Some(Args::new().with("param", param))
        }),
        leaves(browser::ADD_FOLDER, |_| {
            Some(Args::new().with("folder", "/samples"))
        }),
        // Without a folder: the app asks for one.
        leaves(browser::ADD_FOLDER, bare),
        leaves(browser::FIND_SIMILAR, |_| {
            Some(Args::new().with("file", KICK))
        }),
        leaves(browser::FOCUS_SEARCH, bare),
        leaves(browser::PREVIEW, |_| Some(Args::new().with("file", KICK))),
        leaves(browser::REMOVE_FOLDER, |_| {
            Some(Args::new().with("folder", "/samples"))
        }),
        leaves(browser::RESCAN, bare),
        leaves(browser::SEARCH, |_| Some(Args::new().with("text", "kick"))),
        leaves(browser::STOP_PREVIEW, bare),
        leaves(browser::TAG, |_| {
            Some(Args::new().with("file", KICK).with("tag", "dusty"))
        }),
        edits(arrange::DELETE, |s| {
            let clip = find_clip(s, |_| true)?.id;
            Some(Args::new().with("clips", vec![clip]))
        }),
        edits(arrange::INSERT_AUDIO, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Audio)?.id;
            Some(
                Args::new()
                    .with("files", vec![LOOP, LOOP])
                    .with("track", track)
                    .with("at", Beats::new(16)),
            )
        }),
        // Without a track: the selected track, if it's an audio track, or
        // else a new one.
        edits(arrange::INSERT_AUDIO, |_| {
            Some(Args::new().with("files", vec![LOOP]))
        }),
        edits(arrange::INSERT_MIDI_CLIP, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Midi)?.id;
            Some(
                Args::new()
                    .with("track", track)
                    .with("start", Beats::new(16))
                    .with("end", Beats::new(20)),
            )
        }),
        // Without arguments: on the MIDI tracks of the time selection.
        edits(arrange::INSERT_MIDI_CLIP, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Midi)?.id;
            s.select_time(16, 20, &[track]);
            Some(Args::new())
        }),
        edits(arrange::MOVE, |s| {
            let clip = find_clip(s, |_| true)?.id;
            Some(
                Args::new()
                    .with("clips", vec![clip])
                    .with("by", Beats::new(1)),
            )
        }),
        // A copy, on the next track down.
        edits(arrange::MOVE, |s| {
            let tracks = project(s).tracks.windows(2);
            let clips = tracks.flat_map(|pair| {
                let below = &pair[1];
                pair[0].clips.iter().filter(|c| below.holds(&c.content))
            });
            let clip = pick(s, clips)?;
            Some(
                Args::new()
                    .with("clips", vec![clip.id])
                    .with("tracks", 1_i64)
                    .with("copy", true),
            )
        }),
        edits(arrange::RENAME, |s| {
            let clip = find_clip(s, |_| true)?;
            Some(
                Args::new()
                    .with("clip", clip.id)
                    .with("name", other_name(&clip.name)),
            )
        }),
        edits(arrange::SET_FADES, |s| {
            let clip = find_clip(s, |c| c.length >= Beats::new(1) && !is_midi(c))?;
            let ClipContent::Audio(audio) = &clip.content else {
                return None;
            };
            let half = Beats::from_ratio(1, 2);
            let fade = match audio.fade_in == half {
                true => Beats::from_ratio(1, 4),
                false => half,
            };
            Some(
                Args::new()
                    .with("clip", clip.id)
                    .with("fade_in", fade)
                    .with("fade_out", fade),
            )
        }),
        edits(arrange::SPLIT, |s| {
            let clip = find_clip(s, |c| c.length.ticks() >= 2)?;
            Some(
                Args::new()
                    .with("clips", vec![clip.id])
                    .with("at", middle(clip)),
            )
        }),
        edits(arrange::TRIM, |s| {
            let clip = find_clip(s, |c| c.length.ticks() >= 2)?;
            Some(Args::new().with("clip", clip.id).with("end", middle(clip)))
        }),
        // Its start, which keeps its content where it plays.
        edits(arrange::TRIM, |s| {
            let clip = find_clip(s, |c| c.length.ticks() >= 2)?;
            Some(
                Args::new()
                    .with("clip", clip.id)
                    .with("start", middle(clip)),
            )
        }),
        edits(controllers::CLEAR, |s| {
            let (clip, controller, _) = find_curve(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("controller", i64::from(controller.number())),
            )
        }),
        // Without arguments: the curve the piano roll's lane shows.
        edits(controllers::CLEAR, |s| {
            let (clip, controller, _) = find_curve(s)?;
            s.run_with(&notes::OPEN, Args::new().with("clip", clip))
                .ok()?;
            let number = i64::from(controller.number());
            let args = Args::new().with("controller", number);
            s.run_with(&controllers::SHOW, args).ok()?;
            Some(Args::new())
        }),
        edits(controllers::DRAW, |s| {
            let clip = find_clip(s, is_midi)?.id;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("controller", 1_i64)
                    .with("start", Beats::ZERO)
                    .with("end", Beats::new(2))
                    .with("at", vec![Beats::ZERO, Beats::new(1)])
                    .with("values", vec![0.25, 0.75]),
            )
        }),
        edits(controllers::ERASE, |s| {
            let (clip, controller, at) = find_curve(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("controller", i64::from(controller.number()))
                    .with("start", at)
                    .with("end", at + Beats::new(1)),
            )
        }),
        leaves(controllers::SHOW, |_| {
            Some(Args::new().with("controller", 1_i64))
        }),
        leaves(controllers::SHOW_VELOCITIES, bare),
        edits(device::ADD, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("kind", "eq").with("track", track))
        }),
        // An instrument, on a new MIDI track.
        edits(device::ADD, |_| {
            Some(Args::new().with("kind", "synth").with("new_track", true))
        }),
        edits(device::ADD, |_| {
            Some(
                Args::new()
                    .with("kind", "compressor")
                    .with("master", true)
                    .with("index", 0_i64),
            )
        }),
        edits(device::ADD_PLUGIN, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Midi)?.id;
            Some(
                Args::new()
                    .with("format", "clap")
                    .with("plugin", "org.example.synth")
                    .with("name", "Synth")
                    .with("instrument", true)
                    .with("track", track),
            )
        }),
        edits(device::ADD_PLUGIN, |_| {
            Some(
                Args::new()
                    .with("format", "vst3")
                    .with("plugin", "ABCDEF0123456789ABCDEF0123456789")
                    .with("name", "Echo")
                    .with("master", true),
            )
        }),
        edits(device::ADD_PRESET, |_| {
            Some(Args::new().with("preset", PRESET).with("master", true))
        }),
        edits(device::ADD_PRESET, |_| {
            Some(Args::new().with("preset", PRESET).with("new_track", true))
        }),
        edits(device::DELETE, |s| {
            let device = find_device(s, |_| true)?;
            Some(Args::new().with("device", device))
        }),
        // One of Musicable's own devices.
        edits(device::SET_PARAM, |s| {
            let device = find_device(s, |d| d.native().is_some())?;
            let (_, _, found) = project(s).find_device(device)?;
            let native = found.kind.native()?;
            let info = native.params().first()?;
            let value = match native.value(0) == Some(info.min) {
                true => info.max,
                false => info.min,
            };
            Some(
                Args::new()
                    .with("device", device)
                    .with("param", 0_i64)
                    .with("value", value),
            )
        }),
        // A plugin.
        edits(device::SET_PARAM, |s| {
            let device = find_device(s, |d| d.plugin().is_some())?;
            let (_, _, found) = project(s).find_device(device)?;
            let params = &found.kind.plugin()?.settings.params;
            let value = params.iter().find(|p| p.id == 1).map(|p| p.value);
            Some(
                Args::new()
                    .with("device", device)
                    .with("param", 1_i64)
                    .with("value", other_level(value)),
            )
        }),
        edits(device::SET_SAMPLE, |s| {
            let device = find_device(s, is_sampler)?;
            let (_, _, found) = project(s).find_device(device)?;
            let args = Args::new().with("device", device);
            // Without a file, the sampler has no sample.
            Some(match &found.kind.native()?.sample {
                Some(_) => args,
                None => args.with("file", KICK),
            })
        }),
        leaves(device::HIDE_WINDOW, |s| {
            let device = find_device(s, |d| d.plugin().is_some())?;
            Some(Args::new().with("device", device))
        }),
        leaves(device::LOAD_SAMPLE, |s| {
            let device = find_device(s, is_sampler)?;
            Some(Args::new().with("device", device))
        }),
        leaves(device::SAVE_PRESET, |s| {
            let device = find_device(s, |_| true)?;
            Some(Args::new().with("device", device))
        }),
        leaves(device::SHOW_WINDOW, |s| {
            let device = find_device(s, |d| d.plugin().is_some())?;
            Some(Args::new().with("device", device))
        }),
        leaves(edit::COPY, clip_selected),
        leaves(edit::COPY, note_selected),
        edits(edit::CUT, clip_selected),
        edits(edit::CUT, time_selected),
        edits(edit::CUT, note_selected),
        edits(edit::DELETE, clip_selected),
        edits(edit::DELETE, time_selected),
        // The selected tracks.
        edits(edit::DELETE, |s| {
            let track = find_track(s, |_| true)?.id;
            s.select_tracks(&[track]);
            Some(Args::new())
        }),
        edits(edit::DELETE, note_selected),
        // The markers selected in the Sample Editor.
        edits(edit::DELETE, |s| {
            find_marker(s, |w| w.markers.len() > 1)?;
            marker_selected(s)
        }),
        edits(edit::DUPLICATE, clip_selected),
        edits(edit::DUPLICATE, time_selected),
        edits(edit::DUPLICATE, note_selected),
        edits(edit::PASTE, |s| {
            clip_selected(s)?;
            s.run(&edit::COPY);
            start_marker(s, Beats::new(32));
            Some(Args::new())
        }),
        edits(edit::PASTE, |s| {
            let (clip, _) = find_note(s)?;
            note_selected(s)?;
            s.run(&edit::COPY);
            let (_, c) = project(s).find_clip(clip)?;
            let at = c.start + Beats::new(2);
            start_marker(s, at);
            Some(Args::new())
        }),
        edits(edit::QUANTIZE, |s| {
            let (clip, notes) = find_notes(s)?;
            Some(Args::new().with("clip", clip).with("notes", notes))
        }),
        // Without arguments, while the arrangement has the focus: the notes
        // of the selected MIDI clips.
        edits(edit::QUANTIZE, |s| {
            let (clip, _) = find_notes(s)?;
            s.select_clips(&[clip]);
            Some(Args::new())
        }),
        // A warped clip's transients, which gain warp markers.
        edits(edit::QUANTIZE, |s| {
            let (clip, frame) = find_transient(s)?;
            let frames = vec![frame as i64];
            Some(Args::new().with("clip", clip).with("frames", frames))
        }),
        // Without arguments: those in the Sample Editor's time selection.
        edits(edit::QUANTIZE, transient_selected),
        // And while the arrangement has the focus, those of the selected
        // warped clips.
        edits(edit::QUANTIZE, |s| {
            let (clip, _) = find_transient(s)?;
            s.select_clips(&[clip]);
            Some(Args::new())
        }),
        steps(edit::REDO, bare),
        leaves(edit::RENAME, |s| {
            let clip = find_clip(s, |_| true)?.id;
            Some(Args::new().with("clip", clip))
        }),
        // Without arguments: the selected clip.
        leaves(edit::RENAME, clip_selected),
        leaves(edit::SELECT_ALL, bare),
        // In the piano roll.
        leaves(edit::SELECT_ALL, note_selected),
        // In the Sample Editor.
        leaves(edit::SELECT_ALL, marker_selected),
        leaves(edit::SELECT_CLIPS, |s| {
            let clip = find_clip(s, |_| true)?.id;
            Some(Args::new().with("clips", vec![clip]))
        }),
        leaves(edit::SELECT_TIME, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(
                Args::new()
                    .with("start", Beats::ZERO)
                    .with("end", Beats::new(4))
                    .with("tracks", vec![track]),
            )
        }),
        // On the master alone.
        leaves(edit::SELECT_TIME, |_| {
            Some(
                Args::new()
                    .with("start", Beats::new(2))
                    .with("end", Beats::new(6))
                    .with("tracks", Vec::<TrackId>::new())
                    .with("master", true),
            )
        }),
        leaves(edit::SELECT_TRACKS, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("tracks", vec![track]))
        }),
        leaves(edit::SELECT_MASTER, bare),
        edits(edit::SPLIT, |s| {
            let clip = find_clip(s, |c| c.length.ticks() >= 2)?;
            let (id, at) = (clip.id, middle(clip));
            s.select_clips(&[id]);
            start_marker(s, at);
            Some(Args::new())
        }),
        edits(edit::SPLIT, time_selected),
        steps(edit::UNDO, bare),
        leaves(file::EXPORT_AUDIO, bare),
        // Given files, as scripts and agents give them, the file commands
        // ask for no dialog.
        leaves(file::EXPORT_AUDIO, |_| {
            Some(Args::new().with("file", "/exports/Song.wav"))
        }),
        leaves(file::IMPORT_AUDIO, bare),
        leaves(file::IMPORT_AUDIO, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Audio)?.id;
            Some(
                Args::new()
                    .with("files", vec![KICK])
                    .with("track", track)
                    .with("at", Beats::new(2)),
            )
        }),
        leaves(file::NEW, bare),
        leaves(file::NEW_DEMO, bare),
        leaves(file::OPEN, bare),
        leaves(file::OPEN, |_| {
            Some(Args::new().with("file", "/songs/Other.musicable"))
        }),
        leaves(file::QUIT, bare),
        leaves(file::SAVE, bare),
        // Before every step, which the fixture's history hasn't.
        steps(history::GO_TO, |_| Some(Args::new().with("step", 0_i64))),
        leaves(file::SAVE_AS, bare),
        leaves(file::SAVE_AS, |_| {
            Some(Args::new().with("file", "/songs/Copy.musicable"))
        }),
        edits(locator::ADD, |s| {
            let locators = &project(s).locators;
            let free = |at: &Beats| locators.iter().all(|l| l.at != *at);
            let at = (1..).map(Beats::new).find(free)?;
            Some(Args::new().with("at", at).with("name", "Bridge"))
        }),
        // Without arguments: at the start marker, named by a number.
        edits(locator::ADD, |s| {
            let locators = &project(s).locators;
            let free = |at: &Beats| locators.iter().all(|l| l.at != *at);
            let at = (1..).map(Beats::new).find(free)?;
            start_marker(s, at);
            Some(Args::new())
        }),
        edits(locator::DELETE, |s| {
            let locator = pick(s, project(s).locators.iter())?;
            Some(Args::new().with("locator", locator.id))
        }),
        edits(locator::MOVE, |s| {
            let locator = pick(s, project(s).locators.iter())?;
            Some(
                Args::new()
                    .with("locator", locator.id)
                    .with("at", locator.at + Beats::new(1)),
            )
        }),
        leaves(locator::NEXT, bare),
        leaves(locator::PREVIOUS, |s| {
            start_marker(s, Beats::new(64));
            Some(Args::new())
        }),
        edits(locator::RENAME, |s| {
            let locator = pick(s, project(s).locators.iter())?;
            Some(
                Args::new()
                    .with("locator", locator.id)
                    .with("name", other_name(&locator.name)),
            )
        }),
        edits(mixer::REMOVE_SEND, |s| {
            let (track, to) = find_send(s)?;
            Some(Args::new().with("track", track).with("to", to))
        }),
        edits(mixer::SET_ARM, |s| {
            let track = find_track(s, |t| !t.kind.is_bus())?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("armed", !track.armed),
            )
        }),
        edits(mixer::SET_CLIP_TYPE, |s| {
            let track = find_track(s, |_| true)?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("clip_type", next_clip_type(track.clip_type)),
            )
        }),
        edits(mixer::SET_INPUT, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Audio)?;
            let args = Args::new().with("track", track.id);
            Some(match track.input {
                Some(_) => args.with("none", true),
                None => args
                    .with("device", "in-1")
                    .with("name", "Input 1")
                    .with("channel", 2_i64)
                    .with("stereo", false),
            })
        }),
        edits(mixer::SET_INPUT, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Midi)?;
            let args = Args::new().with("track", track.id);
            Some(match track.input {
                Some(_) => args.with("none", true),
                None => args
                    .with("device", "keys")
                    .with("name", "Keys")
                    .with("channel", 9_i64),
            })
        }),
        edits(mixer::SET_MASTER_CLIP_TYPE, |s| {
            let clip_type = next_clip_type(project(s).master.clip_type);
            Some(Args::new().with("clip_type", clip_type))
        }),
        edits(mixer::SET_MASTER_PAN, |s| {
            let pan = other_level(Some(f64::from(project(s).master.pan)));
            Some(Args::new().with("pan", pan))
        }),
        edits(mixer::SET_MASTER_VOLUME, |s| {
            let gain = other_level(Some(f64::from(project(s).master.gain)));
            Some(Args::new().with("gain", gain))
        }),
        edits(mixer::SET_MONITOR, |s| {
            let track = find_track(s, |t| !t.kind.is_bus())?;
            let monitor = match track.monitor {
                Monitor::In => "Auto",
                Monitor::Auto => "Off",
                Monitor::Off => "In",
            };
            Some(Args::new().with("track", track.id).with("monitor", monitor))
        }),
        edits(mixer::SET_MUTE, |s| {
            let track = find_track(s, |_| true)?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("muted", !track.muted),
            )
        }),
        edits(mixer::SET_OUTPUT, |s| {
            let group = find_track(s, |t| t.kind == TrackKind::Group)?.id;
            let track = find_track(s, |t| !t.kind.is_bus())?;
            let args = Args::new().with("track", track.id);
            // Without a group, the master.
            Some(match track.output {
                Bus::Master => args.with("group", group),
                Bus::Group(_) => args,
            })
        }),
        edits(mixer::SET_PAN, |s| {
            let track = find_track(s, |_| true)?;
            let pan = other_level(Some(f64::from(track.pan)));
            Some(Args::new().with("track", track.id).with("pan", pan))
        }),
        edits(mixer::SET_SEND_LEVEL, |s| {
            let (track, to) = find_send(s)?;
            let t = project(s).track(track)?;
            let level = t.sends.iter().find(|send| send.to == to)?.level;
            Some(
                Args::new()
                    .with("track", track)
                    .with("to", to)
                    .with("level", other_level(Some(f64::from(level)))),
            )
        }),
        // A track without a send to the return track gets one.
        edits(mixer::SET_SEND_LEVEL, |s| {
            let to = find_track(s, |t| t.kind == TrackKind::Return)?.id;
            let track = find_track(s, |t| {
                t.kind != TrackKind::Return && t.sends.iter().all(|send| send.to != to)
            })?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("to", to)
                    .with("level", 0.5),
            )
        }),
        edits(mixer::SET_SEND_PRE_FADER, |s| {
            let (track, to) = find_send(s)?;
            let t = project(s).track(track)?;
            let send = t.sends.iter().find(|send| send.to == to)?;
            Some(
                Args::new()
                    .with("track", track)
                    .with("to", to)
                    .with("pre_fader", !send.pre_fader),
            )
        }),
        edits(mixer::SET_SOLO, |s| {
            let track = find_track(s, |_| true)?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("solo", !track.solo),
            )
        }),
        // Unsoloing every other track.
        edits(mixer::SET_SOLO, |s| {
            let track = find_track(s, |_| true)?;
            let soloed = project(s).tracks.iter().any(|t| t.id != track.id && t.solo);
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("solo", soloed || !track.solo)
                    .with("exclusive", true),
            )
        }),
        edits(mixer::SET_SOLO_SAFE, |s| {
            let track = find_track(s, |_| true)?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("solo_safe", !track.solo_safe),
            )
        }),
        edits(mixer::SET_VOLUME, |s| {
            let track = find_track(s, |_| true)?;
            let gain = other_level(Some(f64::from(track.gain)));
            Some(Args::new().with("track", track.id).with("gain", gain))
        }),
        edits(notes::ADD, |s| {
            let clip = find_clip(s, is_midi)?.id;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("start", Beats::new(3))
                    .with("key", 60_i64),
            )
        }),
        // Without a clip: the one the piano roll shows.
        edits(notes::ADD, |s| {
            let clip = find_clip(s, is_midi)?.id;
            s.run_with(&notes::OPEN, Args::new().with("clip", clip))
                .ok()?;
            Some(
                Args::new()
                    .with("start", Beats::new(3))
                    .with("key", 62_i64)
                    .with("length", Beats::from_ratio(1, 2))
                    .with("velocity", 0.5),
            )
        }),
        edits(notes::DELETE, |s| {
            let (clip, note) = find_note(s)?;
            Some(Args::new().with("clip", clip).with("notes", vec![note]))
        }),
        edits(notes::DRAW, |s| {
            let clip = find_clip(s, is_midi)?.id;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("starts", vec![Beats::ZERO, Beats::from_ratio(1, 2)])
                    .with("keys", vec![72_i64, 74]),
            )
        }),
        edits(notes::LENGTHEN, note_selected),
        edits(notes::MOVE, |s| {
            let (clip, note) = find_note(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("notes", vec![note])
                    .with("by", Beats::from_ratio(1, 2))
                    .with("keys", 2_i64),
            )
        }),
        edits(notes::MOVE, |s| {
            let (clip, notes) = find_notes(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("notes", notes)
                    .with("by", Beats::new(4))
                    .with("copy", true),
            )
        }),
        edits(notes::NUDGE_LEFT, note_selected),
        edits(notes::NUDGE_RIGHT, note_selected),
        edits(notes::OCTAVE_DOWN, note_selected),
        edits(notes::OCTAVE_UP, note_selected),
        leaves(notes::OPEN, |s| {
            let clip = find_clip(s, is_midi)?.id;
            Some(Args::new().with("clip", clip))
        }),
        leaves(notes::SELECT, |s| {
            let (clip, notes) = find_notes(s)?;
            Some(Args::new().with("clip", clip).with("notes", notes))
        }),
        edits(notes::SET_VELOCITY, |s| {
            let (clip, note) = find_note(s)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Midi(midi) = &c.content else {
                return None;
            };
            let velocity = midi.note(note).map(|n| f64::from(n.velocity));
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("notes", vec![note])
                    .with("velocity", other_level(velocity)),
            )
        }),
        edits(notes::SHORTEN, note_selected),
        leaves(notes::TOGGLE_DRAW, bare),
        edits(notes::TRANSPOSE_DOWN, note_selected),
        edits(notes::TRANSPOSE_UP, note_selected),
        edits(notes::TRIM, |s| {
            let (clip, note) = find_note(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("notes", vec![note])
                    .with("start_by", Beats::from_ratio(1, 4))
                    .with("end_by", Beats::from_ratio(1, 2)),
            )
        }),
        edits(notes::VELOCITY_DOWN, note_selected),
        edits(notes::VELOCITY_UP, note_selected),
        leaves(options::AGENTS, bare),
        leaves(options::APPEARANCE, bare),
        leaves(options::AUDIO_SETTINGS, bare),
        leaves(options::KEYBOARD_SHORTCUTS, bare),
        leaves(options::NEXT_PROFILE, bare),
        leaves(options::PROFILES, bare),
        leaves(options::RESCAN_PLUGINS, bare),
        leaves(options::SETTINGS, bare),
        edits(take::COMP, |s| {
            let take = find_take(s, |_| true)?;
            let clip = pick(s, take.clips.iter())?;
            Some(
                Args::new()
                    .with("take", take.id)
                    .with("start", clip.start)
                    .with("end", clip.end()),
            )
        }),
        // A MIDI take's notes.
        edits(take::COMP, |s| {
            let take = find_take(s, |c| matches!(c.content, ClipContent::Midi(_)))?;
            let clip = pick(s, take.clips.iter())?;
            Some(
                Args::new()
                    .with("take", take.id)
                    .with("start", clip.start)
                    .with("end", clip.end()),
            )
        }),
        edits(take::DELETE, |s| {
            let take = find_take(s, |_| true)?;
            Some(Args::new().with("take", take.id))
        }),
        edits(take::RENAME, |s| {
            let take = find_take(s, |_| true)?;
            Some(
                Args::new()
                    .with("take", take.id)
                    .with("name", other_name(&take.name)),
            )
        }),
        edits(track::DELETE, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("tracks", vec![track]))
        }),
        // Sends to it go.
        edits(track::DELETE, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Return)?.id;
            Some(Args::new().with("tracks", vec![track]))
        }),
        // The tracks feeding it feed the master.
        edits(track::DELETE, |s| {
            let track = find_track(s, |t| t.kind == TrackKind::Group)?.id;
            Some(Args::new().with("tracks", vec![track]))
        }),
        // Without arguments: the selected tracks.
        edits(track::DELETE, |s| {
            let tracks: Vec<_> = project(s)
                .tracks
                .iter()
                .rev()
                .take(2)
                .map(|t| t.id)
                .collect();
            (!tracks.is_empty()).then_some(())?;
            s.select_tracks(&tracks);
            Some(Args::new())
        }),
        edits(track::NEW_AUDIO_TRACK, bare),
        edits(track::NEW_GROUP_TRACK, bare),
        edits(track::NEW_MIDI_TRACK, bare),
        edits(track::NEW_RETURN_TRACK, bare),
        edits(track::RENAME, |s| {
            let track = find_track(s, |_| true)?;
            Some(
                Args::new()
                    .with("track", track.id)
                    .with("name", other_name(&track.name)),
            )
        }),
        leaves(transport::CAPTURE, bare),
        leaves(transport::GO_TO_START, bare),
        leaves(transport::LOCATE, |_| {
            Some(Args::new().with("at", Beats::new(8)))
        }),
        leaves(transport::PLAY, bare),
        leaves(transport::PLAY_STOP, bare),
        leaves(transport::RECORD, bare),
        edits(transport::SET_LOOP, |s| {
            let (start, end) = other_loop(project(s));
            Some(Args::new().with("start", start).with("end", end))
        }),
        // Turning looping on or off as well.
        edits(transport::SET_LOOP, |s| {
            let (start, end) = other_loop(project(s));
            let looping = !project(s).looping;
            Some(
                Args::new()
                    .with("start", start)
                    .with("end", end)
                    .with("looping", looping),
            )
        }),
        leaves(transport::SET_START_MARKER, |_| {
            Some(Args::new().with("at", Beats::new(4)))
        }),
        edits(transport::SET_TEMPO, |s| {
            let bpm = match project(s).tempo.bpm_at(Beats::ZERO) == 100.0 {
                true => 90.0,
                false => 100.0,
            };
            Some(Args::new().with("bpm", bpm).with("at", Beats::ZERO))
        }),
        leaves(transport::STOP, bare),
        edits(transport::TEMPO_DOWN, bare),
        edits(transport::TEMPO_UP, bare),
        edits(transport::TOGGLE_LOOP, bare),
        // With clips selected: loops them.
        edits(transport::TOGGLE_LOOP, clip_selected),
        edits(transport::TOGGLE_METRONOME, bare),
        edits(transport::TOGGLE_PUNCH_IN, bare),
        edits(transport::TOGGLE_PUNCH_OUT, bare),
        leaves(view::COMMAND_PALETTE, bare),
        leaves(view::FIXED_GRID, bare),
        leaves(view::MOVE_TO_MAIN_WINDOW, bare),
        leaves(view::MOVE_TO_NEW_WINDOW, bare),
        leaves(view::NARROW_GRID, bare),
        leaves(view::SET_FOCUS, |_| {
            Some(Args::new().with("panel", "arrangement"))
        }),
        leaves(view::SET_FOCUS, |_| {
            Some(Args::new().with("panel", "mixer"))
        }),
        leaves(view::SET_FOCUS, |s| {
            let clip = find_clip(s, is_midi)?.id;
            s.select_clips(&[clip]);
            Some(Args::new().with("panel", "piano_roll"))
        }),
        leaves(view::SET_FOCUS, |s| {
            let clip = find_clip(s, is_audio)?.id;
            s.select_clips(&[clip]);
            Some(Args::new().with("panel", "sample_editor"))
        }),
        leaves(view::SET_PIANO_ROLL_VIEWPORT, |_| {
            Some(
                Args::new()
                    .with("start", Beats::ZERO)
                    .with("zoom", 250.0)
                    .with("top", 0.0)
                    .with("width", 800.0)
                    .with("height", 300.0)
                    .with("key_height", 16.0),
            )
        }),
        leaves(view::SET_SAMPLE_EDITOR_VIEWPORT, |_| {
            Some(
                Args::new()
                    .with("start", Beats::new(2))
                    .with("zoom", 120.0)
                    .with("width", 640.0),
            )
        }),
        leaves(view::SET_TRACK_HEIGHT, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("track", track).with("height", 80.0))
        }),
        // The master's row.
        leaves(view::SET_TRACK_HEIGHT, |_| {
            Some(Args::new().with("master", true).with("height", 120.0))
        }),
        leaves(view::SET_VIEWPORT, |_| {
            Some(
                Args::new()
                    .with("start", Beats::ZERO)
                    .with("zoom", 40.0)
                    .with("top", 0.0)
                    .with("width", 800.0),
            )
        }),
        leaves(view::SWITCH_DEVICES_PIANO_ROLL, bare),
        leaves(view::TOGGLE_BROWSER, bare),
        leaves(view::TOGGLE_CLIP_EDITOR, bare),
        leaves(view::TOGGLE_DEVICES, bare),
        leaves(view::TOGGLE_FOLLOW, bare),
        leaves(view::TOGGLE_HISTORY, bare),
        leaves(view::TOGGLE_MIXER, bare),
        leaves(view::TOGGLE_PIANO_ROLL, bare),
        leaves(view::TOGGLE_SAMPLE_EDITOR, bare),
        leaves(view::TOGGLE_SNAP, bare),
        leaves(view::TOGGLE_TAKE_LANES, |s| {
            let track = find_track(s, |_| true)?.id;
            Some(Args::new().with("tracks", vec![track]))
        }),
        leaves(view::TRIPLET_GRID, bare),
        leaves(view::WIDEN_GRID, bare),
        leaves(view::ZOOM_IN, bare),
        leaves(view::ZOOM_OUT, bare),
        leaves(view::ZOOM_TO_SELECTION, clip_selected),
        // The markers selected in the Sample Editor.
        leaves(view::ZOOM_TO_SELECTION, marker_selected),
        edits(warp::DELETE_MARKERS, |s| {
            let (clip, beat) = find_marker(s, |w| w.markers.len() > 1)?;
            Some(Args::new().with("clip", clip).with("markers", vec![beat]))
        }),
        // Without arguments: the markers selected in the Sample Editor.
        edits(warp::DELETE_MARKERS, |s| {
            find_marker(s, |w| w.markers.len() > 1)?;
            marker_selected(s)
        }),
        edits(warp::DOUBLE_TEMPO, warped_shown),
        edits(warp::HALVE_TEMPO, warped_shown),
        edits(warp::INSERT_MARKERS, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Audio(audio) = &c.content else {
                return None;
            };
            // Halfway through the clip, where there's no marker.
            let offset = audio.offset.beats()?;
            let at = offset + Beats::from_ticks(c.length.ticks() / 2) + Beats::from_ticks(7);
            Some(Args::new().with("clip", clip).with("at", vec![at]))
        }),
        // Without arguments: at the start marker.
        edits(warp::INSERT_MARKERS, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            let c = project(s).find_clip(clip)?.1.clone();
            s.run_with(&warp::OPEN, Args::new().with("clip", clip))
                .ok()?;
            start_marker(s, middle(&c) + Beats::from_ticks(5));
            Some(Args::new())
        }),
        // On a transient.
        edits(warp::INSERT_MARKERS, |s| {
            let (clip, frame) = find_transient(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("frames", vec![frame as i64]),
            )
        }),
        // Without arguments: at the transients in the time selection.
        edits(warp::INSERT_MARKERS, transient_selected),
        edits(warp::MOVE_AUDIO, |s| {
            let (clip, beat) = find_marker(s, |w| w.markers.len() > 1)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Audio(audio) = &c.content else {
                return None;
            };
            let warp = audio.warp.as_ref()?;
            let index = warp.marker_at(beat)?;
            let frame = warp.markers[index].frame;
            // A little later in the file, short of the next marker's frame.
            let next = warp.markers.get(index + 1).map_or(u64::MAX, |m| m.frame);
            let frame = (frame + 100).min(next - 1);
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("marker", beat)
                    .with("frame", frame as i64),
            )
        }),
        edits(warp::MOVE_MARKERS, |s| {
            let (clip, beat) = find_marker(s, |_| true)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("markers", vec![beat])
                    .with("by", Beats::from_ratio(-1, 4)),
            )
        }),
        edits(warp::NUDGE_LEFT, marker_selected),
        edits(warp::NUDGE_RIGHT, marker_selected),
        leaves(warp::OPEN, |s| {
            let clip = find_clip(s, is_audio)?.id;
            Some(Args::new().with("clip", clip))
        }),
        leaves(warp::SELECT_MARKERS, |s| {
            let (clip, beat) = find_marker(s, |_| true)?;
            Some(Args::new().with("clip", clip).with("markers", vec![beat]))
        }),
        leaves(warp::SELECT_TIME, |s| {
            let (clip, beat) = find_marker(s, |_| true)?;
            let args = Args::new()
                .with("clip", clip)
                .with("start", beat)
                .with("end", beat + Beats::new(1));
            Some(args)
        }),
        edits(warp::SET_GAIN, |s| {
            let clip = find_clip(s, is_audio)?;
            let ClipContent::Audio(audio) = &clip.content else {
                return None;
            };
            let gain = other_level(Some(f64::from(audio.gain)));
            Some(Args::new().with("clip", clip.id).with("gain", gain))
        }),
        edits(warp::SET_TEMPO, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            Some(Args::new().with("clip", clip).with("bpm", 140.0))
        }),
        edits(warp::SET_WARP, |s| {
            let clip = find_clip(s, |c| is_audio(c) && !is_warped(c))?.id;
            Some(Args::new().with("clip", clip).with("on", true))
        }),
        edits(warp::SET_WARP, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            Some(Args::new().with("clip", clip).with("on", false))
        }),
        // Without arguments: the other way, for the clip the Sample Editor
        // shows.
        edits(warp::SET_WARP, |s| {
            let clip = find_clip(s, is_audio)?.id;
            s.run_with(&warp::OPEN, Args::new().with("clip", clip))
                .ok()?;
            Some(Args::new())
        }),
        edits(warp_mode::SET_MODE, |s| {
            let (clip, warp) = find_warp(s)?;
            let mode = match warp.mode {
                WarpMode::RePitch => WarpMode::Slice,
                WarpMode::Slice => WarpMode::Spectral,
                WarpMode::Spectral => WarpMode::RePitch,
            };
            Some(Args::new().with("clip", clip).with("mode", mode.key()))
        }),
        edits(warp_mode::SET_TRANSPOSE, |s| {
            let (clip, warp) = find_warp(s)?;
            let semitones: i64 = if warp.transpose == 7 { -5 } else { 7 };
            Some(Args::new().with("clip", clip).with("semitones", semitones))
        }),
        edits(warp_mode::SET_DETUNE, |s| {
            let (clip, warp) = find_warp(s)?;
            let cents: i64 = if warp.detune == 25 { -10 } else { 25 };
            Some(Args::new().with("clip", clip).with("cents", cents))
        }),
        edits(warp_mode::SET_PRESERVE, |s| {
            let (clip, warp) = find_warp(s)?;
            let preserve = match warp.slice.preserve {
                Preserve::Sixteenth => Preserve::Transients,
                _ => Preserve::Sixteenth,
            };
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("preserve", preserve.key()),
            )
        }),
        edits(warp_mode::SET_TRANSIENT_LOOP, |s| {
            let (clip, warp) = find_warp(s)?;
            let next = (warp.slice.transient_loop.step() + 1) % TransientLoop::ALL.len();
            let mode = TransientLoop::ALL[next].key();
            Some(Args::new().with("clip", clip).with("mode", mode))
        }),
        edits(warp_mode::SET_TRANSIENT_ENVELOPE, |s| {
            let (clip, warp) = find_warp(s)?;
            let envelope = if warp.slice.envelope == 50.0 {
                80.0
            } else {
                50.0
            };
            Some(Args::new().with("clip", clip).with("envelope", envelope))
        }),
        edits(warp_mode::SET_FORMANTS, |s| {
            let (clip, warp) = find_warp(s)?;
            let formants = if warp.spectral.formants == 40.0 {
                70.0
            } else {
                40.0
            };
            Some(Args::new().with("clip", clip).with("formants", formants))
        }),
        edits(warp_mode::SET_HIGH_QUALITY, |s| {
            let (clip, warp) = find_warp(s)?;
            let on = !warp.spectral.high_quality;
            Some(Args::new().with("clip", clip).with("on", on))
        }),
        edits(warp_mode::SET_ENVELOPE, |s| {
            let (clip, warp) = find_warp(s)?;
            let envelope = if warp.spectral.envelope == 64.0 {
                256.0
            } else {
                64.0
            };
            Some(Args::new().with("clip", clip).with("envelope", envelope))
        }),
        leaves(warp_mode::SHOW_LANE, |s| {
            let clip = find_clip(s, is_audio)?.id;
            Some(Args::new().with("clip", clip).with("param", "preserve"))
        }),
        edits(rewarp::FROM_HERE, marker_frame),
        edits(rewarp::FROM_HERE_AT_SONG_TEMPO, marker_frame),
        edits(rewarp::FROM_HERE_STRAIGHT, marker_frame),
        edits(rewarp::SELECTION_AS_LOOP, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Audio(audio) = &c.content else {
                return None;
            };
            // All of the clip.
            let offset = audio.offset.beats()?;
            let args = Args::new()
                .with("clip", clip)
                .with("start", offset)
                .with("end", offset + c.length);
            Some(args)
        }),
        // Without arguments: the Sample Editor's time selection.
        edits(rewarp::SELECTION_AS_LOOP, |s| {
            let (clip, _) = find_marker(s, |_| true)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Audio(audio) = &c.content else {
                return None;
            };
            // Its first two beats.
            let offset = audio.offset.beats()?;
            let args = Args::new()
                .with("clip", clip)
                .with("start", offset)
                .with("end", offset + Beats::new(2).min(c.length));
            s.run_with(&warp::SELECT_TIME, args).ok()?;
            Some(Args::new())
        }),
        edits(rewarp::SET_DOWNBEAT, marker_frame),
        edits(rewarp::SONG_TEMPO_FROM_HERE, marker_frame),
        edits(transient::DELETE, |s| {
            let (clip, frame) = find_transient(s)?;
            Some(
                Args::new()
                    .with("clip", clip)
                    .with("frames", vec![frame as i64]),
            )
        }),
        // Without arguments: those in the Sample Editor's time selection.
        edits(transient::DELETE, transient_selected),
        edits(transient::INSERT, |s| {
            let (clip, frame) = find_transient(s)?;
            // Just after one, where there's none.
            let frames = vec![frame as i64 + 3];
            Some(Args::new().with("clip", clip).with("frames", frames))
        }),
        // Without arguments: at the start marker, an eighth of a beat after
        // the transient halfway through the groove.
        edits(transient::INSERT, |s| {
            let (clip, _) = find_transient(s)?;
            let c = project(s).find_clip(clip)?.1.clone();
            s.run_with(&warp::OPEN, Args::new().with("clip", clip))
                .ok()?;
            start_marker(s, middle(&c) + Beats::from_ratio(1, 8));
            Some(Args::new())
        }),
        edits(transient::MOVE, |s| {
            let (clip, frame) = find_transient(s)?;
            let (_, c) = project(s).find_clip(clip)?;
            let ClipContent::Audio(audio) = &c.content else {
                return None;
            };
            let onsets = audio.warp.as_ref()?.onsets.as_ref()?;
            // A little later, short of the next.
            let (_, next) = onsets.room(frame)?;
            let to = (frame + 50).min(next.map_or(u64::MAX, |next| next - 1));
            let args = Args::new()
                .with("clip", clip)
                .with("frame", frame as i64)
                .with("to", to as i64);
            (to > frame).then_some(args)
        }),
        edits(transient::RESET, |s| {
            let (clip, _) = find_transient(s)?;
            Some(Args::new().with("clip", clip))
        }),
        leaves(workspace::DELETE, bare),
        leaves(workspace::RENAME, bare),
        leaves(workspace::RESET, bare),
        leaves(workspace::SAVE_AS, bare),
    ];
    examples.extend(switches);
    examples
}

fn project(s: &Rig) -> &Project {
    s.document.project()
}

/// The one of `found` that `s` picks, or `None` if there's none.
fn pick<T>(s: &Rig, found: impl Iterator<Item = T>) -> Option<T> {
    let mut found: Vec<T> = found.collect();
    let i = s.pick.checked_rem(found.len())?;
    Some(found.swap_remove(i))
}

/// A track that `keep` keeps.
fn find_track(s: &Rig, keep: impl Fn(&Track) -> bool) -> Option<&Track> {
    pick(s, project(s).tracks.iter().filter(|t| keep(t)))
}

/// A clip on a track that `keep` keeps.
fn find_clip(s: &Rig, keep: impl Fn(&Clip) -> bool) -> Option<&Clip> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    pick(s, clips.filter(|c| keep(c)))
}

fn is_midi(clip: &Clip) -> bool {
    matches!(clip.content, ClipContent::Midi(_))
}

fn is_audio(clip: &Clip) -> bool {
    matches!(clip.content, ClipContent::Audio(_))
}

fn is_warped(clip: &Clip) -> bool {
    matches!(&clip.content, ClipContent::Audio(audio) if audio.is_warped())
}

/// An audio clip's gain curve, if it has one.
fn curve(clip: &Clip) -> Option<&[musicable_model::automation::Point]> {
    match &clip.content {
        ClipContent::Audio(audio) => audio.lane(ClipParam::Gain).map(|l| l.points.as_slice()),
        ClipContent::Midi(_) => None,
    }
}

/// A warped clip on a track whose warp `keep` keeps, and the beat of one of
/// its markers.
fn find_marker(s: &Rig, keep: impl Fn(&Warp) -> bool) -> Option<(ClipId, Beats)> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    let found = clips.flat_map(|clip| match &clip.content {
        ClipContent::Audio(AudioClip {
            warp: Some(warp), ..
        }) if keep(warp) => warp.markers.iter().map(|m| (clip.id, m.beat)).collect(),
        _ => Vec::new(),
    });
    pick(s, found)
}

/// A warped clip on a track, and its warp.
fn find_warp(s: &Rig) -> Option<(ClipId, Warp)> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    let found = clips.filter_map(|clip| match &clip.content {
        ClipContent::Audio(AudioClip {
            warp: Some(warp), ..
        }) => Some((clip.id, warp.clone())),
        _ => None,
    });
    pick(s, found)
}

/// A warped clip on a track with transients, and the frame of one of
/// those in sight.
fn find_transient(s: &Rig) -> Option<(ClipId, u64)> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    let found = clips.flat_map(|clip| match &clip.content {
        ClipContent::Audio(AudioClip {
            warp: Some(warp), ..
        }) => warp.transients().map(|o| (clip.id, o.frame)).collect(),
        _ => Vec::new(),
    });
    pick(s, found)
}

/// A MIDI clip on a track with notes, and its notes' ids.
fn find_notes(s: &Rig) -> Option<(ClipId, Vec<NoteId>)> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    let found = clips.filter_map(|clip| match &clip.content {
        ClipContent::Midi(midi) if !midi.notes.is_empty() => {
            Some((clip.id, midi.notes.iter().map(|n| n.id).collect()))
        }
        _ => None,
    });
    pick(s, found)
}

/// A note in a MIDI clip on a track, and the clip.
fn find_note(s: &Rig) -> Option<(ClipId, NoteId)> {
    let (clip, notes) = find_notes(s)?;
    Some((clip, pick(s, notes.into_iter())?))
}

/// A MIDI clip on a track that moves a controller, the controller, and
/// where its curve's first point is.
fn find_curve(s: &Rig) -> Option<(ClipId, Controller, Beats)> {
    let clips = project(s).tracks.iter().flat_map(|t| &t.clips);
    let midi = clips.filter_map(|clip| match &clip.content {
        ClipContent::Midi(midi) => Some((clip.id, midi)),
        ClipContent::Audio(_) => None,
    });
    let curves = midi.flat_map(|(clip, midi)| {
        let curves = midi.controllers.iter();
        curves.filter_map(move |(&controller, points)| Some((clip, controller, points.first()?.at)))
    });
    pick(s, curves)
}

/// A take with a clip that `keep` keeps.
fn find_take(s: &Rig, keep: impl Fn(&Clip) -> bool) -> Option<&Take> {
    let takes = project(s).tracks.iter().flat_map(|t| &t.takes);
    pick(s, takes.filter(|take| take.clips.iter().any(&keep)))
}

/// A device of a kind `keep` keeps, on the tracks or the master.
fn find_device(s: &Rig, keep: impl Fn(&DeviceKind) -> bool) -> Option<DeviceId> {
    let project = project(s);
    let tracks = project.tracks.iter().flat_map(|t| &t.devices);
    let devices = tracks.chain(&project.master.devices);
    pick(s, devices.filter(|d| keep(&d.kind))).map(|d| d.id)
}

fn is_sampler(kind: &DeviceKind) -> bool {
    kind.native()
        .is_some_and(|native| native.kind == NativeKind::Sampler)
}

/// A track with a send, and the return track it goes to.
fn find_send(s: &Rig) -> Option<(TrackId, TrackId)> {
    let tracks = project(s).tracks.iter();
    let sends = tracks.flat_map(|t| t.sends.iter().map(|send| (t.id, send.to)));
    pick(s, sends)
}

/// Selects a clip, which gives the arrangement the focus, for a command
/// without arguments.
fn clip_selected(s: &mut Rig) -> Option<Args> {
    let clip = find_clip(s, |_| true)?.id;
    s.select_clips(&[clip]);
    Some(Args::new())
}

/// Selects time inside a clip at least four ticks long, from a quarter of
/// the way in to halfway, on its track, for a command without arguments.
fn time_selected(s: &mut Rig) -> Option<Args> {
    let tracks = project(s).tracks.iter();
    let clips = tracks.flat_map(|t| t.clips.iter().map(|clip| (t.id, clip)));
    let (track, clip) = pick(s, clips.filter(|(_, c)| c.length.ticks() >= 4))?;
    let quarter = Beats::from_ticks(clip.length.ticks() / 4);
    let args = Args::new()
        .with("start", clip.start + quarter)
        .with("end", clip.start + quarter * 2)
        .with("tracks", vec![track]);
    s.run_with(&edit::SELECT_TIME, args).ok()?;
    Some(Args::new())
}

/// Selects a warp marker, which gives the Sample Editor the focus, for a
/// command without arguments.
fn marker_selected(s: &mut Rig) -> Option<Args> {
    let (clip, beat) = find_marker(s, |_| true)?;
    let args = Args::new().with("clip", clip).with("markers", vec![beat]);
    s.run_with(&warp::SELECT_MARKERS, args).ok()?;
    Some(Args::new())
}

/// Selects a stretch of a warped clip's own time around one of its
/// transients in the Sample Editor, which gives it the focus, for a command
/// without arguments.
fn transient_selected(s: &mut Rig) -> Option<Args> {
    let (clip, frame) = find_transient(s)?;
    let (_, c) = project(s).find_clip(clip)?;
    let ClipContent::Audio(audio) = &c.content else {
        return None;
    };
    let rate = s.document.editor().sample_rate_of(&audio.file);
    let beat = Beats::from_f64(audio.warp.as_ref()?.beat_at(frame as f64, rate));
    let around = Beats::from_ticks(9);
    let args = Args::new()
        .with("clip", clip)
        .with("start", beat - around)
        .with("end", beat + around);
    s.run_with(&warp::SELECT_TIME, args).ok()?;
    Some(Args::new())
}

/// A warped clip and the frame of one of its markers, for the commands
/// that warp it from a point.
fn marker_frame(s: &mut Rig) -> Option<Args> {
    let (clip, beat) = find_marker(s, |_| true)?;
    let (_, c) = project(s).find_clip(clip)?;
    let ClipContent::Audio(audio) = &c.content else {
        return None;
    };
    let warp = audio.warp.as_ref()?;
    let frame = warp.markers[warp.marker_at(beat)?].frame;
    Some(Args::new().with("clip", clip).with("frame", frame as i64))
}

/// Shows a warped clip in the Sample Editor, for a command without
/// arguments.
fn warped_shown(s: &mut Rig) -> Option<Args> {
    let (clip, _) = find_marker(s, |_| true)?;
    s.run_with(&warp::OPEN, Args::new().with("clip", clip))
        .ok()?;
    Some(Args::new())
}

/// Selects a note, which gives the piano roll the focus, for a command
/// without arguments.
fn note_selected(s: &mut Rig) -> Option<Args> {
    let (clip, note) = find_note(s)?;
    let args = Args::new().with("clip", clip).with("notes", vec![note]);
    s.run_with(&notes::SELECT, args).ok()?;
    Some(Args::new())
}

fn start_marker(s: &mut Rig, at: Beats) {
    let args = Args::new().with("at", at);
    s.run_with(&transport::SET_START_MARKER, args).unwrap();
}

/// Halfway through `clip`, to the tick.
fn middle(clip: &Clip) -> Beats {
    clip.start + Beats::from_ticks(clip.length.ticks() / 2)
}

/// A name other than `name`, to rename something back and forth.
fn other_name(name: &str) -> &'static str {
    match name {
        "Lead" => "Pad",
        _ => "Lead",
    }
}

/// A level other than `level`: a half, or else a quarter. It suits gains,
/// pans, sends, velocities and plugin parameters alike.
fn other_level(level: Option<f64>) -> f64 {
    match level == Some(0.5) {
        true => 0.25,
        false => 0.5,
    }
}

fn next_clip_type(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "Hard",
        ClipType::Hard => "Soft",
        ClipType::Soft => "Off",
    }
}

/// A loop region other than `project`'s.
fn other_loop(project: &Project) -> (Beats, Beats) {
    let region = BeatRange::new(Beats::new(4), Beats::new(12));
    match project.loop_region == region {
        true => (Beats::new(8), Beats::new(16)),
        false => (region.start, region.end),
    }
}

/// A track of each kind, and a spare audio track that feeds the group: on
/// the audio track, an audio clip with fades and another clip, two takes,
/// and a plugin with a state and a parameter set after it; on the spare
/// track, a warped clip with three markers and a gain curve; on the MIDI
/// track, a MIDI clip with notes, one off the grid and bending, and the
/// sustain pedal, a MIDI take and a sampler; sends from both to the return
/// track; an EQ on the group; two locators; a test device on the master;
/// and lanes automating the audio track's volume, the master's pan and the
/// plugin's parameter, which someone overrode.
fn fixture() -> Project {
    let mut project = Project::new();
    let audio = project.add_track("Audio", TrackKind::Audio);
    let spare = project.add_track("Spare", TrackKind::Audio);
    let midi = project.add_track("MIDI", TrackKind::Midi);
    let group = project.add_track("Group", TrackKind::Group);
    let reverb = project.add_track("Reverb", TrackKind::Return);
    project.track_mut(spare).unwrap().output = Bus::Group(group);
    for track in [audio, midi] {
        let send = project.new_send(reverb);
        project.track_mut(track).unwrap().sends.push(send);
    }
    let range = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let mut faded = AudioClip::new("a.wav");
    faded.fade_in = Beats::new(1);
    faded.fade_out = Beats::new(1);
    let mut bass = MidiClip::default();
    bass.add_note(Beats::ZERO, Beats::new(1), 36, 0.75);
    bass.add_note(Beats::new(1), Beats::new(1), 43, 0.5);
    let start = Beats::new(2) + Beats::from_ticks(1);
    let bent = bass.add_note(start, Beats::from_ratio(1, 2), 48, 1.0);
    let bend = [(0, 0.0), (1, 2.0)].map(|(at, value)| Point::new(Beats::from_ratio(at, 4), value));
    bent.expression.pitch_bend = bend.to_vec();
    let pedal = [(0, 1.0), (2, 0.0)].map(|(at, value)| Point::new(Beats::new(at), value));
    bass.set_controller(Controller::SUSTAIN, pedal.to_vec());
    // As if a fourth note had been added and deleted.
    bass.new_note_id();
    let mut groove = AudioClip::new(GROOVE);
    let markers = [(0, 0), (48_000, 2), (60_000, 3)];
    let markers = markers.map(|(frame, beat)| WarpMarker {
        frame,
        beat: Beats::new(beat),
    });
    groove.warp = Some(Warp {
        markers: markers.to_vec(),
        onsets: Some(groove_transients()),
        ..Warp::new(0, Beats::ZERO, 100.0)
    });
    groove.offset = Offset::Beats(Beats::from_ratio(1, 2));
    let swell = [(0, 0.5), (4, 1.0)];
    let point = |at, value| musicable_model::automation::Point::new(Beats::new(at), value);
    let swell = swell.map(|(at, value)| point(at, value));
    groove.set_lane(ClipParam::Gain, swell.to_vec());
    // Slice's sixteenths, then its transients from beat 2.
    groove.set_lane(ClipParam::Preserve, vec![point(0, 5.0), point(2, 0.0)]);
    let clips = [
        (audio, "Faded", range(0, 8), ClipContent::Audio(faded)),
        (
            audio,
            "Loop",
            range(8, 12),
            ClipContent::Audio(AudioClip::new("b.wav")),
        ),
        (spare, "Groove", range(4, 8), ClipContent::Audio(groove)),
        (midi, "Bass", range(4, 8), ClipContent::Midi(bass)),
    ];
    for (track, name, range, content) in clips {
        let clip = project.new_clip(name, range, content);
        project.track_mut(track).unwrap().clips.push(clip);
    }
    for name in ["Take 1", "Take 2"] {
        let mut take = project.new_take(name);
        let recorded = ClipContent::Audio(AudioClip::new(RECORDING));
        take.clips
            .push(project.new_clip("Audio", range(0, 4), recorded));
        project.track_mut(audio).unwrap().takes.push(take);
    }
    let mut played = MidiClip::default();
    played.add_note(Beats::ZERO, Beats::new(2), 60, 0.5);
    let mut take = project.new_take("Take 1");
    let played = ClipContent::Midi(played);
    take.clips
        .push(project.new_clip("MIDI", range(8, 12), played));
    project.track_mut(midi).unwrap().takes.push(take);
    for (name, at) in [("Verse", 0), ("Chorus", 16)] {
        let locator = project.new_locator(name, Beats::new(at));
        project.locators.push(locator);
    }
    let plugin = DeviceKind::Plugin(PluginDevice {
        format: PluginFormat::Clap,
        id: "org.example.effect".into(),
        name: "Effect".into(),
        instrument: false,
        settings: PluginSettings {
            state: Some(PluginState::new(*b"state")),
            params: vec![PluginParam { id: 1, value: 0.5 }],
        },
    });
    let devices = [
        (Chain::Track(audio), plugin),
        (
            Chain::Track(midi),
            DeviceKind::Native(NativeDevice::new(NativeKind::Sampler)),
        ),
        (
            Chain::Track(group),
            DeviceKind::Native(NativeDevice::new(NativeKind::Eq)),
        ),
        (Chain::Master, DeviceKind::TestGain { gain: 0.5 }),
    ];
    let mut ids = Vec::new();
    for (chain, kind) in devices {
        let device = project.new_device(kind);
        ids.push(device.id);
        project.devices_mut(chain).unwrap().push(device);
    }
    let point = |at, value| musicable_model::automation::Point::new(Beats::new(at), value);
    let mut overridden = Lane::new(
        ParamAddress::Device(ids[0], 1),
        vec![point(0, 0.25), point(4, 0.75)],
    );
    overridden.overridden = true;
    project.automation = vec![
        Lane::new(
            ParamAddress::Volume(Chain::Track(audio)),
            vec![point(0, 1.0), point(8, 0.5)],
        ),
        Lane::new(ParamAddress::Pan(Chain::Master), vec![point(0, -0.5)]),
        overridden,
    ];
    project
}

/// The [`fixture`], with the editor knowing its audio files, and the
/// library its samples and a preset, picking the first of everything.
/// The groove's transients, as the fixture's warped clip has them: what
/// analysis found, but for one the user added and one deleted.
fn groove_transients() -> Onsets {
    let onset = |frame, origin| Onset {
        frame,
        strength: 0.5,
        percussiveness: 0.75,
        origin,
    };
    let found = GROOVE_ONSETS.map(|frame| onset(frame, Origin::Found));
    let mut onsets = Onsets::new(found);
    onsets.delete(&[GROOVE_ONSETS[1]]);
    onsets.insert(18_000);
    onsets
}

/// Where analysis finds the groove's transients.
const GROOVE_ONSETS: [u64; 5] = [6_000, 12_000, 24_000, 36_000, 54_000];

fn setup() -> Rig {
    let mut s = Setup::new(fixture());
    let onsets = GROOVE_ONSETS.map(|frame| musicable_analysis::Onset {
        frame,
        strength: 0.5,
        percussiveness: 0.75,
    });
    // A steady beat at 120 BPM, for Warp From Here.
    let beats: Vec<u64> = (0..8).map(|k| k * 24_000).collect();
    let steady = musicable_analysis::Steady {
        bpm: 120.0,
        origin: 0.0,
        error_ms: 0.0,
        drift_ms: 0.0,
    };
    let analysis = Analysis {
        pulse: true,
        steady: Some(steady),
        downbeats: vec![0, 4],
        tracker: musicable_analysis::Tracker::Network,
        agreement: Some(1.0),
        events: beats.len(),
        beats,
        ..Analysis::with_onsets(48_000, 4 * 48_000, onsets.to_vec())
    };
    let analyses = &mut s.document.editor_mut().analyses;
    analyses.insert(GROOVE.to_owned(), Arc::new(analysis));
    // Two seconds of the loop, four of the groove, and ten of the others:
    // longer than their clips.
    let read = [
        (LOOP, 2),
        ("a.wav", 10),
        ("b.wav", 10),
        (GROOVE, 4),
        (RECORDING, 10),
    ];
    let files = &mut s.document.editor_mut().files;
    for (path, seconds) in read {
        let file = AudioFile {
            sample_rate: 48_000,
            frames: 48_000 * seconds,
            channels: 2,
            tempo: None,
        };
        files.insert(path.to_owned(), file);
    }
    s.library.files = vec![PathBuf::from(KICK), PathBuf::from("/samples/snare.wav")];
    let eq = DeviceKind::Native(NativeDevice::new(NativeKind::Eq));
    s.library.presets.insert(PathBuf::from(PRESET), eq);
    Rig { setup: s, pick: 0 }
}

/// `project` without the counters that hand out ids, which undo leaves as
/// they are, so that no id is used twice. Undo puts back everything else.
fn masked(project: &Project) -> Project {
    let mut project = project.clone();
    project.next_track_id = 0;
    project.next_send_id = 0;
    project.next_device_id = 0;
    project.next_clip_id = 0;
    project.next_locator_id = 0;
    project.next_take_id = 0;
    for track in &mut project.tracks {
        let takes = track.takes.iter_mut().flat_map(|take| &mut take.clips);
        for clip in track.clips.iter_mut().chain(takes) {
            if let ClipContent::Midi(midi) = &mut clip.content {
                midi.next_note_id = 0;
            }
        }
    }
    project
}

/// The project's counters that hand out ids, and each MIDI clip's, by the
/// clip's id.
fn counters(project: &Project) -> ([u32; 6], BTreeMap<ClipId, u32>) {
    let ids = [
        project.next_track_id,
        project.next_send_id,
        project.next_device_id,
        project.next_clip_id,
        project.next_locator_id,
        project.next_take_id,
    ];
    let notes = every_clip(project).filter_map(|clip| match &clip.content {
        ClipContent::Midi(midi) => Some((clip.id, midi.next_note_id)),
        ClipContent::Audio(_) => None,
    });
    (ids, notes.collect())
}

/// Every clip, on the tracks and in their takes.
fn every_clip(project: &Project) -> impl Iterator<Item = &Clip> {
    project.tracks.iter().flat_map(|track| {
        let takes = track.takes.iter().flat_map(|take| &take.clips);
        track.clips.iter().chain(takes)
    })
}

/// Checks that no counter in `later` is behind where it was in `earlier`,
/// for the clips in both, and that every MIDI clip's is past its notes',
/// so ids handed out are never handed out again.
fn kept_counters(earlier: &Project, later: &Project, what: &str) {
    let (before, before_notes) = counters(earlier);
    let (after, after_notes) = counters(later);
    let behind = before
        .iter()
        .zip(&after)
        .any(|(before, after)| after < before);
    assert!(
        !behind,
        "{what} took the counters back: {before:?} to {after:?}"
    );
    for (clip, before) in before_notes {
        if let Some(&after) = after_notes.get(&clip) {
            assert!(
                after >= before,
                "{what} took the note counter of clip {} back from {before} to {after}",
                clip.0
            );
        }
    }
    // A clip undo brings back has no counter to keep up with: its notes'
    // ids must still be below its own.
    for clip in every_clip(later) {
        if let ClipContent::Midi(midi) = &clip.content {
            let below = midi.notes.iter().all(|n| n.id.0 < midi.next_note_id);
            assert!(
                below,
                "{what} left clip {} handing out its notes' ids",
                clip.id.0
            );
        }
    }
}

/// Checks what's true of every project: it saves, which checks every rule
/// of the file format, such as ids being unique and below their counters,
/// and nothing naming what has gone, and it loads back as it was.
fn saves(project: &Project, what: &str) {
    let text = match musicable_io::project::to_string(project) {
        Ok(text) => text,
        Err(error) => panic!("{what} left a project that doesn't save: {error:?}"),
    };
    let loaded = musicable_io::project::from_str(&text);
    let loaded = loaded.unwrap_or_else(|error| panic!("{what}: {error:?}"));
    assert_eq!(
        &loaded, project,
        "{what} left a project that loads back changed"
    );
}

/// Applies the edits of each of `committed` to `mirror`, as the engine
/// follows them, checking that each edit finds the project as the one
/// before it left it.
fn replay(mirror: &mut Document, committed: &[Committed], what: &str) {
    for transaction in committed {
        mirror.transact("Replay", |tx| {
            for edit in &transaction.edits {
                tx.apply(edit);
            }
        });
        let replayed = mirror.take_committed();
        let edits = replayed.first().map_or(&[][..], |c| &c.edits);
        assert_eq!(edits, transaction.edits, "{what}: the edits follow on");
    }
}

fn names(committed: &[Committed]) -> Vec<&str> {
    committed.iter().map(|c| c.name.as_str()).collect()
}

/// Runs `example` on `s`: gets it ready, and runs it, if there's anything
/// for it to act on. Returns what it committed, and the project just
/// before it ran.
fn run(
    s: &mut Rig,
    example: &Example,
) -> Option<(Result<Value, CommandError>, Vec<Committed>, Project)> {
    let before = project(s).clone();
    let args = (example.prepare)(s);
    let what = &example.command;
    assert!(
        s.document.take_committed().is_empty(),
        "getting {what} ready changes nothing"
    );
    assert_eq!(project(s), &before, "getting {what} ready changes nothing");
    let result = s.run_with(&example.command, args?);
    let committed = s.document.take_committed();
    Some((result, committed, before))
}

#[test]
fn every_command_has_an_example() {
    let s = setup();
    let registered: BTreeSet<&CommandId> = s.registry.iter().map(|c| &c.id).collect();
    let examples = examples();
    let tried: BTreeSet<&CommandId> = examples.iter().map(|e| &e.command).collect();
    let missing: Vec<_> = registered.difference(&tried).collect();
    assert!(
        missing.is_empty(),
        "give these commands an example in every_command.rs: {missing:?}"
    );
    let unknown: Vec<_> = tried.difference(&registered).collect();
    assert!(unknown.is_empty(), "no command is called {unknown:?}");
    let mut does = BTreeMap::new();
    for example in &examples {
        let first = does.entry(&example.command).or_insert(example.does);
        assert_eq!(*first, example.does, "{}'s examples agree", example.command);
    }
}

#[test]
fn undo_takes_back_what_each_command_does_and_redo_makes_it_again() {
    saves(&fixture(), "the fixture");
    for example in examples() {
        let what = &example.command;
        let mut s = setup();
        let Some((result, committed, before)) = run(&mut s, &example) else {
            panic!("{what} finds nothing to act on in the fixture");
        };
        assert!(result.is_ok(), "{what}: {result:?}");
        let mut mirror = Document::new(before.clone());
        replay(&mut mirror, &committed, &format!("{what}"));
        if example.does != Does::Edits {
            assert_eq!(names(&committed), [] as [&str; 0], "{what} leaves no step");
            assert_eq!(project(&s), &before, "{what} leaves the project alone");
            assert_eq!(s.document.undo_name(), None, "{what} leaves no step");
            continue;
        }
        let [step] = &committed[..] else {
            panic!("{what} is one step, not {:?}", names(&committed));
        };
        let after = project(&s).clone();
        assert_ne!(
            masked(&after),
            masked(&before),
            "{what} changes the project"
        );
        assert_eq!(s.document.undo_name(), Some(step.name.as_str()));
        assert_eq!(masked(mirror.project()), masked(&after));
        saves(&after, &format!("{what}"));

        s.run_with(&edit::UNDO, Args::new()).unwrap();
        let undone = s.document.take_committed();
        let name = format!("Undo {}", step.name);
        assert_eq!(names(&undone), [name.as_str()]);
        let undo = format!("undoing {what}");
        assert_eq!(masked(project(&s)), masked(&before), "{undo}");
        kept_counters(&after, project(&s), &undo);
        assert!(!s.document.is_modified(), "{undo}");
        assert_eq!(s.document.undo_name(), None, "{undo}");
        assert_eq!(s.document.redo_name(), Some(step.name.as_str()), "{undo}");
        replay(&mut mirror, &undone, &undo);
        assert_eq!(masked(mirror.project()), masked(project(&s)), "{undo}");

        s.run_with(&edit::REDO, Args::new()).unwrap();
        let redone = s.document.take_committed();
        let name = format!("Redo {}", step.name);
        assert_eq!(names(&redone), [name.as_str()]);
        let redo = format!("redoing {what}");
        assert_eq!(project(&s), &after, "{redo}");
        assert_eq!(s.document.undo_name(), Some(step.name.as_str()), "{redo}");
        assert_eq!(s.document.redo_name(), None, "{redo}");
        replay(&mut mirror, &redone, &redo);
        assert_eq!(masked(mirror.project()), masked(&after), "{redo}");
    }
}

/// Runs each example as an agent runs it, through `run_command`: its
/// arguments written as JSON and read back as [`json::agent_call`] reads
/// them, and its result written as JSON. It must do just what it does from
/// the UI, unless it's left to the user, or needs an argument the app would
/// have asked the user for.
#[test]
fn agents_run_each_command_as_the_ui_does() {
    for example in examples() {
        let what = &example.command;
        let mut ui = setup();
        let mut agent = setup();
        let (Some(args), Some(same)) = ((example.prepare)(&mut ui), (example.prepare)(&mut agent))
        else {
            panic!("{what} finds nothing to act on in the fixture");
        };
        assert_eq!(args, same, "{what} gets ready the same way twice");
        let command = ui.registry.get(what).expect("it has an example").clone();
        let ran = ui.run_with(what, args.clone());
        // What only the user can see or answer is the user's.
        if ui.requests.iter().any(|request| !request.for_agents()) {
            let asked = &ui.requests;
            assert!(
                !command.agents,
                "{what} asks for {asked:?}, which is the user's"
            );
        }

        let written = json::args_to_json(&args);
        let call = json::agent_call(&agent.registry, what.as_str(), &written);
        if !command.agents {
            assert_eq!(call, Err(CommandError::NotForAgents(what.clone())));
            continue;
        }
        let asked = (command.params.iter()).find(|p| p.asked && args.value(&p.name).is_none());
        if let Some(param) = asked {
            let missing = CommandError::MissingArgument(param.name.to_string());
            assert_eq!(call, Err(missing), "{what}");
            continue;
        }
        let (id, read) = call.unwrap_or_else(|error| panic!("{what}: {error}"));
        let result = agent.run_with(&id, read);
        let as_json = |result: &Result<Value, CommandError>| {
            (result.as_ref())
                .map(json::to_json)
                .map_err(ToString::to_string)
        };
        assert_eq!(as_json(&result), as_json(&ran), "{what}");
        assert_eq!(agent.committed_names(), ui.committed_names(), "{what}");
        assert_eq!(project(&agent), project(&ui), "{what}");
        assert_eq!(agent.document.editor(), ui.document.editor(), "{what}");
        assert_eq!(
            agent.document.undo_name(),
            ui.document.undo_name(),
            "{what}"
        );
        assert_eq!(agent.requests, ui.requests, "{what}");
        assert_eq!(agent.transport.calls, ui.transport.calls, "{what}");
        assert_eq!(agent.library.calls, ui.library.calls, "{what}");
    }
}

#[test]
fn commands_run_as_a_batch_make_one_step_or_none() {
    let mut s = setup();
    let fixture = project(&s).clone();
    let tempo = Args::new().with("bpm", 92.0);
    let locator = Args::new().with("at", Beats::new(48)).with("name", "Drop");
    let batch = [
        (track::NEW_MIDI_TRACK, Args::new()),
        (transport::SET_TEMPO, tempo),
        (locator::ADD, locator),
    ];
    let values = s.run_batch("Start a Beat", &batch).unwrap();
    let after = project(&s).clone();
    let track = after.tracks.iter().find(|t| t.name == "MIDI 2");
    assert_eq!(values[0], Value::Track(track.expect("the new track").id));
    assert_eq!(values.len(), 3);
    // Each transaction keeps its name for the engine, and the undo history
    // has one step.
    let committed = s.committed_names();
    assert_eq!(committed.len(), 3);
    assert_eq!(s.document.steps().len(), 1);
    assert_eq!(s.document.undo_name(), Some("Start a Beat"));
    assert!(s.document.undo());
    assert_eq!(masked(project(&s)), masked(&fixture));
    assert!(s.document.redo());
    assert_eq!(project(&s), &after);
    s.document.take_committed();

    // One that fails takes back all that ran before it, asks the app for
    // nothing, and leaves the history alone.
    let editor = s.document.editor().clone();
    let failing = [
        (track::NEW_AUDIO_TRACK, Args::new()),
        (file::OPEN, Args::new()),
    ];
    let error = s.run_batch("Open", &failing).unwrap_err();
    assert_eq!(error.index, 1);
    assert_eq!(
        error.error,
        CommandError::invalid("file.open runs on its own, not among other commands")
    );
    let missing = Args::new().with("locator", 999_i64).with("name", "Outro");
    let failing = [
        (track::NEW_AUDIO_TRACK, Args::new()),
        (
            edit::SELECT_TRACKS,
            Args::new().with("tracks", Vec::<TrackId>::new()),
        ),
        (locator::RENAME, missing),
    ];
    let error = s.run_batch("Broken", &failing).unwrap_err();
    assert_eq!(error.index, 2, "{error}");
    assert_eq!(
        error.to_string(),
        "command 3 failed: there's no locator 999"
    );
    assert_eq!(masked(project(&s)), masked(&after));
    kept_counters(&after, project(&s), "the broken batch");
    assert_eq!(s.document.editor(), &editor);
    assert_eq!(s.document.take_committed(), []);
    assert_eq!(s.document.steps().len(), 1);
    assert_eq!(s.document.redo_name(), None);
    assert_eq!(s.requests, []);
    let unknown = [(CommandId::from("track.nope"), Args::new())];
    let error = s.run_batch("Nope", &unknown).unwrap_err();
    assert_eq!(error.error, CommandError::Unknown("track.nope".into()));
    // Undo, redo and Go to Step step through the history, so they can't be
    // part of a step in it.
    for example in examples().iter().filter(|e| e.does == Does::Steps) {
        let command = s.registry.get(&example.command).unwrap();
        assert!(command.alone, "{} runs alone", example.command);
    }

    // A batch that changes nothing leaves no step, and what was undone
    // before it can still be redone.
    assert!(s.document.undo());
    s.document.take_committed();
    let select = [(
        edit::SELECT_TRACKS,
        Args::new().with("tracks", Vec::<TrackId>::new()),
    )];
    s.run_batch("Select", &select).unwrap();
    assert_eq!(s.document.undo_name(), None);
    assert_eq!(s.document.redo_name(), Some("Start a Beat"));
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// Runs examples in random orders in one batch, as `run_commands` runs
    /// commands, with a step to redo from before it. Ended, the batch is
    /// one step, which undo takes back whole and redo makes again, or no
    /// step if its changes cancel out. Cancelled, it's as if it never ran.
    #[test]
    fn a_batch_is_one_step_or_nothing(
        runs in prop::collection::vec((0..runnable().len(), 0..32_usize), 1..16),
        cancel in any::<bool>(),
    ) {
        let examples = runnable();
        let mut s = setup();
        let before = Args::new().with("at", Beats::new(64)).with("name", "Before");
        s.run_with(&locator::ADD, before).unwrap();
        s.document.undo();
        s.document.take_committed();
        let redo = s.document.redo_name().map(str::to_owned);
        let start = project(&s).clone();
        let editor = s.document.editor().clone();
        s.document.begin_batch("Batch");
        let mut ran = Vec::new();
        for (i, pick) in runs {
            let example = &examples[i];
            s.pick = pick;
            ran.push(example.command.to_string());
            if let Some(args) = (example.prepare)(&mut s) {
                let _ = s.run_with(&example.command, args);
            }
        }
        let what = format!("{ran:?}");
        if cancel {
            s.document.cancel_batch();
            prop_assert_eq!(s.document.take_committed(), [], "{}", what);
            prop_assert_eq!(masked(project(&s)), masked(&start), "{}", what);
            kept_counters(&start, project(&s), &what);
            prop_assert_eq!(s.document.editor(), &editor, "{}", what);
            prop_assert_eq!(s.document.steps().done(), 0, "{}", what);
            prop_assert_eq!(s.document.redo_name().map(str::to_owned), redo, "{}", what);
            return Ok(());
        }
        s.document.end_batch();
        let committed = s.document.take_committed();
        let end = project(&s).clone();
        saves(&end, &what);
        match s.document.steps().done() {
            0 => {
                prop_assert_eq!(masked(&end), masked(&start), "{}", what);
                prop_assert_eq!(s.document.redo_name().map(str::to_owned), redo, "{}", what);
            }
            1 => {
                prop_assert!(!committed.is_empty());
                prop_assert_eq!(s.document.undo_name(), Some("Batch"), "{}", what);
                prop_assert_eq!(s.document.redo_name(), None, "{}", what);
                s.document.undo();
                prop_assert_eq!(masked(project(&s)), masked(&start), "undoing {}", what);
                kept_counters(&end, project(&s), &what);
                s.document.redo();
                prop_assert_eq!(project(&s), &end, "redoing {}", what);
            }
            done => prop_assert!(false, "{} made {} steps", what, done),
        }
    }
}

/// What the random test does next.
#[derive(Debug, Clone)]
enum Op {
    /// Runs one of the examples that edit or leave the project, by its
    /// place among them, picking what it acts on by the second number.
    Run(usize, usize),
    Undo,
    Redo,
    /// Goes to a step in the history, picked by the number, as clicking it
    /// in the History panel does.
    GoTo(usize),
}

/// The examples [`Op::Run`] picks from.
fn runnable() -> Vec<Example> {
    let mut examples = examples();
    examples.retain(|e| e.does != Does::Steps);
    examples
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..runnable().len(), 0..32_usize).prop_map(|(i, pick)| Op::Run(i, pick)),
        2 => Just(Op::Undo),
        1 => Just(Op::Redo),
        1 => (0..32_usize).prop_map(Op::GoTo),
    ]
}

proptest! {
    // Like the scheduler's random graphs, without a regressions file.
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn undo_and_redo_step_through_what_the_commands_did(
        ops in prop::collection::vec(op(), 1..80),
    ) {
        let examples = runnable();
        let mut s = setup();
        let start = project(&s).clone();
        // The project before any step, then after each, with the step's
        // name: `states[done]` is the project now.
        let mut states = vec![(start.clone(), String::new())];
        let mut done = 0;
        // Follows the committed transactions, as the engine does.
        let mut mirror = Document::new(start.clone());
        let mut ran = Vec::new();
        for op in ops {
            let earlier = project(&s).clone();
            let committed = match op {
                Op::Run(i, pick) => {
                    let example = &examples[i];
                    s.pick = pick;
                    let what = format!("{} (after {ran:?})", example.command);
                    ran.push(example.command.to_string());
                    let Some((result, committed, before)) = run(&mut s, example) else {
                        continue;
                    };
                    match (example.does, &committed[..]) {
                        (_, []) => {
                            prop_assert_eq!(project(&s), &before, "{} changed nothing", what);
                        }
                        (Does::Edits, [step]) => {
                            prop_assert!(result.is_ok(), "{} failed, as a step: {:?}", what, result);
                            saves(project(&s), &what);
                            states.truncate(done + 1);
                            states.push((project(&s).clone(), step.name.clone()));
                            done += 1;
                        }
                        _ => prop_assert!(false, "{} committed {:?}", what, names(&committed)),
                    }
                    committed
                }
                Op::Undo => {
                    ran.push("undo".to_owned());
                    s.run_with(&edit::UNDO, Args::new()).unwrap();
                    let committed = s.document.take_committed();
                    if done > 0 {
                        let name = format!("Undo {}", states[done].1);
                        prop_assert_eq!(names(&committed), [name.as_str()]);
                        done -= 1;
                        let undo = format!("undo (after {ran:?})");
                        prop_assert_eq!(masked(project(&s)), masked(&states[done].0), "{}", undo);
                    } else {
                        prop_assert!(committed.is_empty());
                    }
                    committed
                }
                Op::Redo => {
                    ran.push("redo".to_owned());
                    s.run_with(&edit::REDO, Args::new()).unwrap();
                    let committed = s.document.take_committed();
                    if done + 1 < states.len() {
                        done += 1;
                        let name = format!("Redo {}", states[done].1);
                        prop_assert_eq!(names(&committed), [name.as_str()]);
                        let redo = format!("redo (after {ran:?})");
                        prop_assert_eq!(masked(project(&s)), masked(&states[done].0), "{}", redo);
                    } else {
                        prop_assert!(committed.is_empty());
                    }
                    committed
                }
                Op::GoTo(step) => {
                    let step = step % states.len();
                    ran.push(format!("go to {step}"));
                    let args = Args::new().with("step", step as i64);
                    s.run_with(&history::GO_TO, args).unwrap();
                    let committed = s.document.take_committed();
                    // Each step in between, undone or redone in turn.
                    let between: Vec<String> = if step < done {
                        let undone = (step + 1..=done).rev();
                        undone.map(|i| format!("Undo {}", states[i].1)).collect()
                    } else {
                        let redone = done + 1..=step;
                        redone.map(|i| format!("Redo {}", states[i].1)).collect()
                    };
                    prop_assert_eq!(names(&committed), between);
                    done = step;
                    let went = format!("going to {step} (after {ran:?})");
                    prop_assert_eq!(masked(project(&s)), masked(&states[done].0), "{}", went);
                    committed
                }
            };
            let what = format!("{ran:?}");
            let undo_name = (done > 0).then(|| states[done].1.as_str());
            prop_assert_eq!(s.document.undo_name(), undo_name, "{}", what);
            let redo_name = states.get(done + 1).map(|(_, name)| name.as_str());
            prop_assert_eq!(s.document.redo_name(), redo_name, "{}", what);
            // The History panel lists the steps, and where the history is.
            let steps = s.document.steps();
            let listed: Vec<_> = (0..steps.len()).filter_map(|i| steps.name(i)).collect();
            let made: Vec<_> = states[1..].iter().map(|(_, name)| name.as_str()).collect();
            prop_assert_eq!(listed, made, "{}", what);
            prop_assert_eq!(steps.done(), done, "{}", what);
            prop_assert_eq!(s.document.is_modified(), done > 0, "{}", what);
            // Undo and redo lead back to the fixture or to a project a step
            // made, both of which saved, but for the counters: if those are
            // as they must be, the project saves too.
            kept_counters(&earlier, project(&s), &what);
            replay(&mut mirror, &committed, &what);
            prop_assert_eq!(masked(mirror.project()), masked(project(&s)), "{}", what);
        }

        // Undoing everything goes back through each step to the start, and
        // redoing it all comes back to the newest.
        let undo = format!("undoing all of {ran:?}");
        while done > 0 {
            let earlier = project(&s).clone();
            s.run_with(&edit::UNDO, Args::new()).unwrap();
            done -= 1;
            prop_assert_eq!(masked(project(&s)), masked(&states[done].0), "{}", undo);
            kept_counters(&earlier, project(&s), &undo);
        }
        prop_assert_eq!(s.document.undo_name(), None);
        let redo = format!("redoing all of {ran:?}");
        while done + 1 < states.len() {
            let earlier = project(&s).clone();
            s.run_with(&edit::REDO, Args::new()).unwrap();
            done += 1;
            prop_assert_eq!(masked(project(&s)), masked(&states[done].0), "{}", redo);
            kept_counters(&earlier, project(&s), &redo);
        }
        prop_assert_eq!(s.document.redo_name(), None);
    }
}
