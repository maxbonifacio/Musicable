//! The transport commands: play, stop, record, capture, the start marker,
//! loop, punch in and out, metronome and tempo.
//!
//! Play, stop, record and locating drive the engine's transport. Capture
//! asks the app, which holds what the inputs played. The loop,
//! punching in and out, the metronome and the tempo are part of the project,
//! so those commands change it through transactions, and the app passes the
//! changes on to the engine. The start marker is the arrangement's, in the
//! [`Editor`](crate::Editor).

use musicable_model::time::{BeatRange, Beats};

use crate::editor::PlayFrom;
use crate::{
    Command, CommandError, CommandId, Context, Document, Param, Registry, Request, Type, Value,
};

/// Plays from the start marker when stopped, and stops when playing. The
/// behaviour settings can have it play from the playhead instead.
pub const PLAY_STOP: CommandId = CommandId::new("transport.play_stop");
/// Plays from the playhead: where playing last stopped, or where the start
/// marker was last put.
pub const PLAY: CommandId = CommandId::new("transport.play");
/// Stops, leaving the playhead where it is.
pub const STOP: CommandId = CommandId::new("transport.stop");
/// Records the armed tracks, playing from the start marker as
/// [`PLAY_STOP`] does, or while playing, turns recording on or off: a punch
/// in or out by hand.
pub const RECORD: CommandId = CommandId::new("transport.record");
/// Makes what the armed tracks, and those monitoring In, played over the
/// transport's last run into takes, as if they had recorded it: see
/// [`Request::Capture`].
pub const CAPTURE: CommandId = CommandId::new("transport.capture");
/// Moves the start marker and the playhead to the start of the song.
pub const GO_TO_START: CommandId = CommandId::new("transport.go_to_start");
/// Moves the start marker and the playhead to the end of the song: the end
/// of its last clip, locator, automation or loop.
pub const GO_TO_END: CommandId = CommandId::new("transport.go_to_end");
/// Loops the selection, or else turns looping over the loop region on or
/// off.
pub const TOGGLE_LOOP: CommandId = CommandId::new("transport.toggle_loop");
/// Turns the metronome on or off.
pub const TOGGLE_METRONOME: CommandId = CommandId::new("transport.toggle_metronome");
/// Turns punching in at the loop region's start on or off.
pub const TOGGLE_PUNCH_IN: CommandId = CommandId::new("transport.toggle_punch_in");
/// Turns punching out at the loop region's end on or off.
pub const TOGGLE_PUNCH_OUT: CommandId = CommandId::new("transport.toggle_punch_out");
/// Raises every tempo in the song by 1 BPM.
pub const TEMPO_UP: CommandId = CommandId::new("transport.tempo_up");
/// Lowers every tempo in the song by 1 BPM.
pub const TEMPO_DOWN: CommandId = CommandId::new("transport.tempo_down");
/// Moves the start marker and the playhead to a position: playing goes on
/// from there.
pub const LOCATE: CommandId = CommandId::new("transport.locate");
/// Moves the start marker, and the playhead with it while stopped.
pub const SET_START_MARKER: CommandId = CommandId::new("transport.set_start_marker");
/// Sets the loop region.
pub const SET_LOOP: CommandId = CommandId::new("transport.set_loop");
/// Sets the tempo at a position, moving every other tempo by as much.
pub const SET_TEMPO: CommandId = CommandId::new("transport.set_tempo");
/// Counts the audio device's dropouts from zero again, as clicking the
/// transport bar's Dropouts does.
pub const RESET_DROPOUTS: CommandId = CommandId::new("transport.reset_dropouts");

type Handler = fn(&mut Context<'_>);

/// The transport commands that take no arguments, with their titles and
/// what they run.
const COMMANDS: [(CommandId, &str, Handler); 11] = [
    (PLAY_STOP, "Play/Stop", play_stop),
    (PLAY, "Play", |ctx| {
        start(ctx);
        ctx.transport.play();
    }),
    (STOP, "Stop", |ctx| ctx.transport.stop()),
    (RECORD, "Record", record),
    (CAPTURE, "Capture", |ctx| {
        ctx.requests.push(Request::Capture)
    }),
    (GO_TO_START, "Go to Start", |ctx| go_to(ctx, Beats::ZERO)),
    (GO_TO_END, "Go to End", |ctx| {
        go_to(ctx, crate::navigation::song_end(ctx.document.project()))
    }),
    (TOGGLE_LOOP, "Toggle Loop", toggle_loop),
    (TOGGLE_METRONOME, "Toggle Metronome", toggle_metronome),
    (TEMPO_UP, "Tempo Up", |ctx| {
        change_tempo(ctx, "Tempo Up", 1.0)
    }),
    (TEMPO_DOWN, "Tempo Down", |ctx| {
        change_tempo(ctx, "Tempo Down", -1.0)
    }),
];

/// The transport bar's buttons with no keys by default: punch in and out,
/// and the Dropouts count, which a click counts from zero again.
const BUTTONS: [(CommandId, &str, Handler); 3] = [
    (TOGGLE_PUNCH_IN, "Toggle Punch In", |ctx| {
        ctx.document.transact("Toggle Punch In", |tx| {
            tx.set_punch_in(!tx.project().punch_in);
        });
    }),
    (TOGGLE_PUNCH_OUT, "Toggle Punch Out", |ctx| {
        ctx.document.transact("Toggle Punch Out", |tx| {
            tx.set_punch_out(!tx.project().punch_out);
        });
    }),
    (RESET_DROPOUTS, "Reset Dropouts", |ctx| {
        ctx.requests.push(Request::ResetDropouts)
    }),
];

/// Registers the transport commands.
pub fn register(registry: &mut Registry) {
    for (id, title, run) in COMMANDS.into_iter().chain(BUTTONS) {
        registry.register(id, title, run);
    }
    const AT: Param = Param::required("at", Type::Beats, "The position.");
    registry.add(
        Command::new(LOCATE, "Locate").with_params(&[AT]),
        |ctx, args| {
            jump(ctx, args.get("at")?);
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_START_MARKER, "Set Start Marker").with_params(&[AT]),
        |ctx, args| {
            let at = Beats::max(args.get("at")?, Beats::ZERO);
            ctx.document.editor_mut().start_marker = at;
            if !ctx.transport.is_playing() {
                ctx.transport.locate(at);
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_LOOP, "Set Loop").with_params(&[
            Param::required("start", Type::Beats, "Where the loop starts."),
            Param::required("end", Type::Beats, "Where it ends."),
            Param::optional("looping", Type::Bool, "Turns looping on or off too."),
        ]),
        |ctx, args| {
            let (a, b): (Beats, Beats) = (args.get("start")?, args.get("end")?);
            let (start, end) = (a.min(b).max(Beats::ZERO), a.max(b).max(Beats::ZERO));
            if end == start {
                return Err(CommandError::invalid("a loop must end after it starts"));
            }
            let looping = args.opt("looping")?;
            ctx.document.transact("Set Loop", |tx| {
                tx.set_loop_region(BeatRange::new(start, end));
                if let Some(looping) = looping {
                    tx.set_looping(looping);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_TEMPO, "Set Tempo").with_params(&[
            Param::required("bpm", Type::Number, "The tempo, in beats per minute."),
            Param::optional(
                "at",
                Type::Beats,
                "Where the tempo to set is: the playhead without it.",
            ),
        ]),
        |ctx, args| {
            let bpm = args.get("bpm")?;
            let at = match args.opt("at")? {
                Some(at) => at,
                None => ctx.transport.playhead(),
            };
            set_tempo(ctx.document, at, bpm);
            Ok(Value::None)
        },
    );
}

/// Moves the start marker and the playhead to `at`, or the song's start if
/// it's before it. Playing goes on from there.
pub fn jump(ctx: &mut Context<'_>, at: Beats) {
    let at = at.max(Beats::ZERO);
    ctx.document.editor_mut().start_marker = at;
    ctx.transport.locate(at);
}

/// Moves the start marker and the playhead to `at`, scrolling the
/// arrangement there while stopped, if it's out of sight. While the song
/// plays, Follow shows it.
fn go_to(ctx: &mut Context<'_>, at: Beats) {
    jump(ctx, at);
    if ctx.transport.is_playing() {
        return;
    }
    let editor = ctx.document.editor_mut();
    if !editor.viewport.visible().contains(at) {
        let panel = crate::navigation::Panel::Arrangement;
        editor.navigate(panel, crate::navigation::Record::Step, false, |editor| {
            editor.viewport.follow(at);
        });
    }
}

/// Lets Follow follow the playhead again everywhere, as the song starts
/// playing.
fn start(ctx: &mut Context<'_>) {
    if !ctx.transport.is_playing() {
        ctx.document.editor_mut().navigation.resume_all();
    }
}

fn play_stop(ctx: &mut Context<'_>) {
    if ctx.transport.is_playing() {
        ctx.transport.stop();
        return;
    }
    play(ctx);
}

/// Plays from the start marker, or from the playhead if the behaviour
/// settings say so.
fn play(ctx: &mut Context<'_>) {
    let editor = ctx.document.editor();
    if editor.behaviour.play_from == PlayFrom::StartMarker {
        ctx.transport.locate(editor.start_marker);
    }
    start(ctx);
    ctx.transport.play();
}

/// Starts recording and playing, as Live's arrangement record button does
/// with "Start Playback with Record" on, or while playing, turns recording
/// on or off.
fn record(ctx: &mut Context<'_>) {
    if ctx.transport.is_playing() {
        let on = !ctx.transport.is_recording();
        ctx.transport.set_recording(on);
        return;
    }
    ctx.transport.set_recording(true);
    play(ctx);
}

/// Loops the selection and turns looping on, as Live's Ctrl+L does, or
/// turns looping on or off when the selection is looped already or there
/// isn't one.
fn toggle_loop(ctx: &mut Context<'_>) {
    let project = ctx.document.project();
    let selected = ctx.document.editor().selected_range(project);
    match selected {
        Some(range) if range != project.loop_region => {
            ctx.document.transact("Loop Selection", |tx| {
                tx.set_loop_region(range);
                tx.set_looping(true);
            });
        }
        _ => ctx
            .document
            .transact("Toggle Loop", |tx| tx.set_looping(!tx.project().looping)),
    }
}

fn toggle_metronome(ctx: &mut Context<'_>) {
    ctx.document.transact("Toggle Metronome", |tx| {
        tx.set_metronome(!tx.project().metronome);
    });
}

/// Moves every tempo in the song by `bpm`, within the tempo map's limits.
fn change_tempo(ctx: &mut Context<'_>, name: &str, bpm: f64) {
    ctx.document.transact(name, |tx| {
        let mut tempo = tx.project().tempo.clone();
        for change in tx.project().tempo.tempos() {
            tempo.insert_tempo(change.at, change.bpm + bpm);
        }
        tx.set_tempo(tempo);
    });
}

/// Sets the tempo at `at` to `bpm`, and moves every other tempo in the song
/// by as much, as Tempo Up and Down do. Each stays within the tempo map's
/// limits. This is what [`SET_TEMPO`] does, for the tempo field.
///
/// # Panics
///
/// If `bpm` is NaN.
pub fn set_tempo(document: &mut Document, at: Beats, bpm: f64) {
    document.transact("Set Tempo", |tx| {
        let changes = tx.project().tempo.tempos();
        // The change that sets the tempo at `at`. It gets exactly `bpm`, and
        // the others move by the difference.
        let current = changes.partition_point(|c| c.at <= at).saturating_sub(1);
        let by = bpm - changes[current].bpm;
        let mut tempo = tx.project().tempo.clone();
        for (i, change) in changes.iter().enumerate() {
            let bpm = if i == current { bpm } else { change.bpm + by };
            tempo.insert_tempo(change.at, bpm);
        }
        tx.set_tempo(tempo);
    });
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::{ClipContent, MidiClip};
    use musicable_model::time::TempoMap;
    use musicable_model::{Project, TrackKind};

    use super::*;
    use crate::testing::Setup;
    use crate::{Args, Edit, Key, KeyChord, Keymap, Modifiers};

    #[test]
    fn every_transport_command_is_bound_by_default() {
        let mut registry = Registry::new();
        crate::register_all(&mut registry);
        let keymap = Keymap::default_keymap();
        for (id, title, _) in COMMANDS {
            assert_eq!(registry.get(&id).unwrap().title, title);
            assert!(keymap.chords_for(&id).next().is_some(), "{id} has no chord");
        }
        // The buttons are for the mouse, and the rest for the mouse,
        // the tempo field and scripts.
        for (id, title, _) in BUTTONS {
            assert_eq!(registry.get(&id).unwrap().title, title);
            assert_eq!(keymap.chords_for(&id).next(), None, "{id}");
        }
        for id in [LOCATE, SET_START_MARKER, SET_LOOP, SET_TEMPO] {
            assert!(registry.get(&id).unwrap().needs_arguments(), "{id}");
        }
        assert_eq!(keymap.dangling(&registry).count(), 0);
    }

    #[test]
    fn space_plays_from_the_start_marker_and_stops() {
        let keymap = Keymap::default_keymap();
        assert_eq!(keymap.lookup(Key::Space.into()), Some(&PLAY_STOP));
        assert_eq!(
            keymap.lookup(KeyChord::new(Key::Space, Modifiers::SHIFT)),
            Some(&PLAY)
        );

        let mut setup = Setup::new(Project::new());
        setup.document.editor_mut().start_marker = Beats::new(8);
        setup.run(&PLAY_STOP);
        setup.run(&PLAY_STOP);
        setup.run(&PLAY);
        setup.run(&STOP);
        setup.run(&GO_TO_START);
        assert_eq!(
            setup.transport.calls,
            ["locate 8", "play", "stop", "play", "stop", "locate 0"]
        );
        assert_eq!(setup.document.editor().start_marker, Beats::ZERO);
        // The transport isn't part of the project.
        assert_eq!(setup.document.take_committed(), []);

        // As a setting, Space plays on from where playing stopped.
        setup.transport.calls.clear();
        setup.document.editor_mut().behaviour.play_from = PlayFrom::Playhead;
        setup.run(&PLAY_STOP);
        assert_eq!(setup.transport.calls, ["play"]);
    }

    #[test]
    fn f9_records_from_the_start_marker_and_punches_while_playing() {
        let keymap = Keymap::default_keymap();
        assert_eq!(keymap.lookup(Key::F9.into()), Some(&RECORD));

        let mut setup = Setup::new(Project::new());
        setup.document.editor_mut().start_marker = Beats::new(4);
        setup.run(&RECORD);
        assert!(setup.transport.playing && setup.transport.recording);
        // Punched out and in again by hand, and stopping stops recording.
        setup.run(&RECORD);
        setup.run(&RECORD);
        setup.run(&STOP);
        assert!(!setup.transport.recording);
        assert_eq!(
            setup.transport.calls,
            [
                "record true",
                "locate 4",
                "play",
                "record false",
                "record true",
                "stop"
            ]
        );
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn punching_in_and_out_toggle_through_transactions() {
        let mut setup = Setup::new(Project::new());
        setup.run(&TOGGLE_PUNCH_IN);
        setup.run(&TOGGLE_PUNCH_OUT);
        setup.run(&TOGGLE_PUNCH_IN);
        let project = setup.document.project();
        assert!(!project.punch_in && project.punch_out);
        assert_eq!(
            setup.committed_names(),
            ["Toggle Punch In", "Toggle Punch Out", "Toggle Punch In"]
        );
        assert!(setup.document.undo());
        assert!(setup.document.project().punch_in);
    }

    #[test]
    fn the_start_marker_takes_the_playhead_along_while_stopped() {
        let mut setup = Setup::new(Project::new());
        let at = |beats: i64| Args::new().with("at", Beats::new(beats));
        setup.run_with(&SET_START_MARKER, at(4)).unwrap();
        setup.transport.playing = true;
        setup.run_with(&SET_START_MARKER, at(-2)).unwrap();
        assert_eq!(setup.document.editor().start_marker, Beats::ZERO);
        // Locating moves both, and playing goes on from there.
        setup.run_with(&LOCATE, at(12)).unwrap();
        assert_eq!(setup.document.editor().start_marker, Beats::new(12));
        assert_eq!(setup.transport.calls, ["locate 4", "locate 12"]);
    }

    #[test]
    fn loop_and_metronome_toggle_through_transactions() {
        let mut setup = Setup::new(Project::new());
        setup.run(&TOGGLE_LOOP);
        setup.run(&TOGGLE_METRONOME);
        assert!(setup.document.project().looping);
        assert!(setup.document.project().metronome);
        setup.run(&TOGGLE_LOOP);
        assert!(!setup.document.project().looping);

        let committed = setup.document.take_committed();
        let names: Vec<_> = committed.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Toggle Loop", "Toggle Metronome", "Toggle Loop"]);
        assert_eq!(
            committed[1].edits,
            [Edit::Metronome {
                from: false,
                to: true,
            }]
        );
        assert_eq!(setup.transport.calls, Vec::<String>::new());
    }

    #[test]
    fn the_loop_key_loops_the_selection() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::new(8), Beats::new(14));
        let clip = project.new_clip("K", range, ClipContent::Midi(MidiClip::default()));
        let id = clip.id;
        project.track_mut(keys).unwrap().clips.push(clip);
        let mut setup = Setup::new(project);
        setup.select_clips(&[id]);
        setup.run(&TOGGLE_LOOP);
        let project = setup.document.project();
        assert_eq!((project.loop_region, project.looping), (range, true));
        // Looped already: the key turns looping off, and on again.
        setup.run(&TOGGLE_LOOP);
        assert!(!setup.document.project().looping);
        setup.select_time(0, 4, &[keys]);
        setup.run(&TOGGLE_LOOP);
        assert_eq!(
            setup.document.project().loop_region,
            BeatRange::new(Beats::ZERO, Beats::new(4))
        );
        assert_eq!(
            setup.committed_names(),
            ["Loop Selection", "Toggle Loop", "Loop Selection"]
        );
        // The loop brace sets the region alone.
        let args = Args::new()
            .with("start", Beats::new(16))
            .with("end", Beats::new(8));
        setup.run_with(&SET_LOOP, args).unwrap();
        let project = setup.document.project();
        assert_eq!(
            project.loop_region,
            BeatRange::new(Beats::new(8), Beats::new(16))
        );
        assert!(project.looping);
        let empty = Args::new()
            .with("start", Beats::new(3))
            .with("end", Beats::new(3));
        assert_eq!(
            setup.run_with(&SET_LOOP, empty),
            Err(CommandError::invalid("a loop must end after it starts"))
        );
    }

    #[test]
    fn tempo_moves_by_one_bpm_within_limits() {
        let mut setup = Setup::new(Project::new());
        setup.run(&TEMPO_UP);
        setup.run(&TEMPO_UP);
        setup.run(&TEMPO_DOWN);
        assert_eq!(setup.document.project().tempo.bpm_at(Beats::ZERO), 121.0);

        let mut project = Project::new();
        project.tempo.set_tempo(TempoMap::MAX_BPM);
        project.tempo.insert_tempo(Beats::new(16), 100.0);
        let mut setup = Setup::new(project);
        setup.run(&TEMPO_UP);
        let tempo = &setup.document.project().tempo;
        assert_eq!(tempo.bpm_at(Beats::ZERO), TempoMap::MAX_BPM);
        assert_eq!(tempo.bpm_at(Beats::new(16)), 101.0);

        // At the limit, nothing changes and nothing is recorded.
        let mut project = Project::new();
        project.tempo.set_tempo(TempoMap::MIN_BPM);
        let mut setup = Setup::new(project);
        setup.run(&TEMPO_DOWN);
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn a_tempo_field_sets_the_tempo_where_the_playhead_is() {
        let mut document = Document::new(Project::new());
        set_tempo(&mut document, Beats::new(3), 97.3);
        assert_eq!(document.project().tempo.bpm_at(Beats::ZERO), 97.3);
        let committed = document.take_committed();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].name, "Set Tempo");

        // The tempo at the playhead gets the value, and the others move with
        // it, within the limits.
        let mut project = Project::new();
        project.tempo.insert_tempo(Beats::new(16), 100.0);
        project.tempo.insert_tempo(Beats::new(32), 30.0);
        let mut document = Document::new(project.clone());
        set_tempo(&mut document, Beats::new(20), 85.5);
        let tempo = &document.project().tempo;
        assert_eq!(tempo.bpm_at(Beats::ZERO), 105.5);
        assert_eq!(tempo.bpm_at(Beats::new(16)), 85.5);
        assert_eq!(tempo.bpm_at(Beats::new(32)), TempoMap::MIN_BPM);

        // The same tempo again changes nothing.
        document.take_committed();
        set_tempo(&mut document, Beats::new(20), 85.5);
        assert_eq!(document.take_committed(), []);

        // The command takes the playhead's position.
        let mut setup = Setup::new(project);
        setup.transport.playhead = Beats::new(20);
        let bpm = Args::new().with("bpm", 85.5);
        setup.run_with(&SET_TEMPO, bpm).unwrap();
        assert_eq!(setup.document.project().tempo.bpm_at(Beats::new(16)), 85.5);
    }
}
