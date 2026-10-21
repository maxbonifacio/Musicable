# The app

`musicable` opens a window with the app shell from the Tier 0 plan: just
enough to run the Tier 0 demo. It plays a project through the audio device
with the parallel scheduler, runs the transport, edit and file commands
from keys, menus and the command palette, and opens and saves project files.
Every command's keys can be changed in the app. There's no editing yet
beyond the transport, the loop and the tempo, which undo and redo cover.

## Running it

```sh
cargo run --release -p musicable-app                      # an empty project
cargo run --release -p musicable-app -- path/to/song.musicable
```

On Windows, release builds open no console window, so double-clicking
`musicable.exe` starts the app without a command line. Debug builds keep the
console, and are much slower: the engine checks every allocation, and egui
runs unoptimised.

`musicable keymap [FILE]` still prints the keys, as
[commands.md](commands.md#trying-it-out) describes, and
`musicable null-test [PROJECT]` checks the parallel scheduler against the
reference renderer (see [The null test](#the-null-test)).

## The Tier 0 demo

1. Start the app, and choose **File > New Demo Project**. Or open
   [`crates/io/tests/fixtures/v1/demo.musicable`](../crates/io/tests/fixtures/v1/demo.musicable),
   which is the same project saved: a dozen test tones through their gain
   stages, one of them muted, looping the first four bars at 124 BPM with the
   metronome on.
2. Press Space. It plays, loops and clicks.
3. **Options > Audio Settings** picks the device, shared or exclusive mode,
   and the buffer size. The corner of the menu bar shows the CPU load and
   counts dropouts.
4. **File > Save Project As**, then **File > Open Project**: the project comes
   back as it was. Saving the demo writes the same bytes as the fixture.
5. `musicable null-test` checks what you can't hear: that the parallel
   scheduler plays the demo exactly as the reference renderer does, before
   and after saving.

[`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs) runs
the same demo offline in every CI run, on Windows, macOS and Linux: it opens
the saved demo, plays it past the end of the loop with both renderers, and
saves it and opens it again.

## The null test

`musicable null-test` plays a project offline from the start of the song
with the reference renderer and with the parallel scheduler side by side,
and checks that they play it the same, sample for sample:

```sh
cargo run --release -p musicable-app -- null-test                  # the demo
cargo run --release -p musicable-app -- null-test song.musicable --seconds 60 --buffer 64
```

Each player opens the project in a session and presses Play, as the app
does, then renders one buffer at a time:

| Line | What plays |
|---|---|
| Reference renderer | The oracle: one node at a time, in order, on one thread |
| Parallel, as the app plays | The parallel scheduler with the app's options, which keep blocks with little work on the audio thread |
| Parallel, every block shared | The parallel scheduler sharing every block between all its workers |
| Saved and opened again | The project written as a project file and read back, played as the app plays it |

A line says `identical` or where the output first differed, how fast it
rendered, how many blocks the scheduler shared and with how many workers,
and how many threads ran nodes at once, on average, while blocks were
shared. The test fails if any output differed, or if the project came back
different after saving. `--seconds`, `--rate` and `--buffer` set how long
it plays, the sample rate and the buffer size: 20 seconds at 48 kHz in
128-frame buffers by default. A debug build checks the renders for
allocations too, as the tests do.

On a 4-core cloud VM:

```text
Null test of the demo: 12 tracks, 20 s at 48000 Hz in 128-frame buffers

Reference renderer             peak -2.7 dBFS, 0.23 s, 87x real time
Parallel, as the app plays     identical, 0.23 s, 89x real time; 14 of 7502 blocks shared, 3 workers, 0.8 threads busy
Parallel, every block shared   identical, 0.32 s, 63x real time; 7502 of 7502 blocks shared, 3 workers, 1.1 threads busy
Saved and opened again         identical, 0.24 s, 84x real time; 23 of 7502 blocks shared, 3 workers, 0.8 threads busy

Passed: the parallel scheduler played it exactly as the reference renderer did, before and after saving.
```

The demo's nodes are tiny, so the app's options render most blocks on the
audio thread alone, and sharing every block is slower than not sharing.
That's what `ParallelOptions::inline_below` is for
([engine.md](engine.md#parallel-rendering)).

## The window

- **The menu bar** lists the commands with their keys, under File, Edit,
  Transport, View and Options. Undo and Redo name the change they would
  make, as in "Undo Set Tempo", and are greyed out when there's nothing to
  undo or redo. A command in an area with no menu yet gets a menu named
  after its area, before View, so every command can be reached. On the
  right are the CPU meter and the dropout counter.
- **The transport bar** has go to start, play, stop, loop, the metronome, the
  tempo, the time signature, and the playhead in bars, beats and sixteenths
  and in minutes and seconds. Hovering a button shows its command and keys,
  and hovering Loop shows the loop region.
- **The tempo field** sets the tempo where the playhead is, and moves any
  other tempos in the song by as much, as Tempo Up and Down do. Drag it, or
  click it and type a tempo, which applies on Enter. Each change is a "Set
  Tempo" transaction, and a whole drag is one step in the undo history.
- **The track list** shows each track's kind, test tone, gain and whether
  it's muted.
- **The status bar** shows errors and notices until they're dismissed, the
  audio device's sample rate, buffer, and latency as the device's clock
  measures it (click it for the audio settings), and the project's name,
  with "(not saved)" while it has changes. The window's title marks them
  with a `*`.
- **View > Command Palette** (Ctrl+Shift+P) finds any command by name or
  keys and runs it ([below](#the-command-palette)).
- **Options > Keyboard Shortcuts** (Ctrl+K) lists every command with its
  keys, and changes them ([below](#changing-keys)).

## The command palette

The command palette lists every command as "Area: Title", with its keys.
Typing narrows the list:

- Every word typed has to match. A word matches best at the start of a word
  of the name ("tog", "loop"), then inside one, or as the initials of its
  words ("tl" is Toggle Loop), then in the command's id.
- Keys find their command: "ctrl+l" lists Toggle Loop first, and "ctrl+shift"
  lists every command with a Ctrl+Shift chord while you type the rest.
- The commands run from the palette lately come first among those that
  match as well.

Up and Down choose a command, and Enter or a click runs it and closes the
palette. Escape or a click outside closes it. While it's open, keys type
into it and run no commands.

## Changing keys

Options > Keyboard Shortcuts lists the commands by menu, each with its keys.
The search field at the top finds commands the way the command palette
does, by name or by keys.

- **Click a key** to change it, or **+** to add one. The window waits for
  the keys, and any keys can be pressed, Escape, Enter and Tab included;
  they run nothing while it waits. Then it says what the keys do now:
  - "isn't used yet": **Assign** (or Enter) gives them to the command.
  - "runs *another command* now": **Reassign** (or Enter) takes them from
    that command.
  - "runs *this command* already": there's nothing to do.

  **Try Other Keys** waits for keys again. **Cancel**, Escape, or a click
  anywhere else stops without changing anything.
- **×** next to a key removes it.
- **Reset** shows on a command whose keys aren't the default ones, and gives
  it its default keys back. If another command has one of them now, the
  tooltip says so, and Reset takes it.

Every change is saved to your keymap file at once
([below](#settings-and-your-keymap)), as the changes to the default keymap.
Under the list:

- **Mistakes in your keymap file**, as [commands.md](commands.md#mistakes)
  describes. A chord that two lines claim runs none of their commands, and
  gets a button for each to give it to. Keys for commands this version
  doesn't have can be removed.
- **Import...** uses the keys in another keymap file in place of yours. What
  it gets wrong is left out, and listed.
- **Export...** saves your keys to a keymap file, to share or keep.
- **Reset All** gives every command its default keys again.
- **Reload** reads your keymap file again after you edit it, and **Create
  It** creates it when there's none yet, to edit.
- Escape closes the window, once it isn't waiting for keys.

## Keys

Every key press goes to the keymap before egui sees it, through
`musicable_ui::KeyPresses`. A bound chord runs its command through
`Session::key_pressed` and goes no further, so Space plays even when a button
has keyboard focus. Unbound keys go on to egui. Key presses never reach the
engine any other way.

- While a text field has focus, such as the tempo field being typed into,
  every key goes to it instead. Ctrl+C, Ctrl+X and Ctrl+V reach the keymap
  like other chords, and copy, cut and paste only in text fields.
- Holding a key down runs its command once. The repeats the system sends are
  swallowed.
- While the app asks whether to save changes, keys go to that question.
- While the command palette is open, keys go to it, and run no commands.
- While the keyboard shortcuts window waits for the keys to give a command,
  every key press goes to it, Escape and Enter too, and nothing else sees
  it. `KeyPresses::capture` takes it there instead of `KeyPresses::take`.

### Keyboard layouts

Keymaps name keys after a US keyboard ([commands.md](commands.md#key-chords)).
A key press on another layout is named like this:

1. **The digit row by its place**: the key that types 1 on a US keyboard is
   `1`, even where the layout needs Shift for 1, as AZERTY does. So Ctrl+1
   is the same key on every layout.
2. **Other keys by what they type**, when keymaps have a name for it: Ctrl+Z
   is the key labelled Z, wherever the layout puts it (German, AZERTY), and a
   key that types `-` is `Minus`.
3. **Otherwise by its place on a US keyboard.** That covers letters on
   layouts without Latin letters (Ctrl+F on a Russian keyboard is the key
   where a US keyboard has F), characters a US keyboard has no key for (ö is
   `Semicolon`, ß is `Minus`), and keys pressed with Shift, since Shift
   changes what they type: Shift+1 is `Shift+1`, not `!`.

This is what most apps do with letters, and keeps shortcuts on the digit row
and punctuation where a keymap file puts them. The numeric keypad comes
through as the main keyboard's keys, so the keypad's 5 is `5`, except
NumpadAdd. The other keypad names in keymap files don't match any key press
yet. The Windows key isn't reported to the app, so `Cmd` chords only work on
macOS.

## Files

**File > Open Project**, **Save Project** and **Save Project As** use the
system's file dialogs, starting in the folder of the last project. Saving
adds `.musicable` to a name without it. Project files are read and written
by `musicable_io::project`, which writes through a temporary file, so a
failed save leaves the old file alone. Errors go to the status bar.

New, New Demo, Open and Quit lose the project's unsaved changes, so they ask
first: Save, Don't Save or Cancel. So does closing the window. Saving there
can still be cancelled in the file dialog, which cancels the rest.

The file commands are real commands (`file.new` and so on, see
[commands.md](commands.md#file)), so they can be remapped. A command can't
show a dialog, so it pushes a `Request` into its `Context`, and the app
carries it out after the command has run.

## Settings and your keymap

The app keeps its settings and your keymap in its config folder:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\Musicable` |
| macOS | `~/Library/Application Support/Musicable` |
| Linux | `$XDG_CONFIG_HOME/musicable`, or `~/.config/musicable` |

The `MUSICABLE_CONFIG_DIR` environment variable picks another folder, which
is handy for testing.

`keymap.toml` is your keymap file, as [commands.md](commands.md#the-keymap-file)
describes. The app reads it at startup and when you click Reload in
Options > Keyboard Shortcuts, which shows any mistakes in it. A file that
can't be used leaves the default keymap, and says why. The app saves every
change of keys made in the window to the file at once. Create It writes a
file with no changes in it yet, just the comment that explains it.

Before writing over your keymap file, the app copies it to
`keymap.backup.toml` in the same folder when the write would lose something:

- When the file has mistakes, or can't be read at all, the first change made
  in the window writes a new file, and the old one becomes the backup. The
  window says so.
- Reset All and Import replace all your changes at once, so the file with
  your keys from before becomes the backup. Importing the backup brings them
  back.

Single changes to a file without mistakes make no backup: there's nothing to
lose. Without a config folder, changes last until the app closes. If the
backup or the file can't be written, the window says so, and the keys you
set still work until the app closes.

`settings.toml` holds the audio settings, written whenever you apply new
ones:

```toml
format = "musicable-settings"
version = 1

[audio]
device = "{0.0.0.00000000}.{a1b2c3d4-...}"
exclusive = false
buffer_size = 128
```

Every setting is optional: no device is the default output, no
`sample_rate` is the rate the device runs at, and no `buffer_size` is the
smallest the device supports. Fields this version doesn't know are ignored.

## Audio

`musicable_app::Output` keeps the audio device playing the engine:

- At startup it opens the stream the settings ask for. If the device in the
  settings isn't plugged in, it plays through the default output instead, and
  says so; the settings keep the device for next time.
- The engine is built for the stream, with its sample rate and its period as
  the block size, and always renders with the parallel scheduler
  (`musicable_app::engine`).
- When the stream ends by itself, because the device was unplugged or its
  format changed, the output tries it again at once and then every two
  seconds. While nothing plays, the engine waits, and the commands sent to it
  wait in its queue and the session's.
- **Apply** in the audio settings stops the stream and opens the new one. If
  it needs another sample rate or block size, a new engine takes over:
  `Session::replace_engine` sets it up with the project's graph, tempo map
  and loop, at the same place in bars and beats, playing if the old one was.
  If the new stream doesn't open, the old one comes back, and the error shows
  in the window.

The audio settings window lists the devices (Refresh lists them again after
plugging one in), offers exclusive mode where the device does, the device's
rates in exclusive mode, and buffer sizes from the smallest the device
supports. Drivers round the buffer size, and some ignore it, so "Now playing"
says what the stream got.

### Meters

- **CPU**: the share of the time the audio thread spent in its wake-ups,
  rendering included, over the last quarter of a second. Near 100%, wake-ups
  start running late and dropouts follow. It comes from
  `StreamStats::busy`. The tooltip adds the longest wake-up, and how many
  threads the parallel scheduler kept busy at once while it shared blocks.
- **Dropouts**: `StreamStats::underruns` since the stream started. Click to
  count from zero again. The tooltip has the audio lost, how many of the
  dropouts were in the system's audio engine or driver, and the late
  wake-ups.

## How it's built

- `crates/ui` draws the shell with egui. It depends on the model and the
  commands only. The app hands it a `View` of what to show each frame, and
  carries out the `Action`s it hands back. Its tests drive the shell with
  `egui_kittest`.
- `crates/app` has the eframe app (`App`), which holds the `Session`, the
  `Output`, the audio settings window and the user's keymap file
  (`UserKeymap`), and the binary. Debug builds install the engine's
  allocation checker, as the tests do.
- [`crates/app/src/app/remapping.rs`](../crates/app/src/app/remapping.rs)
  drives the whole window with `egui_kittest`, handing it key presses as
  eframe does, without an audio device. It gives every command new keys in
  the keyboard shortcuts window, finds every command in the command palette
  with them, and starts the app again to check they were saved.
- [`crates/app/tests/app_shell.rs`](../crates/app/tests/app_shell.rs) checks
  what the shell asks of the session: file requests, opening projects, the
  tempo field and undoing a drag of it, and a new engine taking over. The
  demo project there must equal the fixture.
- Graphs compile on their own thread
  ([commands.md](commands.md#running-commands-in-the-app)). While one
  compiles, the window looks for it every 5 ms, to send it to the engine
  soon after it's ready.
  [`crates/app/tests/slow_graphs.rs`](../crates/app/tests/slow_graphs.rs)
  checks that a node that's slow to prepare doesn't hold up the
  session, and that what you do meanwhile reaches the engine with the
  graph.
- `musicable_app::null_test` plays projects offline for the null test and
  for [`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs).

To see the window without Windows, a Linux build runs under Xvfb, and the
Windows build runs under Wine with `WGPU_BACKEND=vulkan` (Wine's Direct3D 12
can't compile wgpu's shaders). Wine's WASAPI plays into PulseAudio.

## Not done yet

- Editing tracks.
- Keymaps in the style of other DAWs (Logic, Cubase, Pro Tools). Each would
  be a keymap file to import.
- A macOS default keymap with Cmd.
- Following the default output when it changes.
- An icon, and opening `.musicable` files from the file manager.
