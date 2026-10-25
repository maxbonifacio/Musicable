# Architecture

How Musicable fits together at the end of Tier 1, and where Tier 2 work
plugs in. A project model changes only through commands and transactions.
It's compiled into a graph of nodes, which a real-time engine plays through
the audio device, with a parallel scheduler that matches a single-threaded
reference renderer bit for bit. This page is the map. The detail is in
[engine.md](engine.md), [audio.md](audio.md), [commands.md](commands.md),
[project-format.md](project-format.md), [analysis.md](analysis.md) and
[app.md](app.md), and [the manual](../manual/src/introduction.md) says how
to use Musicable.

## Crates

| Crate | Depends on | Holds |
|---|---|---|
| `model` | nothing | The project as plain data: tracks and their routing, the tempo map, the loop, musical time, and the built-in devices' parameter tables |
| `commands` | `model`, `analysis` | The command registry, keymaps, and the `Document` whose transactions change the project, with their undo history |
| `engine` | `model` | Graphs and nodes, the compiler, the engine, the reference and parallel renderers, clip players and streaming, the built-in devices' DSP, the real-time checker |
| `audio` | `engine` | Device output: our own WASAPI backend and ASIO host on Windows; cpal elsewhere |
| `plugins` | `engine`, `model` | CLAP and VST3 hosting: plugin instances and the nodes that process them, and scanning in a process of its own. ARA in Tier 2 |
| `io` | `model` | Project files, and audio files: reading and writing WAV, AIFF and FLAC. MIDI files later |
| `library` | `model`, `io` | The browser's library: the folders of samples indexed file by file, with room for analysis, searching them, presets, and the queue for file work in the background |
| `analysis` | `io` | Analysis of audio for warping: onsets, the tempo, and beats and downbeats, from the Beat This! network with a classic tracker checking it ([analysis.md](analysis.md)) |
| `ui` | `model`, `commands`, `library` | What the window shows, drawn with egui: the arrangement, the mixer, the browser, menus and dialogs, from a theme and a mouse map; a `View` in, `Action`s out |
| `mcp` | `model`, `commands` | Agents controlling Musicable over MCP: the tools, the server the window answers through, the local pipe, and the `musicable mcp` bridge ([agents.md](agents.md)) |
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
| Disk (`musicable-disk`) | Reading the audio files that stream, ahead of the clips that play them. A second one streams long previews | Normal |
| Audio files (`musicable-audio-files`) | Reading the headers and the waveforms of the project's audio files, for the arrangement | Normal |
| Files (`musicable-files-1` and up) | The browser's file work: reading the library's index, scanning its folders and reading the headers of the files found, finding presets, opening previews and drawing their waveforms ([app.md](app.md#behind-the-browser)). Analysing the project's audio files ([analysis.md](analysis.md#in-the-app)) | Below normal, and scans and analysis in the system's background mode on Windows and macOS |
| Network (`rten-0` and up, once a file needs it) | The Beat This! network's work for the analysis that's running, as many threads as there are Files threads | Below normal |
| Recorder (`musicable-recorder`) | Writing what the armed tracks record, from the inputs' buffers, placed by the engine's timeline | Normal |
| Inputs (`musicable-input`, on Windows) | Capturing each WASAPI input, lined up with the output | MMCSS "Pro Audio" |
| Agents (`musicable-agents`, while agents are let in) | The MCP server, with Tokio: the pipe, each agent's connection, and checking their calls, which the main thread answers ([agents.md](agents.md#how-its-built)) | Normal |

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
2. `Session::key_pressed` looks the chord up in the `Keymap`: in the keys
   of the panel with the focus first, then in those that work everywhere,
   which bind Space to `transport.play_stop`.
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

Tier 2 has to keep these. The tests check most of them.

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
  automation work the same way.
- **What would click when it jumps ramps instead**, with
  `nodes::Smoothed`, as gain stages do. A ramp counts frames from its
  event, never blocks, so it lands on the same samples however the blocks
  fall ([engine.md](engine.md#parameter-smoothing)).
- **Node keys come from model ids** (`compile::keys`), so a node keeps its
  state when the project is recompiled. A new kind of node gets its own role
  in the key.
- **Every parameter change goes through `EngineHandle::send` or
  `send_at`**, even when it's also written to the model, so a value sent
  while a new graph was compiling isn't lost. It comes again after every
  swap, on the next block's first frame, with every other value set on its
  node, so **a node takes several events on one frame**
  ([engine.md](engine.md#graph-swaps)).
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

## What Tier 1 built

Tier 1 of the feature list, the minimum usable DAW, and where each part
lives. What each still lacks is in its doc's "Not done yet".

- **Clips.** Audio clips of WAV, AIFF and FLAC files, and MIDI
  clips of notes with per-note expression (MPE) and controller curves, on
  `Track` in format version 7. A clip player heads each audio and MIDI
  track's strip and renders from the song position. Short files load whole
  as the graph builds, and longer ones stream from the disk thread through
  lock-free chunks, ahead of the playhead and at the loop start. Files at
  other sample rates are resampled, and clips fade and crossfade. Notes are
  `NodeEventKind::Note`s at sample offsets, and controllers' moves
  `NodeEventKind::Controller`s, chased wherever playback starts. Both can
  pass through a chain of nodes, such as a MIDI effect, on their way to an
  instrument ([engine.md](engine.md#clips),
  [app.md](app.md#audio-files)). The arrangement view makes and edits them
  (below), and the piano roll edits their notes and controllers.
- **The arrangement view.** Tracks with headers, clips with
  waveforms and notes, the ruler with locators, the start marker and the
  loop brace, drawn with egui's painter, only what's in sight, so a hundred
  tracks of clips take 3 to 4 ms a frame ([app.md](app.md#the-arrangement)).
  Its gestures run commands with typed arguments, such as `clip.move`, and
  commands say when they apply (`When`). What it edits besides the project
  (the selection, the start marker, the grid, the zoom, the behaviour
  settings) is the `Editor`, kept with the `Document` outside the widgets,
  where the piano roll, extensions and agents find it too
  ([commands.md](commands.md#the-editors-state)). Colours and sizes come
  from a `Theme`, and gestures from a `MouseMap`, which the user's theme
  and mouse map files change.
- **The piano roll.** A MIDI clip's notes on rows of keys beside a
  keyboard, over a lane of their velocities or a controller's curve, in a
  tab beside the devices panel, drawn and tested as the arrangement is. Its
  gestures run the note and controller commands (`notes.move`,
  `controllers.draw` and the rest) and Live's keys, which act where the
  `Editor`'s focus is; it shares the arrangement's grid and song positions,
  and has a viewport of its own ([app.md](app.md#the-piano-roll)). Plugins
  get notes as CLAP's own or VST3's note expression, or as MPE MIDI, and
  controllers as MIDI or, for VST3, through the plugin's MIDI mapping
  ([plugins.md](plugins.md#notes-and-the-transport)). Recording takes the
  controllers' moves as the clip's curves, so a recorded MPE performance
  plays back into an MPE synth as it played, to the sample
  (`crates/app/tests/mpe.rs`).
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
- **Built-in devices.** Eight kinds, each with a parameter table in
  `musicable_model::native` that the format, the commands and the devices
  panel's controls all work from, in format version 8. The engine's
  `devices` module has the nodes, with each parameter worked out per frame
  from the value set or its automation, and any modulation
  (`devices::Params`); all eight play
  ([engine.md](engine.md#built-in-devices), [app.md](app.md#built-in-devices)).
- **Plugin delay compensation.** Nodes report their latency
  (`Node::latency`), and `Graph::prepare` delays the earlier paths wherever
  paths of different latency meet, lines the cue output up with the output,
  and reports the graph's total (`PreparedGraph::latency`, `Stats::latency`;
  [engine.md](engine.md#delay-compensation)). Oversampled native stages will
  report their latency the same way.
- **The mixer.** Group and return tracks are buses with strips of
  their own, summing through `compile::Summing`; sends before or after the
  fader; pan, mute, solo, solo-safe and arm; a clip stage per channel; and
  peak and RMS meters in atomics, shown in the app's mixer
  ([engine.md](engine.md#the-mixer), [app.md](app.md#the-mixer)).
- **Automation.** In format version 9, each lane is compiled into
  the node with its parameter, not sent through the message queue, and
  read at the song position, so it loops and locates like everything else.
  The mixer's `Gain` and the built-in devices read their lanes frame by
  frame with an `automation::Param`, which glides over jumps and adds
  modulation after its smoothing, and plugins get theirs as parameter
  events on the points' frames and on a 32-frame grid. Moving a control by
  hand overrides its lane until it's re-enabled
  ([engine.md](engine.md#automation), [commands.md](commands.md#automation),
  [app.md](app.md#the-arrangement)). Tempo automation is Tier 2.
- **Recording.** The backend captures inputs, system loopback on
  Windows among them, lined up with the output frame for frame
  ([audio.md](audio.md#inputs)), and stamps each MIDI message with the
  frame heard as it came in ([audio.md](audio.md#midi-inputs)); each audio
  track monitors its input through an `InputMonitor` node, and each MIDI
  track through a `MidiMonitor`; and the app's recorder thread reads the
  inputs' lock-free buffers and the engine's timeline, and writes takes
  ([engine.md](engine.md#recording), [app.md](app.md#recording)).
- **Undo and redo.** Every committed transaction is a step in the
  document's history, and undo applies its edits' old values as a
  transaction of its own, which `Session::sync` sends to the engine like any
  other. A new kind of edit is undoable once it has its inverse
  ([commands.md](commands.md#undo-and-redo)). A drag, such as a fader's, is
  a gesture (`Action::BeginGesture` and `EndGesture`), so it's one step.
- **Export.** An export plays the project offline in a session of
  its own, as `null_test::Player` does, on a background thread, faster than
  real time, and writes WAV, AIFF or FLAC files through
  `musicable_io::audio::Writer`. Stems are taps on the graph, lined up with
  the mix by delay compensation, and the tests check that an exported mix
  is what the app plays live, sample for sample
  ([app.md](app.md#exporting-audio)).
- **The clip stage.** Hard and Soft clip types, in format version 2,
  and `native_stage()` in `compile.rs` puts their node in the post-fader
  slot. Tier 2's modelled stages go in the slots the same way.
- **Keys** are done: the Settings window's Keys page changes any command's
  keys, everywhere or in one panel, shows conflicts and saves them to the
  keymap file, and the command
  palette runs any command ([app.md](app.md#changing-keys),
  [commands.md](commands.md#changing-keys-in-the-app)). Keymaps in the style
  of other DAWs would be keymap files to import.
- **ASIO** is built: our own host, behind `musicable_audio::open` like the
  other backends, in every Windows build ([audio.md](audio.md#asio)).

Audio Units and macOS were out of Tier 1's scope, since Windows comes
first, so VST3 plugins' windows and audio and MIDI inputs on macOS and
Linux are still to come, and so are MIDI outputs to devices. The rest of
what Tier 1 left is in each doc's "Not done yet"
([app.md](app.md#not-done-yet), [plugins.md](plugins.md#not-done-yet),
[audio.md](audio.md#not-done-yet)).

## Where Tier 2 starts

Tier 2 of the feature list, a studio replacement, and where each part
should go. None of it is built yet. Where Tier 1 left room for a part, this
says what's there.

- **ARA 2**, for Melodyne and the like. ARA rides on a plugin's instance,
  so it starts in `crates/plugins`, beside `Instance` on the main thread.
  The project holds what ARA calls the document already: audio clips, with
  their ids, are its playback regions, and their files its audio sources.
  Its change notifications go out from `Document::take_committed`, which
  says what each committed transaction changed
  ([commands.md](commands.md#transactions)), and each plugin's archive is
  saved with the project, in a new format version. A plugin reads an audio
  source from any thread, whenever it likes, so it needs a reader of its
  own over `musicable_io::audio::Decoder`, not the engine's streams. A
  plugin that plays its regions itself takes their clips' place, so the
  compiler leaves those clips out of the track's clip player.
- **Hybrid buffering, CPU meters per plugin, and multithreaded export.**
  [engine.md](engine.md#parallel-rendering) sketches hybrid buffering: the
  tracks nobody hears live render ahead in larger blocks on the same
  workers, into lock-free queues that the live graph reads. Nothing times
  nodes yet: a time for each node, kept in atomics as `Meters` keeps
  levels, would give each plugin its meter. Exports render on the parallel
  scheduler's workers already, in the app engine's block size
  (`export::Job`).
- **Plugin sandboxing.** Scanning loads each new library in a process of
  its own already (`plugins::scan`). Hosting a plugin in another process is
  another kind of `Instance`, which drives the plugin in a process that
  hosts it, with a node that hands it each block through shared memory. A
  process for each plugin, one for a group of them, or none differ only in
  how many plugins share one.
- **Warping and time-stretch.** Warping has started: format 11 gives
  `AudioClip` a warp, markers pinning frames of the file to beats of the
  clip's own time, and the clip player reads the file through them in
  Re-Pitch mode, resampling each piece at the speed it plays at, and in
  Slice, cutting it into slices that each play from where they land
  ([engine.md](engine.md#clips)), while the Sample Editor edits the markers
  ([app.md](app.md#the-sample-editor)). Re-Pitch and Slice carry no state
  but the resampler's, so they start at any song position at once, as every
  node must. A stretch that carries state from frame to frame, as a phase
  vocoder does for Spectral, can't, so it would play from stretched
  segments rendered ahead, on the disk thread that streams files now.
  Tempo, beats and transients come
  from analysing the file, on the library's background queue
  (`musicable_library::Queue`), and the project keeps what playback reads,
  such as the onsets a slicing mode plays from.
- **Sidechains, multi-output instruments and routing.** A node gets its
  inputs in the order they're connected (`Graph::connect`), and the nodes
  so far mix them together. A sidechain is a second connection into a
  device's node, which reads it apart from the first; plugins need their
  audio ports beyond the main ones for it
  ([plugins.md](plugins.md#not-done-yet)). A node's output is one buffer of
  `Node::channels()` channels, so a multi-output instrument is a node with
  more of them, and a node for each of its outputs picks that output's
  channels out for a strip of its own. In the model, a track feeds the
  master or a group (`Track::output`) and records from an input
  (`Track::input`), and flexible routing adds to both, such as another
  track's output as an input.
- **Per-app loopback on Windows 11.** System loopback is an input of its
  own kind (`musicable_audio::InputKind::Loopback`, whose id `loopback_id`
  makes), lined up with the output and resampled when its device runs on
  another clock ([audio.md](audio.md#inputs)). One app's sound, or
  everything but Musicable's, is a third kind, captured with WASAPI's
  process loopback, on the same path.
- **MPE editing in the piano roll.** Each note keeps its expression from
  the start (`clip::Expression`: curves of pitch bend, pressure and timbre,
  timed from the note's start), so moving, quantising and transposing a
  note keep it already. What's missing is showing and drawing the curves:
  lanes like the piano roll's for velocities and controllers, and note
  commands for them ([commands.md](commands.md#notes)).
- **Modelled clip types and console mode.** The clip stage's types are
  `ClipType` values, and `native_stage()` in `compile.rs` puts each one's
  node in its slot, so modelled types are more of both. Console mode fills
  the strip's other slots (`compile::Slot`) from `Track::console` and
  `Master::console`, which project files hold already, empty, and sums buses
  through a modelled `compile::Summing` ([engine.md](engine.md#the-mixer)).
  Oversampled stages report their latency (`Node::latency`), which delay
  compensation lines up, and each channel's variation seed is a new field of
  `console` ([project-format.md](project-format.md#coming-later)).
- **Freeze and flatten, and bounce in place.** An export renders any track
  on its own already, as a stem, from a tap on the graph
  (`compile_with_stems`). Freezing renders a track that way, from a tap
  after its devices, into a file that the track then plays in place of its
  devices, and flattening swaps the devices for the file's clip for good.
  A sidechained track's sources play along while it renders.
- **Folders, hidden tracks, VCAs, colours and templates.** Group tracks are
  buses already. Folders, hiding and colours are new `Track` fields, in a
  format version, for the arrangement and the mixer to draw. A VCA moves its
  tracks' faders without summing them, so it's the compiler's: a factor on
  each member's fader (`nodes::Gain`). A template is a project file to start
  from.
- **Tempo and time-signature automation, and tempo from audio.** The tempo
  map holds changes of tempo and of time signature already
  (`TempoMap::insert_tempo`, `insert_signature`), and everything that plays
  reads it, but nothing in the app adds one yet. Ramps are a new kind of
  change inside `TempoMap` ([engine.md](engine.md#musical-time)), and a
  format version. Tempo from audio is the beats that analysis finds (see
  warping), made into a map.
- **Windows, panels, workspaces, themes and profiles.** Each view is a
  panel with its own id (`musicable_commands::panel::MIXER`, `PIANO_ROLL`
  and the others), which the shell's panel registry holds with its title,
  its place and its size, docked in splits and tabs the user drags about
  ([egui_tiles](https://docs.rs/egui_tiles); [app.md](app.md#the-panels)).
  Clicking in a panel gives it the focus (`Editor::focus`), which an
  outline shows: the edit commands act on its selection, and its own keys,
  from its table in the keymap, win over those that work everywhere
  ([commands.md](commands.md#the-editors-state)).
  Panels the user moves out of the main window go in windows of their
  own (`crates/ui/src/windows.rs`; [app.md](app.md#windows-of-their-own)):
  egui immediate viewports that the shell draws in the main window's
  pass, each with its own tree of panels, its own focus, and the keys
  pressed in it, which the shell hands the keymap itself. While a drag
  from the browser or of a panel's tab is on, an egui plugin
  (`crates/ui/src/carry.rs`) passes the pointer on to the window under
  it, as the system sends it only to the window the drag started in. A
  tab dragged over another window goes there as if it were dragged about
  in that window, where egui_tiles shows where it would go, and out past
  the edge of its own window, its ghost is a little window of its own
  that follows the pointer (`crates/ui/src/ghost.rs`). The layout, with
  where each window is, is a `musicable_ui::Workspace`, a file in the
  config folder for each of the user's workspaces, which the app switches
  between ([app.md](app.md#workspaces)). Every view
  draws from a `Theme` and takes its gestures from a `MouseMap`
  (`Shell::set_theme`, `set_mouse_map`; [app.md](app.md#the-arrangement)),
  so a theme file fills a `Theme` ([app.md](app.md#themes)), a mouse map
  file changes Live's `MouseMap` ([app.md](app.md#settings-and-your-keymap)),
  and a profile is a folder of a keymap file, a mouse map file,
  workspaces, and the settings for the theme and the behaviour
  (`Editor::behaviour`), each saved as what differs from its base, which
  the app switches between in one step (`crates/app/src/profiles.rs`;
  [app.md](app.md#profiles)). The transport bar, the track headers and the
  mixer strips are lists of parts, which the user hides, moves and resizes
  (`parts.rs`; [app.md](app.md#configurable-views)), and each workspace
  keeps how they show them. The Settings window, outside the workspaces,
  has a page for each area of the settings, applies each change at once
  and keeps it (`crates/app/src/settings_window.rs`;
  [app.md](app.md#the-settings-window)).
- **Visible undo history.** The History panel lists every step of the
  undo history by name, and clicking one takes the project back or forward
  to it (`crates/ui/src/history.rs`; [app.md](app.md#the-history-panel)).
  `Document::steps` names the steps and says how many are done, and
  `history.go_to` undoes or redoes up to the step clicked, a step at a
  time, so the engine follows it as it follows undo
  ([commands.md](commands.md#undo-and-redo)).
- **MIDI learn.** MIDI inputs reach the engine stamped with their frames
  (`ProcessContext::midi`), and every parameter has one address
  (`ParamAddress`), which automation uses, and modulation and MIDI mapping
  can use too. A mapping turns a controller's moves into its parameter's
  changes: on the main thread, as a fader's drag does, or in the graph, on
  their frames.
- **Agents** control Musicable over MCP already, through the same
  commands, as one undo step for each call or batch
  ([agents.md](agents.md)). A window-less `musicable --headless` for them
  comes later.
- **Extensions.** They build on what Tier 1 made shareable: commands with
  typed arguments and results, which say when they apply
  ([commands.md](commands.md#arguments-and-results)), the `Editor`'s state
  outside the widgets ([commands.md](commands.md#the-editors-state)),
  context menus from the registry, panels with ids, the theme and the
  mouse map. An extension's device is a third kind of device beside
  Musicable's own and plugins: the compiler takes its node from
  `compile::Devices`, which holds plugins' node makers now, and its
  parameters have addresses as any device's do (`ParamAddress::Device`).
  Its WebAssembly gets its memory in `Node::prepare`, where every node
  allocates.
- **Modulators.** Tier 1 left room for them in the engine.
  `NodeEventKind::Modulation` offsets a parameter without changing its
  value, for every voice or for one note. The built-in devices' parameters
  say whether they can be modulated (`ParamInfo::modulated`), and add
  modulation frame by frame, after their smoothing
  (`automation::Param::modulate`). CLAP plugins get it as CLAP's parameter
  modulation, for one note too, and VST3 plugins keep their base values in
  the project, to get it as plain parameter changes. CLAP plugins and the
  built-in instruments say when their voices end (`Node::ended_voices`), for
  modulators that run a copy for each voice. The modulators themselves are
  new: in the model, on tracks, groups and the master, and in the graph, a
  node for each track that sends their targets the modulation.
- **Online sounds and services.** The browser's library keeps a `Record`
  for each file, with room for analysis: tags, measurements, and the
  embedding that searching with a sound compares.
  Online sounds add sources beside folders, an origin on each record, and
  each sound's origin and licence in the project, a format version; export
  writes the credits beside the mix. Freesound's previews need an Ogg
  Vorbis decoder beside the others in `musicable_io::audio`. A store plugin
  docked in the browser plays through the previews' path, which never
  reaches an export ([engine.md](engine.md#previews)).
- **Import and export.** Live's `.als` arrangements, DAWproject and MIDI
  files are formats of their own in `crates/io`, beside the project format,
  not versions of it ([project-format.md](project-format.md#coming-later)).
- **Tags and favourites in the browser.** The library keeps users' tags
  already, each saying who gave it (`TagSource`), so analysis's can join
  them; favourites are another field of `Record`. Tags aren't in the undo
  history yet ([app.md](app.md#not-done-yet)).

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
  trips, the commands and the shell, plugin hosting with the test plugins,
  recording and export, and the Tier 0 and Tier 1 demos themselves
  ([`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs),
  [`tier1_demo.rs`](../crates/app/tests/tier1_demo.rs)), whose songs play
  the same with both renderers. CI runs it on Windows, macOS and Linux.
- The RTSan workflow (`.github/workflows/rtsan.yml`) runs the engine's and
  the plugins' tests with RealtimeSanitizer on Linux when they change, for
  what the allocation checker can't see, such as plugins' C and C++ code
  ([engine.md](engine.md#how-its-checked)).
- `musicable null-test [PROJECT]` plays a project with both renderers and
  compares them, on whatever machine it runs on, without its plugins
  ([app.md](app.md#the-null-test)).
- The scheduler benchmarks run in CI on Windows when the engine changes
  (`.github/workflows/bench.yml`). Hosted runners have few cores, so tune on
  real hardware ([CONTRIBUTING.md](../CONTRIBUTING.md#testing-and-measuring-the-scheduler)).
