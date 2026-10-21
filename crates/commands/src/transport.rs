//! The transport commands: play, stop, loop, metronome and tempo.
//!
//! This registers them with their titles, and the default keymap binds them.
//! Running them, through transactions on the model and messages to the
//! engine, comes with the transaction layer.

use crate::{CommandId, Registry};

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
/// Raises the tempo by 1 BPM.
pub const TEMPO_UP: CommandId = CommandId::new("transport.tempo_up");
/// Lowers the tempo by 1 BPM.
pub const TEMPO_DOWN: CommandId = CommandId::new("transport.tempo_down");

/// Every transport command, with its title.
pub const COMMANDS: [(CommandId, &str); 8] = [
    (PLAY_STOP, "Play/Stop"),
    (PLAY, "Play"),
    (STOP, "Stop"),
    (GO_TO_START, "Go to Start"),
    (TOGGLE_LOOP, "Toggle Loop"),
    (TOGGLE_METRONOME, "Toggle Metronome"),
    (TEMPO_UP, "Tempo Up"),
    (TEMPO_DOWN, "Tempo Down"),
];

/// Registers the transport commands.
pub fn register(registry: &mut Registry) {
    for (id, title) in COMMANDS {
        registry.register(id, title);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Key, KeyChord, Keymap, Modifiers};

    #[test]
    fn every_transport_command_is_bound_by_default() {
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.iter().count(), COMMANDS.len());

        let keymap = Keymap::default_keymap();
        for (id, title) in COMMANDS {
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
    }
}
