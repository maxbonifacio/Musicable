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
| `model` | nothing | The project as plain data: tracks, the tempo map, the loop, musical time |
| `commands` | `model` | The command registry, keymaps, and the `Document` whose transactions change the project, with their undo history |
| `engine` | `model` | Graphs and nodes, the compiler, the engine, the reference and parallel renderers, the real-time checker |
| `audio` | `engine` | Device output: our own WASAPI backend on Windows, cpal elsewhere |
| `plugins` | `engine` | Nothing yet: CLAP and VST3 hosting in Tier 1, ARA in Tier 2 |
| `io` | `model` | Project files; audio and MIDI files later |
| `ui` | `model`, `commands` | What the window shows, drawn with egui: a `View` in, `Action`s out |
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
| Workers (`musicable-worker-1` and up) | Their share of each block the parallel renderer shares | The same, on Windows only |

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
(`ParallelStats`). The window reads them every frame for the transport bar
and the meters. Old graphs and tempo maps come back through a second queue,
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

- **Clips and the arrangement.** Clips go on `Track` in the model, in a new
  format version, with per-note expression (MPE) in MIDI notes from the
  start. In `compile()`, a clip player takes the test tone's place at the
  head of the channel strip. Like the tone, it renders from the song
  position: the block's position and the tempo map say which part of which
  clip plays. The audio thread can't read files, so a streaming thread
  fills lock-free buffers ahead of the playhead and at the loop start, or a
  short file loads whole when the graph is prepared. Instruments need a new
  `NodeEventKind` for notes, at sample offsets like parameter events.
- **Devices and plugins.** A track's devices go between its source and its
  pre-fader slot, each with a `Slot::BeforeDevice` in front of it, where the
  comment in `compile()` marks the place. Each device needs an id in the
  model for its node key. CLAP and VST3 instances live in `crates/plugins`
  as `Node`s: created and activated in `prepare`, which runs on the
  compiler's thread, and handed from the old graph to the new one in
  `adopt`. A plugin's own parameter changes come back to the model as
  transactions, so undo covers them. Scanning should run in a separate
  process, so a plugin that crashes doesn't take the app with it.
  [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html) can check
  plugin code for what our allocation checker can't see.
- **Plugin delay compensation.** Nodes need to report their
  latency (a `Node::latency` method), and `Graph::prepare` needs a pass that
  delays the faster paths wherever paths of different latency meet, and
  reports the graph's total. Oversampled native stages report their latency
  the same way.
- **The mixer.** The model has return and group tracks, but `compile()`
  sends every track straight to the master. Groups become buses with strips
  of their own, sends become gain stages feeding a return's bus, and every
  bus sums through `compile::Summing`, so console mode can model it later.
  Pan and solo are new stages or parameters, and pan and send levels ramp
  with `nodes::Smoothed`. For an edit that changes the graph's shape,
  `Session::sync` calls `EngineLink::recompile`, which compiles the new
  graph in the background while the old one plays on. Meters need a way
  back from the engine: a peak and RMS level per track in atomics, read
  like `Stats`.
- **Automation.** Parameter events are already sample-accurate, but a lane
  belongs in the graph, not in the message queue, which holds 1,024
  messages. Compile each lane into its node, or into an automation node, and
  read it at the song position, so it loops and locates like everything
  else. A lane can give its node a value per frame, which needs no
  smoothing; values that arrive as events ramp over 10 ms
  (`nodes::Smoothed`).
- **Recording.** The audio backend only has outputs. Recording needs capture
  streams and their latency, input nodes in the graph, and a recorder that
  passes buffers to a disk-writing thread through a lock-free queue. The
  input and output latency place recorded audio where it was played. System
  loopback on Windows is another capture stream, resampled when its device
  runs on another clock.
- **Undo and redo.** Built: every committed transaction is a step in the
  document's history, and undo applies its edits' old values as a
  transaction of its own, which `Session::sync` sends to the engine like any
  other. A new kind of edit is undoable once it has its inverse
  ([commands.md](commands.md#undo-and-redo)). A drag, such as a fader's,
  should be a gesture (`Action::BeginGesture` and `EndGesture`), as the
  tempo field's is, so it's one step.
- **Export.** Rendering faster than real time is the engine without a
  device. `null_test::Player` already plays a project offline, block by
  block, with either renderer; export does the same on a background thread
  and writes the samples to a file.
- **The clip stage.** `Track::clip_type` and the channel strip's slots are
  in place. Hard and Soft are new `ClipType` values, which is a format
  version, and `native_stage()` in `compile.rs` puts their node in a slot.
- **Keys.** Remapping in the app, showing conflicts and the command palette
  build on `Keymap`, `KeymapFile` and the registry; commands.md has
  [notes](commands.md#notes-for-remapping-keys-in-the-app).
- **ASIO**, as an opt-in GPL build feature, moved to Tier 1. It's another
  backend behind `musicable_audio::open`.

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
- `musicable null-test [PROJECT]` plays a project with both renderers and
  compares them, on whatever machine it runs on.
- The scheduler benchmarks run in CI on Windows when the engine changes
  (`.github/workflows/bench.yml`). Hosted runners have few cores, so tune on
  real hardware ([CONTRIBUTING.md](../CONTRIBUTING.md#testing-and-measuring-the-scheduler)).
