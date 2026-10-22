# Architecture

How Musicable fits together at the end of Tier 0, and where Tier 1 work
plugs in. A project model changes only through commands and transactions.
It's compiled into a graph of nodes, which a real-time engine plays through
the audio device, with a parallel scheduler that matches a single-threaded
reference renderer bit for bit. This page is the map. The detail is in
[engine.md](engine.md), [audio.md](audio.md), [commands.md](commands.md),
[project-format.md](project-format.md) and [app.md](app.md).

## Crates

| Crate | Depends on | Holds |
|---|---|---|
| `model` | nothing | The project as plain data: tracks and their routing, the tempo map, the loop, musical time, and the built-in devices' parameter tables |
| `commands` | `model` | The command registry, keymaps, and the `Document` whose transactions change the project, with their undo history |
| `engine` | `model` | Graphs and nodes, the compiler, the engine, the reference and parallel renderers, clip players and streaming, the built-in devices' DSP, the real-time checker |
| `audio` | `engine` | Device output: our own WASAPI backend on Windows, and ASIO with the `asio` feature; cpal elsewhere |
| `plugins` | `engine`, `model` | CLAP and VST3 hosting: plugin instances and the nodes that process them, and scanning in a process of its own. ARA in Tier 2 |
| `io` | `model` | Project files, and audio files: reading WAV, AIFF and FLAC, writing WAV and AIFF. MIDI files later |
| `ui` | `model`, `commands` | What the window shows, drawn with egui: the arrangement, the mixer, menus and dialogs, from a theme and a mouse map; a `View` in, `Action`s out |
| `app` | all of them | The `musicable` binary: the window, the `Session`, the audio `Output`, the settings |

Cargo enforces the directions that matter. The engine knows nothing of
commands or the UI, the UI can't reach the engine, and only `app` sees
everything. A new feature usually touches the model, a command, the
compiler, a node and the UI, in that order.

## Threads

| Thread | What it runs | Priority |
|---|---|---|
| Main | The window and the `Session`: key presses, commands, transactions, sending the engine what changed | Normal |
| Compiler (`musicable-compiler`) | Building and preparing the graphs the session asks for: `compile()`, then `Graph::prepare` | Normal |
| Audio (`musicable-audio` on Windows, cpal's elsewhere) | `Engine::process_interleaved`, once per device period | MMCSS "Pro Audio", critical |
| ASIO driver's (instead, in ASIO streams) | `Engine::process`, once per buffer half. The stream's own thread, `musicable-asio`, makes the driver's other calls | The driver's: real-time, as ASIO asks of drivers |
| Workers (`musicable-worker-1` and up) | Their share of each block the parallel renderer shares | The same, on Windows only |
| Disk (`musicable-disk`) | Reading the audio files that stream, ahead of the clips that play them | Normal |
| Audio files (`musicable-audio-files`) | Reading the headers and the waveforms of the project's audio files, for the arrangement | Normal |
| Recorder (`musicable-recorder`) | Writing what the armed tracks record, from the inputs' buffers, placed by the engine's timeline | Normal |
| Inputs (`musicable-input`, on Windows) | Capturing each WASAPI input, lined up with the output | MMCSS "Pro Audio" |

There is one worker per physical core, minus one for the audio thread,
which works too, and no more than 24 on Windows. While audio plays, the
main thread meets the audio thread and the workers only at lock-free queues
and atomics, and nothing on those threads locks, allocates, frees or does
I/O. The audio thread and the workers run nodes with denormals flushed to
zero, all of them alike ([engine.md](engine.md#denormals)).

The session never builds a graph itself. It hands the project to the
engine crate's `Compiler`, which builds and prepares the graph on its own
thread, so a node that's slow to prepare, as a plugin loading can be, never
holds up the window. `Session::tick` sends the graph on when it's ready.

## From a key press to sound

```text
key press → keymap → command → transaction → Session::sync → EngineHandle
  ⇒ queue ⇒ Engine (audio thread) → renderer (audio thread and workers) → device
```

Pressing Space with the demo open:

1. `musicable_ui::KeyPresses` takes the key press before egui's widgets see
   it and names it as a `KeyChord`, by the rules in
   [app.md](app.md#keyboard-layouts).
2. `Session::key_pressed` looks the chord up in the `Keymap`, which binds
   Space to `transport.play_stop`.
3. `Registry::run` runs the command with a `Context`. Play and stop go
   straight to the `Transport`, which the session implements on top of the
   engine. Changes to the project go through `Document::transact`, which
   records each change as an `Edit` with its old and new values.
4. After the command, `Session::sync` turns what the committed transactions
   changed into messages for the engine: a tempo map, a loop, a parameter
   value. A change to what the graph holds, such as a new track, means a new
   graph: the compiler's thread runs `compile()`, then `Graph::prepare` for
   the engine's sample rate and block size, and once it's done, the session
   sends it with `EngineHandle::swap_graph`. The old graph plays on until
   then.
5. The messages reach the audio thread through a lock-free queue.
   `Engine::process_interleaved` takes them at the start of the next device
   callback. It installs a new graph between blocks, where each node takes
   over the state of the old node with the same key (`Node::adopt`), and it
   applies each event on its exact frame. A block ends early wherever the
   transport changes or the loop jumps back.
6. The renderer runs every node of the graph once per block. The
   `ParallelRenderer` runs each node as soon as its inputs are done, on the
   workers and the audio thread, or on the audio thread alone when a block
   has too little work to share. The master bus's output goes into the
   device's buffer, and the cue bus, which the metronome feeds, is added on
   top.

On the way back, the engine keeps its counters in atomics (`Stats`: the
playhead, whether it plays, late and dropped events), and so does the stream
(`StreamStats`: dropouts, time busy) and the parallel renderer
(`ParallelStats`). Each channel's meter keeps its levels in atomics too
(`Meters`). The window reads them every frame for the transport bar, the
CPU meter and the mixer. Old graphs and tempo maps come back through a second queue,
to be dropped on the main thread when `Session::tick` runs, once per UI
frame.

## Rules that hold it together

Tier 1 has to keep these. The tests check most of them.

- **Every change to the project is a command and a transaction.**
  `Document` hands out `&Project` only. That's what makes every action
  remappable and every change undoable, and gives ARA's change
  notifications one place to start from
  ([commands.md](commands.md#transactions)).
- **Nothing on the audio thread or the workers allocates, frees, locks,
  blocks or does I/O.** Allocate in `Node::prepare`, and in `Node::adopt`
  swap resources with the old node instead of dropping them. Debug builds
  catch allocations, and Clippy bans locks, sleeping and printing in the
  engine ([engine.md](engine.md#real-time-rules)).
- **The parallel renderer matches the reference renderer bit for bit.** A
  node's output can't depend on the thread that runs it: no thread-locals,
  no state shared between nodes outside the graph's connections. Buses sum
  their inputs in connection order. The equivalence tests check thousands
  of random graphs on every CI run, and `musicable null-test` checks any
  project on any machine ([app.md](app.md#the-null-test)). Every thread
  that runs nodes flushes denormals to zero, so they all do the same
  arithmetic.
- **Time comes from the song position, not from counting blocks.** A node
  that depends on time, such as the test tone's phase or the metronome's
  clicks, works it out from the block's song position and the tempo map. So
  loops, locates and any block size give the same samples. Clip players and
  automation should work the same way.
- **What would click when it jumps ramps instead**, with
  `nodes::Smoothed`, as gain stages do. A ramp counts frames from its
  event, never blocks, so it lands on the same samples however the blocks
  fall ([engine.md](engine.md#parameter-smoothing)).
- **Node keys come from model ids** (`compile::keys`), so a node keeps its
  state when the project is recompiled. A new kind of node gets its own role
  in the key.
- **Every parameter change goes through `EngineHandle::send` or
  `send_at`**, even when it's also written to the model, so a value sent
  while a new graph was compiling isn't lost.
- **The mouse runs commands too.** A gesture in the arrangement or the
  mixer runs a command with arguments, not code of its own, and a drag runs
  one when it ends, so it's one step to undo. Context menus list commands
  from the registry. Selections, the zoom and the like live in the
  `Editor`, not in the widgets.
- **A change to what a project file holds is a new format version**, with a
  migration step and fixtures
  ([project-format.md](project-format.md#changing-the-format)).
- **`Edit` isn't `#[non_exhaustive]`, on purpose.** A new kind of edit stops
  the app compiling until `Session::sync` says what the engine should do
  about it, and the commands crate until undo knows how to take it back
  ([commands.md](commands.md#undo-and-redo)).

## Where Tier 1 starts

Tier 1 of the feature list, and where each part should go. None of this is
built yet, except where it says so.

- **Clips.** Built: audio clips of WAV, AIFF and FLAC files, and MIDI
  clips of notes with per-note expression (MPE), on `Track` in format
  version 3. A clip player heads each audio and MIDI track's strip and
  renders from the song position. Short files load whole as the graph
  builds, and longer ones stream from the disk thread through lock-free
  chunks, ahead of the playhead and at the loop start. Files at other
  sample rates are resampled, and clips fade and crossfade. Notes are
  `NodeEventKind::Note`s at sample offsets, and can pass through a chain of
  nodes, such as a MIDI effect, on their way to an instrument
  ([engine.md](engine.md#clips), [app.md](app.md#audio-files)). The
  arrangement view makes and edits them (below); editing notes comes with
  the piano roll.
- **The arrangement view.** Built: tracks with headers, clips with
  waveforms and notes, the ruler with locators, the start marker and the
  loop brace, drawn with egui's painter, only what's in sight, so a hundred
  tracks of clips take 3 to 4 ms a frame ([app.md](app.md#the-arrangement)).
  Its gestures run commands with typed arguments, such as `clip.move`, and
  commands say when they apply (`When`). What it edits besides the project
  (the selection, the start marker, the grid, the zoom, the behaviour
  settings) is the `Editor`, kept with the `Document` outside the widgets,
  where the piano roll, extensions and agents will find it too
  ([commands.md](commands.md#the-editors-state)). Colours and sizes come
  from a `Theme`, and gestures from a `MouseMap`, for Tier 2's profiles.
- **Devices and plugins.** Devices are in the model, with ids for their
  node keys, and `compile()` puts them between a channel's source and its
  pre-fader slot, each with a `Slot::BeforeDevice` in front of it. There
  are plugins, and a test gain and a test instrument. CLAP and VST3
  hosting are built in `crates/plugins` ([plugins.md](plugins.md)): an
  `Instance` lives on the main thread, where both formats want everything
  but processing done, and makes the `Node`s that process it, so every
  graph's node plays the same plugin. The session's `Plugins` holds an
  instance for each plugin device, hands the compiler their nodes, and
  makes transactions of the changes made in their windows, so undo covers
  them. Scanning runs in a process of its own, so a plugin that crashes
  doesn't take the app with it.
  [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html) checks plugin
  code in CI for what our allocation checker can't see.
- **Built-in devices.** Partly built: eight kinds, each with a parameter
  table in `musicable_model::native` that the format, the commands and the
  devices panel's controls all work from, in format version 7. The engine's
  `devices` module has the nodes, with each parameter worked out per frame
  from the value set and any modulation (`devices::Params`); the EQ, the
  utility, the sampler and the synth play, and the other four follow
  ([engine.md](engine.md#built-in-devices), [app.md](app.md#built-in-devices)).
- **Plugin delay compensation.** Built: nodes report their latency
  (`Node::latency`), and `Graph::prepare` delays the earlier paths wherever
  paths of different latency meet, lines the cue output up with the output,
  and reports the graph's total (`PreparedGraph::latency`, `Stats::latency`;
  [engine.md](engine.md#delay-compensation)). Oversampled native stages will
  report their latency the same way.
- **The mixer.** Built: group and return tracks are buses with strips of
  their own, summing through `compile::Summing`; sends before or after the
  fader; pan, mute, solo, solo-safe and arm; a clip stage per channel; and
  peak and RMS meters in atomics, shown in the app's mixer
  ([engine.md](engine.md#the-mixer), [app.md](app.md#the-mixer)).
- **Automation.** Parameter events are already sample-accurate, but a lane
  belongs in the graph, not in the message queue, which holds 1,024
  messages. Compile each lane into its node, or into an automation node, and
  read it at the song position, so it loops and locates like everything
  else. A lane can give its node a value per frame, which needs no
  smoothing; values that arrive as events ramp over 10 ms
  (`nodes::Smoothed`).
- **Recording.** Built: the backend captures inputs, system loopback on
  Windows among them, lined up with the output frame for frame
  ([audio.md](audio.md#inputs)), and stamps each MIDI message with the
  frame heard as it came in ([audio.md](audio.md#midi-inputs)); each audio
  track monitors its input through an `InputMonitor` node, and each MIDI
  track through a `MidiMonitor`; and the app's recorder thread reads the
  inputs' lock-free buffers and the engine's timeline, and writes takes
  ([engine.md](engine.md#recording), [app.md](app.md#recording)).
- **Undo and redo.** Built: every committed transaction is a step in the
  document's history, and undo applies its edits' old values as a
  transaction of its own, which `Session::sync` sends to the engine like any
  other. A new kind of edit is undoable once it has its inverse
  ([commands.md](commands.md#undo-and-redo)). A drag, such as a fader's, is
  a gesture (`Action::BeginGesture` and `EndGesture`), so it's one step.
- **Export.** Rendering faster than real time is the engine without a
  device. `null_test::Player` already plays a project offline, block by
  block, with either renderer, its streams waiting for the disk; export
  does the same on a background thread and writes the samples to a file.
- **The clip stage.** Built: Hard and Soft clip types, in format version 2,
  and `native_stage()` in `compile.rs` puts their node in the post-fader
  slot. Tier 2's modelled stages go in the slots the same way.
- **Keys** are done: the keyboard shortcuts window changes any command's
  keys, shows conflicts and saves them to the keymap file, and the command
  palette runs any command ([app.md](app.md#changing-keys),
  [commands.md](commands.md#changing-keys-in-the-app)). Keymaps in the style
  of other DAWs would be keymap files to import.
- **ASIO** is built: our own host, behind `musicable_audio::open` like the
  other backends, in builds with the opt-in `asio` feature, which are
  distributed under the GPLv3 ([audio.md](audio.md#asio)).

## Known gaps

- Workers get real-time priority on Windows only. On macOS they should join
  the audio device's workgroup.
- Windows gives a process 32 real-time (MMCSS) threads and turns away any
  more, which then run at normal priority, where a shared block can wait on
  them. So the workers stop at 24 by default, leaving places for the audio
  thread, audio drivers and plugins, and the app stops an old engine's
  workers before a new engine's start. Plugins that take many places could
  still leave a worker out, and workers don't check for that yet.
- The engine plays in stereo. A device with more channels gets silence on
  the rest, and the cue output, with the metronome, plays through the same
  two channels as the master. Routing it to outputs of its own, such as
  headphones, comes with more channels.

## Testing

- `cargo test --workspace` runs in a debug build, so the allocation checker
  is on. It covers the equivalence tests, the real-time checks, file round
  trips, the commands and the shell, and the Tier 0 demo itself
  ([`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs)).
  CI runs it on Windows, macOS and Linux.
- The RTSan workflow (`.github/workflows/rtsan.yml`) runs the engine's and
  the plugins' tests with RealtimeSanitizer on Linux when they change, for
  what the allocation checker can't see, such as plugins' C and C++ code
  ([engine.md](engine.md#how-its-checked)).
- `musicable null-test [PROJECT]` plays a project with both renderers and
  compares them, on whatever machine it runs on.
- The scheduler benchmarks run in CI on Windows when the engine changes
  (`.github/workflows/bench.yml`). Hosted runners have few cores, so tune on
  real hardware ([CONTRIBUTING.md](../CONTRIBUTING.md#testing-and-measuring-the-scheduler)).
