# The app

`musicable` opens a window with the app shell from the Tier 0 plan: just
enough to run the Tier 0 demo. It plays a project through the audio device
with the parallel scheduler, runs the transport, edit and file commands
from keys and menus, and opens and saves project files. There's no editing
yet beyond the transport, the loop and the tempo, which undo and redo
cover.

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

- **The menu bar** lists the commands with their keys, under File, Edit and
  Transport, and Options. Undo and Redo name the change they would make, as
  in "Undo Set Tempo", and are greyed out when there's nothing to undo or
  redo. A command in an area with no menu yet gets a menu named after its
  area, so every command can be reached. On the right are the CPU meter and
  the dropout counter.
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
- **Options > Keyboard Shortcuts** lists every command and its keys, says
  where your keymap file goes, and creates or reloads it.

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
can't be used leaves the default keymap, and says why. Create It writes a
file with no changes in it yet, just the comment that explains it.

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
  `Output` and the audio settings window, and the binary. Debug builds
  install the engine's allocation checker, as the tests do.
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
- Changing keys from the app. For now, edit the keymap file and reload it.
- The command palette.
- A macOS default keymap with Cmd.
- Following the default output when it changes.
- An icon, and opening `.musicable` files from the file manager.
