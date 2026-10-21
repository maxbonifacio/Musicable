# Commands and keymaps

Every user action in Musicable is a named command, and key presses reach
commands only through a keymap. Menus, the command palette, scripts and
agents will run the same commands, so remapping a shortcut is just editing
the keymap. This page covers the commands so far, the transactions they
change the project through, how the app runs them, key chords, the keymap
file, and notes for the app shell.

## Commands

A command has a dotted id, `area.action` in lower case, and a title in title
case for menus and the palette. Keymap files name commands by id, so a
command keeps its id once it has shipped. Each area registers its commands
with the `Registry`, as `transport::register` does, and `Registry::run` runs
one by id. A running command gets a `Context`: the open project's `Document`,
and the engine's `Transport`.

### Transport

The default keymap follows Ableton Live's shortcuts where Live has one.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `transport.play_stop` | Play/Stop | Plays from the playhead, or stops if playing | Space | Space |
| `transport.play` | Play | Plays from the playhead | Shift+Space | Shift+Space (continue from the stop point) |
| `transport.stop` | Stop | Stops, leaving the playhead where it is | Ctrl+Period | none |
| `transport.go_to_start` | Go to Start | Moves the playhead to the start of the song | Home | Home |
| `transport.toggle_loop` | Toggle Loop | Turns looping over the loop region on or off | Ctrl+L | Ctrl+L |
| `transport.toggle_metronome` | Toggle Metronome | Turns the metronome on or off | O | O |
| `transport.tempo_up` | Tempo Up | Raises the tempo by 1 BPM | Ctrl+Shift+Up | none |
| `transport.tempo_down` | Tempo Down | Lowers the tempo by 1 BPM | Ctrl+Shift+Down | none |

In Live, Space plays from the start marker. Musicable has no start marker
yet, so for now Space and Shift+Space both play from the playhead. Live has
no keys for stop and tempo; the ones chosen do nothing in Live outside its
MIDI note editor.

## Transactions

Commands change the project only through transactions. The `Document` that
holds the open project hands out `&Project` only, so every change goes
through `Document::transact`:

```rust
ctx.document.transact("Toggle Metronome", |tx| {
    tx.set_metronome(!tx.project().metronome);
});
```

A `Transaction` has a setter for each part of the project that commands
change: so far the tempo map, the loop region, looping and the metronome.
Each setter records an `Edit` with the new value and the one it replaced. A
setter that changes nothing records nothing, and a transaction with nothing
recorded leaves no trace. Any other is committed under its name, which the
undo menu will show, and the document notes that the project has changed
since it was opened or saved.

Whatever keeps itself in step with the project takes the committed
transactions from `Document::take_committed`. The app sends the engine what
they changed (see below). Undo and redo (Tier 1) will keep them to step
through, and ARA change notifications (Tier 2) will go out from the same
place.

Playing, stopping and moving the playhead don't change the project, so
commands send them straight to the `Transport`, and undo will leave them
alone.

A new kind of change gets a setter on `Transaction` and a variant of `Edit`.
`Edit` isn't `#[non_exhaustive]`, on purpose: the app's match on it stops
compiling until it says what the engine should do about the new variant.

## Running commands in the app

`musicable_app::Session` holds the open project's document, the commands, the
keymap and the engine's handle. Opening a project swaps in its graph and
sends the engine its tempo map and loop. Then:

- `Session::key_pressed` looks a chord up in the keymap and runs the command
  bound to it. It returns `false` if no command is bound, so the key can go
  elsewhere.
- `Session::run` runs a command by id, for menus and the command palette.
- After each command, the session sends the engine what the committed
  transactions changed:

  | Edit | Sent to the engine |
  |---|---|
  | Tempo map | `EngineHandle::set_tempo_map`, with the new map |
  | Loop region, looping | `TransportCommand::Loop`, with the loop as it is now |
  | Metronome | The metronome's `Metronome::ENABLED` parameter |

  Play, stop and go to start send their `TransportCommand` as they run.
- `Session::tick` belongs in every UI frame. While the engine's queue is
  full, as when the audio device has stopped, the session holds on to what
  doesn't fit, and `tick` sends it, in order, once there is room. It also
  drops the graphs and tempo maps the engine is done with.

[`crates/app/tests/key_presses.rs`](../crates/app/tests/key_presses.rs)
drives an engine this way from key presses, rendering offline.

## Key chords

A key chord is its modifiers and its key, joined by `+`: `Space`, `Ctrl+L`,
`Ctrl+Shift+Up`. Case doesn't matter and the modifiers can come in any
order. Musicable writes them as Ctrl, Alt, Shift, Cmd.

| Modifier | Also written | Notes |
|---|---|---|
| `Ctrl` | `Control` | |
| `Alt` | `Option` | |
| `Shift` | | |
| `Cmd` | `Command`, `Super`, `Win` | Command on macOS, the Windows key on Windows |

The keys:

- Letters `A` to `Z` and digits `0` to `9`.
- `F1` to `F24`.
- `Space`, `Enter`, `Tab`, `Backspace`, `Escape`, `Insert`, `Delete`, `Home`,
  `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`.
- Punctuation, named after the character the key types without Shift on a US
  layout: `Minus`, `Equals`, `OpenBracket`, `CloseBracket`, `Backslash`,
  `Semicolon`, `Quote`, `Backtick`, `Comma`, `Period`, `Slash`. The character
  itself works too: `Ctrl+.` is `Ctrl+Period`. The `+` key is `Equals` (or
  `NumpadAdd`), since `+` joins the parts of a chord.
- The numeric keypad: `Numpad0` to `Numpad9`, `NumpadAdd`, `NumpadSubtract`,
  `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter`.
- Other spellings: `Esc`, `Return`, `Del`, `PgUp`, `PgDn`, and `ArrowUp`,
  `ArrowDown`, `ArrowLeft`, `ArrowRight`.

How other keyboard layouts map onto these keys is decided with the app shell.
The default keymap uses Ctrl for now; a macOS default with Cmd comes with
macOS support.

## The keymap file

The default keymap is
[`crates/commands/keymaps/default.toml`](../crates/commands/keymaps/default.toml),
built into the app. Your keymap file holds only your changes to it:

```toml
format = "musicable-keymap"
version = 1

[bindings]
"transport.toggle_metronome" = ["M", "Ctrl+M"]
"transport.toggle_loop" = []
```

- Each line gives a command the chords that run it, in place of its default
  ones. A single chord can be written without the list: `"transport.stop" =
  "Escape"`.
- An empty list, `[]`, unbinds the command.
- Commands that aren't listed keep their default chords.
- A chord runs one command. Giving a chord to one command takes it from the
  command that had it.
- Bare dotted keys work too: `transport.play = ["P"]` is the same as
  `"transport.play" = ["P"]`.
- `format` and `version` are optional when you write the file by hand.
  Musicable writes both, like the header of a project file.

When Musicable saves your keymap, it writes only the commands whose chords
differ from the default, so new default shortcuts in later versions still
reach you.

### Mistakes

A line with a mistake in it, such as a misspelt key, is skipped and the
command keeps its default chords. A chord that two lines claim runs neither
command. Musicable reports both and loads the rest of the file. A binding for
a command that doesn't exist is kept, in case a later version or a plugin
adds it, and reported too.

A file that isn't valid TOML, has an unknown field or comes from a newer
version is ignored as a whole: Musicable uses the default keymap and shows
the error.

### Trying it out

Until the app shell reads your keymap file, this prints the bindings it
gives, along with any mistakes in it:

```sh
cargo run -p musicable-app -- keymap path/to/keymap.toml
```

Leave out the path to see the default keymap.

## Notes for the app shell (M7)

- Turn each key press into a `KeyChord` and hand it to
  `Session::key_pressed`. Key presses never go straight to the engine.
- Give text fields key presses before the keymap, so typing a space in a
  track name doesn't start playback.
- Menus and the palette list `Session::commands`, show each command's chords
  from `Session::keymap` with `Keymap::chords_for`, and run commands with
  `Session::run`.
- Call `Session::tick` once per frame, and show the playhead from
  `Session::stats`.
- Loading: start from `Keymap::default_keymap()`, then `KeymapFile::read` the
  user's file and `apply_to` the keymap, and hand it to `Session::new`. Show
  the problems. A missing file means no changes. On an error, keep the
  default keymap and show it.
- Saving after a remap:
  `KeymapFile::diff(&Keymap::default_keymap(), &keymap).write(path)`.
- `Keymap::bind` returns the command a chord had, for an "already used by"
  warning.
- On Windows the file could live at `%APPDATA%\Musicable\keymap.toml`.
