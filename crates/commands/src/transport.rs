//! The transport commands: play, stop, loop, metronome and tempo.
//!
//! Play, stop and go to start drive the engine's transport. The loop, the
//! metronome and the tempo are part of the project, so those commands change
//! it through transactions, and the app passes the changes on to the engine.

use musicable_model::time::Beats;

use crate::{CommandId, Context, Document, Registry};

/// Plays from the playhead when stopped, and stops when playing.
pub const PLAY_STOP: CommandId = CommandId::new("transport.play_stop");
/// Plays from the playhead.
pub const PLAY: CommandId = CommandId::new("transport.play");
/// Stops, leaving the playhead where it is.
pub const STOP: CommandId = CommandId::new("transport.stop");
/// Moves the playhead to the start of the song.
pub const GO_TO_START: CommandId = CommandId::new("transport.go_to_start");
/// Turns looping over the loop region on or off.
pub const TOGGLE_LOOP: CommandId = CommandId::new("transport.toggle_loop");
/// Turns the metronome on or off.
pub const TOGGLE_METRONOME: CommandId = CommandId::new("transport.toggle_metronome");
/// Raises every tempo in the song by 1 BPM. Tier 0 songs have one tempo.
pub const TEMPO_UP: CommandId = CommandId::new("transport.tempo_up");
/// Lowers every tempo in the song by 1 BPM.
pub const TEMPO_DOWN: CommandId = CommandId::new("transport.tempo_down");

type Handler = fn(&mut Context<'_>);

/// Every transport command, with its title and what it runs.
const COMMANDS: [(CommandId, &str, Handler); 8] = [
    (PLAY_STOP, "Play/Stop", play_stop),
    (PLAY, "Play", |ctx| ctx.transport.play()),
    (STOP, "Stop", |ctx| ctx.transport.stop()),
    (GO_TO_START, "Go to Start", |ctx| {
        ctx.transport.locate(Beats::ZERO)
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

/// Registers the transport commands.
pub fn register(registry: &mut Registry) {
    for (id, title, run) in COMMANDS {
        registry.register(id, title, run);
    }
}

fn play_stop(ctx: &mut Context<'_>) {
    if ctx.transport.is_playing() {
        ctx.transport.stop();
    } else {
        ctx.transport.play();
    }
}

fn toggle_loop(ctx: &mut Context<'_>) {
    ctx.document
        .transact("Toggle Loop", |tx| tx.set_looping(!tx.project().looping));
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
/// limits. This is the transaction behind a tempo field: commands don't take
/// values yet, so it isn't a command.
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
    use musicable_model::Project;
    use musicable_model::time::TempoMap;

    use super::*;
    use crate::{Edit, Key, KeyChord, Keymap, Modifiers, Transport};

    /// Records what commands ask of the transport.
    #[derive(Debug, Default)]
    struct Recorder {
        playing: bool,
        calls: Vec<String>,
    }

    impl Transport for Recorder {
        fn is_playing(&self) -> bool {
            self.playing
        }

        fn play(&mut self) {
            self.playing = true;
            self.calls.push("play".into());
        }

        fn stop(&mut self) {
            self.playing = false;
            self.calls.push("stop".into());
        }

        fn locate(&mut self, at: Beats) {
            self.calls.push(format!("locate {at:?}"));
        }
    }

    struct Setup {
        registry: Registry,
        document: Document,
        transport: Recorder,
    }

    impl Setup {
        fn new(project: Project) -> Self {
            let mut registry = Registry::new();
            register(&mut registry);
            Self {
                registry,
                document: Document::new(project),
                transport: Recorder::default(),
            }
        }

        fn run(&mut self, id: &CommandId) {
            let mut requests = Vec::new();
            let mut ctx = Context {
                document: &mut self.document,
                transport: &mut self.transport,
                requests: &mut requests,
            };
            self.registry.run(id, &mut ctx).unwrap();
            assert_eq!(requests, [], "transport commands ask nothing of the app");
        }
    }

    #[test]
    fn every_transport_command_is_bound_by_default() {
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.iter().count(), COMMANDS.len());
        crate::file::register(&mut registry);
        crate::options::register(&mut registry);
        crate::view::register(&mut registry);

        let keymap = Keymap::default_keymap();
        for (id, title, _) in COMMANDS {
            assert_eq!(registry.get(&id).unwrap().title, title);
            assert!(keymap.chords_for(&id).next().is_some(), "{id} has no chord");
        }
        assert_eq!(keymap.dangling(&registry).count(), 0);
    }

    #[test]
    fn space_plays_and_stops() {
        let keymap = Keymap::default_keymap();
        assert_eq!(keymap.lookup(Key::Space.into()), Some(&PLAY_STOP));
        assert_eq!(
            keymap.lookup(KeyChord::new(Key::Space, Modifiers::SHIFT)),
            Some(&PLAY)
        );

        let mut setup = Setup::new(Project::new());
        setup.run(&PLAY_STOP);
        setup.run(&PLAY_STOP);
        setup.run(&PLAY);
        setup.run(&STOP);
        setup.run(&GO_TO_START);
        assert_eq!(
            setup.transport.calls,
            ["play", "stop", "play", "stop", "locate Beats(0)"]
        );
        // The transport isn't part of the project.
        assert_eq!(setup.document.take_committed(), []);
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
        setup.document.take_committed();
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
        let mut document = Document::new(project);
        set_tempo(&mut document, Beats::new(20), 85.5);
        let tempo = &document.project().tempo;
        assert_eq!(tempo.bpm_at(Beats::ZERO), 105.5);
        assert_eq!(tempo.bpm_at(Beats::new(16)), 85.5);
        assert_eq!(tempo.bpm_at(Beats::new(32)), TempoMap::MIN_BPM);

        // The same tempo again changes nothing.
        document.take_committed();
        set_tempo(&mut document, Beats::new(20), 85.5);
        assert_eq!(document.take_committed(), []);
    }
}
