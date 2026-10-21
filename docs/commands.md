# Commands and keymaps

Every user action in Musicable is a named command, and key presses reach
commands only through a keymap. Menus, the command palette, scripts and
agents will run the same commands, so remapping a shortcut is just editing
the keymap. This page covers the commands so far, the transactions they
change the project through, undo and redo, how the app runs them, key
chords and the keymap file. How the app's window hands key presses to the
keymap is in [app.md](app.md#keys).

## Commands

A command has a dotted id, `area.action` in lower case, and a title in title
case for menus and the palette. Keymap files name commands by id, so a
command keeps its id once it has shipped. Each area registers its commands
with the `Registry`, as `transport::register` does, and `Registry::run` runs
one by id. A running command gets a `Context`: the open project's `Document`,
the engine's `Transport`, and a list of `Request`s for what only the app can
do, such as showing a file dialog.

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

### File

A file command can't show a dialog or close the window itself, so it pushes
a `Request` into its `Context`, and the app carries it out once the command
has run ([app.md](app.md#files)).

| Command | Title | What it does | Default key |
|---|---|---|---|
| `file.new` | New Project | Starts a new, empty project | Ctrl+N |
| `file.new_demo` | New Demo Project | Starts the Tier 0 demo project | none |
| `file.open` | Open Project... | Asks for a project file and opens it | Ctrl+O |
| `file.save` | Save Project | Saves the project, asking where if it has no file yet | Ctrl+S |
| `file.save_as` | Save Project As... | Asks where to save the project, and saves it there | Ctrl+Shift+S |
| `file.quit` | Quit | Closes Musicable | Ctrl+Q |

New, New Demo, Open and Quit ask about unsaved changes first.

### Edit

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `edit.undo` | Undo | Takes back the last change to the project | Ctrl+Z | Ctrl+Z |
| `edit.redo` | Redo | Makes the last change undo took back again | Ctrl+Y, Ctrl+Shift+Z | Ctrl+Y |

The Edit menu names the change, as in "Undo Tempo Up". Ctrl+Shift+Z is
the other common key for redo.

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
Edit menu shows, and becomes a step in the undo history (see below).

Whatever keeps itself in step with the project takes the committed
transactions from `Document::take_committed`. The app sends the engine what
they changed (see below), and ARA change notifications (Tier 2) will go out
from the same place.

Playing, stopping and moving the playhead don't change the project, so
commands send them straight to the `Transport`, and undo leaves them alone.

A new kind of change gets a setter on `Transaction` and a variant of `Edit`.
`Edit` isn't `#[non_exhaustive]`, on purpose: the app's match on it stops
compiling until it says what the engine should do about the new variant,
and the matches in `document.rs` until undo knows how to take it back (see
below).

## Undo and redo

The `Document` keeps each committed transaction as a step in its undo
history, up to 1,000 of them. `Document::undo` takes the last step back: it
applies the inverse of each of its edits, last edit first, as a new
transaction named after the step, such as "Undo Tempo Up".
`Document::redo` makes it again, as "Redo Tempo Up". Since they're
transactions like any other, the engine follows them the same way. A new
transaction forgets what was undone, so there's nothing left to redo.

- `Document::undo_name` and `redo_name` say what Undo and Redo would
  change, for the Edit menu.
- **Gestures.** Dragging the tempo field makes a transaction on every frame
  it moves. Between `Document::begin_gesture` and `end_gesture`,
  transactions with the same name join one step, so one undo takes the whole
  drag back. A gesture that ends where it started leaves no step. Undo and
  redo end a gesture.
- **Unsaved changes.** `Document::is_modified` follows the history: undo
  back to where the project was saved, and it's unchanged again.
- Opening a project starts a new history.

A new kind of edit needs an arm in each match in
[`crates/commands/src/document.rs`](../crates/commands/src/document.rs):
`Transaction::apply` makes it, `Edit::inverse` swaps its old and new values,
`Edit::absorb` merges two in a row for gestures, and `Edit::is_noop` says
when merged ones cancel out. Add an example to `one_of_each` in
`history.rs`, and the tests undo and redo it with every other kind.

## Running commands in the app

`musicable_app::Session` holds the open project's document, the commands, the
keymap, the engine's handle and a `Compiler`. Opening a project, with
`Session::new` or `Session::open`, compiles its graph on the compiler's
thread and sends the engine its tempo map and loop; `Session::open` also
stops the transport at the start of the song. Then:

- `Session::key_pressed` looks a chord up in the keymap and runs the command
  bound to it. It returns `false` if no command is bound, so the key can go
  elsewhere.
- `Session::run` runs a command by id, for menus and the command palette.
- After each command, the session sends the engine what the committed
  transactions changed:

  | Edit | Sent to the engine |
  |---|---|
  | Tempo map | `EngineHandle::set_tempo_map`, with the last new map |
  | Loop region, looping | `TransportCommand::Loop`, with the loop as it is now |
  | Metronome | The metronome's `Metronome::ENABLED` parameter |

  Play, stop and go to start send their `TransportCommand` as they run.
- `Session::take_requests` hands over what the commands asked of the app,
  oldest first.
- `Session::set_tempo` is for a tempo field: it sets the tempo at the
  playhead, as one transaction, with `transport::set_tempo`.
- `Session::begin_gesture` and `end_gesture` make a drag one step in the
  undo history, and `undo_name` and `redo_name` are for the Edit menu.
- `Session::tick` belongs in every UI frame. While the engine's queue is
  full, as when the audio device has stopped, the session holds on to what
  doesn't fit, and `tick` sends it, in order, once there is room. It also
  sends the graphs the compiler has finished, and drops the graphs and
  tempo maps the engine is done with.
- **Waiting for the graph.** A newly opened project's tempo map and loop,
  and whatever follows them, such as Play, wait for its graph: until it
  comes, the engine plays silence and doesn't start. The project in the
  window changes at once. `Session::compiling` says whether a graph is on
  its way, so the window can tick more often, and `Session::wait_for_graph`
  blocks until it has arrived, for offline playing such as tests and the
  null test. A graph compiled for a project that has since been closed is
  dropped.
- `Session::replace_engine` moves the project to a new engine, such as one
  built for another device's sample rate or buffer size. The new engine
  takes over where the old one was, in bars and beats, playing if the old one
  was, once the project's graph for it has compiled.

[`crates/app/tests/key_presses.rs`](../crates/app/tests/key_presses.rs)
drives an engine this way from key presses, undo and redo included,
rendering offline,
[`crates/app/tests/app_shell.rs`](../crates/app/tests/app_shell.rs) checks
what the app shell asks of the session, and
[`crates/app/tests/slow_graphs.rs`](../crates/app/tests/slow_graphs.rs)
checks that a node that's slow to prepare doesn't hold up the session.

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

How key presses on other keyboard layouts get these names is in
[app.md](app.md#keyboard-layouts). The default keymap uses Ctrl for now; a macOS default with Cmd comes with
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

The app reads your keymap file from its config folder at startup
([app.md](app.md#settings-and-your-keymap)), and Options > Keyboard
Shortcuts shows the keys and any mistakes. Without opening the window, this
prints the bindings a keymap file gives, along with any mistakes in it:

```sh
cargo run -p musicable-app -- keymap path/to/keymap.toml
```

Leave out the path to see the default keymap.

## Notes for remapping keys in the app

The app doesn't change keys yet: for now you edit the keymap file and reload
it. When it does:

- Save a remap with
  `KeymapFile::diff(&Keymap::default_keymap(), &keymap).write(path)`, then
  hand the keymap to `Session::set_keymap`.
- `Keymap::bind` returns the command a chord had, for an "already used by"
  warning.
