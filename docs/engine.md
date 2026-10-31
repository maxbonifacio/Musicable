# The engine

How `crates/engine` turns the project model into sound: which thread does
what, how time and events work, and the rules for code on the audio thread.
[architecture.md](architecture.md) shows how the engine fits with the rest
of Musicable, and where Tier 1 work plugs in.

## Threads

```text
 control thread                                 audio thread
 ──────────────                                 ────────────
 Project ── snapshot ──► Compiler (own thread)
                             │ PreparedGraph
                             ▼
 EngineHandle ── message queue: events, swaps, tempo maps, previews ──► Engine
              ◄── garbage queue: old graphs, tempo maps, previews ──
              ◄── atomics: Stats ───────────────────────────────────

 disk thread ── chunks of audio files, ahead of each clip ──► clip players
             ◄── hints: where each clip reads next ──────────
```

- The **control thread** (the app's main thread) owns the model, a
  `Compiler` and the `EngineHandle`.
- The **compiler thread** (`musicable-compiler`) turns model snapshots into
  `PreparedGraph`s: `compile()` builds a `Graph`, and `Graph::prepare`
  calls each node's `prepare`, compensates for latency, orders the nodes and
  allocates their buffers. When
  several snapshots are waiting it compiles only the newest. A node that
  takes its time to prepare, as a plugin can, holds up this thread only.
- The **audio thread** owns the `Engine` and calls `Engine::process` or
  `Engine::process_interleaved` once per device callback, or their
  `_with_input` forms with the audio devices' inputs.
- With the `ParallelRenderer`, **worker threads** help the audio thread
  render each block (see [Parallel rendering](#parallel-rendering)), and
  with a lead set, render ahead of it between callbacks (see
  [Rendering ahead](#rendering-ahead)).
- The **disk thread** (`musicable-disk`) reads the audio files that stream
  ahead of the clips that play them (see [Clips](#clips)).

Both queues are lock-free single-producer, single-consumer ring buffers
([rtrb](https://docs.rs/rtrb)). Nothing on the audio thread locks, allocates
or frees.

## One device callback

1. The engine takes everything waiting in the message queue. Graph swaps and
   tempo maps are installed (see below) and events go into a store sorted by
   time.
2. It renders the frames the device asked for, in blocks of at most
   `EngineConfig::max_block` frames. A callback can be any length: longer ones
   are split, and a block also ends early wherever the transport changes or
   the loop jumps back.
3. For each block it applies the transport changes that are due, gives each
   node its parameter events with their offsets into the block, has the
   nodes that play notes play them ([Notes](#notes)) and the nodes that
   modulate others send their modulation ([Modulation](#modulation)), runs
   the renderer and copies the output to the device buffer, then adds the
   cue output (see below) on top.

A call for no frames does step 1 alone, so `Stats` says what the next
frame plays with: an export calls one first, to learn the latency to skip.
With a lead set, steps 2 and 3 happen before the callback asks, on the
workers, for all but the graph's live half, and the callback copies what
they rendered and renders the live half (see
[Rendering ahead](#rendering-ahead)).

## Time and events

**Engine time** counts the frames rendered since the engine started. It never
stops or jumps. `Stats::time` is the engine time at the end of the last
callback. The **song position** (`TransportState::position`) is separate: it
counts samples from the start of the song, advances only while the transport
plays, and jumps on `Locate` and at the loop end. `Stats::position` and
`Stats::playing` report it to the control thread.

- `EngineHandle::send` applies an event as soon as possible: at the start of
  the engine's next `process` call, or while the engine renders ahead, at
  the furthest frame it has rendered, unless it's for a node in the live
  half ([Rendering ahead](#rendering-ahead)).
- `EngineHandle::send_at(time, ..)` applies it at that engine time, exactly on
  its frame. An event for a time already past applies at the start of the next
  `process` call and counts in `Stats::events_late`.
- Events due at the same time apply in the order they were sent.
- A parameter event finds its node by `NodeKey` in the graph that is running
  when the event is due. If no node has the key, it counts in
  `Stats::events_unmatched`.
- A node gets its events for a block in `ProcessContext::events`, in order,
  each with an `offset` into the block. It applies each from that frame on.
- `NodeEventKind::Modulation` offsets a parameter from its value, as a
  modulator does, and the next one for the parameter replaces it. With a
  note id, it offsets the parameter for that note's voice alone, on top of
  the offset for every voice. Musicable's own nodes take the offset along
  the parameter's scale, and CLAP plugins get it as parameter modulation,
  in their own units, per note too. The modulators send it for every voice
  ([Modulation](#modulation)).
- `Event::Note` plays a note on a node from outside the graph. Notes aren't
  sent again after a swap, as parameter values are (see [Notes](#notes)).
  Live MIDI input doesn't come this way, but with the device callback (see
  **MIDI inputs** below).
- Transport commands are `Play`, `Stop`, `Locate` and `Loop`. Since a block
  ends where the transport changes and where the loop jumps back,
  `ProcessContext::transport` holds for every frame of the block, and while
  playing, frame `i` is at song position `position + i`.
- `ProcessContext::time` is the block's engine time, and `Engine::time` the
  engine time the next callback starts at.
- `ProcessContext::ahead` says whether the block renders ahead of the
  device ([Rendering ahead](#rendering-ahead)), and so needn't be ready
  within a callback: a VST3 plugin hears it as `kPrefetch`
  ([plugins.md](plugins.md#vst3)).

**Device inputs.** `Engine::process_with_input` and
`process_interleaved_with_input` take what the audio devices' inputs captured
for the callback, planar, exactly as many frames as the output: each input
device's channels, one device after another, as the audio backend lays them
out. Every node gets the block's share in `ProcessContext::input`, whatever
the blocks a callback splits into, and none when the stream has no inputs.
The backend gives them a fixed delay behind the output, the monitoring
latency it reports ([audio.md](audio.md#inputs)), so frame `i` of the block
is what the inputs captured that long before frame `i` is heard. Recording
doesn't go through the engine: it reads the inputs' buffers, which line up
with the output exactly. The resampler (`musicable_engine::resample`) is
public, for the backend's inputs on another clock.

**MIDI inputs.** The same two calls take the messages the stream's MIDI
inputs brought for the callback, as `midi::MidiMessage`s in order of time:
the engine time each plays at, which MIDI input it came from (its place
among the stream's), and its bytes, a MIDI 1.0 channel message. Each block
gets those due before it ends in `ProcessContext::midi`. One timed before
the callback plays at its first frame, and one timed after it at its last.
The audio backend times them ([audio.md](audio.md#midi-inputs)).

## Recording

`TransportCommand::Record(on)` is the record button: while it's on, the
transport records as it plays. `Punch { start, end }` narrows that to the
punch range, in song positions: recording punches in at `start` and out at
`end`, either of which can be left open. `TransportState::record` is the
button, and `recording` whether the transport records now. A block ends
where recording punches in or out, as at the loop end, so `recording` holds
for every frame of a block. `Stop` turns recording off.

The engine records nothing itself. It publishes the transport's course on a
`Timeline` (`Engine::take_timeline`, taken before the engine goes to the
audio thread): a `Segment` wherever the transport stops running on
steadily, with the engine time it starts at, the song position there,
whether it plays, records and is recording, and the graph's latency. A
recorder reads them on another thread, and `Read::until` says how far in
engine time they go. The queue holds 4,096 segments, and `Read::lost` says
when a reader fell so far behind that some were dropped.

The segments place what the inputs captured. What plays at song position
`p` is rendered at engine time `t`, and with the graph's latency `L`, it's
heard at `t + L`. Frame `t + L` of an input's buffer is what the input
captured then ([Device inputs](#time-and-events)), so it belongs at `p`,
the position the segment gives for `t`. That's the frame a player heard `p`
on, however late the plugins made it.

**Record taps.** A track that takes its input from another track's tap
records what the engine renders there, which no input buffer has. The
graph lines each record tap up with the output, as it does an export's
stems (`Graph::add_record`, `PreparedGraph::record`), so frame `t + L` of
it is what the output played at `t + L`, and belongs at `p` as an input's
frame does. The engine hands each record tap's frames to its `TapWriter`
as they play, after each block, with the engine time they start at: a writer
the control thread gives it for the tap's key (`EngineHandle::add_writer`,
`compile::keys::record`), and takes back with `remove_writer`. A block
without the tap, where the graph hasn't it or there's no graph yet, comes
with no channels: silence. It writes to `MAX_WRITERS`, 256, at most, and
a writer it replaces or drops goes back to the control thread with the
garbage. The app's writers fill buffers like an input's for its recorder
([app.md](app.md#recording)).

A MIDI track that takes another's notes records what the node at that
track's tap plays: its `MidiMonitor`, its clips' notes and its input's, or
the switch of one of its MIDI effects. The graph keeps them for each block
in a note record (`Graph::add_note_record`, `PreparedGraph::record_notes`),
keyed apart from the tap's record tap (`compile::keys::record_notes`), so a
MIDI track's Post FX can have both, and writers of two taps that come to
the same node, such as Pre FX and Post FX on a track without MIDI effects,
share its notes. The engine hands them to the writer's `write_notes` after
its frames, each at its offset into the block, the
block's notes empty where the graph hasn't the record. A note played at
`t` left the clips then, so it belongs at `p`, without the latency: the
notes of the other track's clips land on them, and those played live on it
where the engine played them, later than that track's own take has them by
as long as monitoring them took.

**Monitoring inputs.** `compile_with_inputs` gives each audio track an
`InputMonitor` after its clip player (key `compile::keys::track_input`),
which plays the track's input in place of its clips: always with
`Monitor::In`, with `Auto` while the track is armed and the transport is
stopped or recording, and never with `Off`, going over in 10 ms. An
`InputLayout` says where each input is among the engine's device inputs,
by the ids the audio backend gave them, and which input the default one is.
A track whose input isn't laid out monitors silence, and one taking
another track's sound has it on the node's side input
([Inputs from taps](#inputs-from-taps)). Arming and monitoring are the
node's parameters (`InputMonitor::ARMED`, `MONITOR`), so changing them
doesn't rebuild the graph. `compile` and `compile_with_devices` lay out no
inputs.

A MIDI track has a `MidiMonitor` instead, with the same key and parameters,
between its clip player and its instrument, on the way the notes go. It
turns the messages its input takes into notes ([MIDI input](#notes)) and
plays them as players expect of an instrument track: always with `In`,
while the track is armed with `Auto`, whether the transport plays or not,
and never with `Off`. A track taking another's notes has them come into
the monitor's side input for notes instead
(`MidiMonitor::with_side_input`), and plays them the same way. The clips
play too, except with `In` and while an armed `Auto` track records over
them. The notes sounding end when either
stops playing. Controllers go on from whichever plays, as their notes do:
when one stops, the switches it held on, such as the sustain pedal, are let
go, and when the clips play again, their controllers go back to what the
clips hold. It reads its parameters as it renders, after it has played
the block's notes, so arming changes what plays from the next block. The
layout lists the MIDI inputs (`InputLayout::add_midi`), and `find_midi`
says which messages a track's `MidiInput` takes: one input's or every
input's, one channel or all of them. A track whose input isn't laid out
plays none.

## Musical time

Musical positions are `Beats` from `musicable_model::time`: quarter notes,
stored exactly as whole ticks, 322,560 to the beat. That divides by every
tuplet up to 10 and by 1024, and a tick is shorter than a sample at any tempo
and sample rate we support.

The engine keeps the song position in samples, and a `TempoMap` (tempo and
time signature changes) says where musical positions fall in it:

- A musical position plays on the frame nearest to its exact time:
  `TempoMap::frame_at`, where halfway rounds up. It's worked out from the
  absolute position each time, never by adding up block lengths, so it lands
  on the same frame however the blocks fall. The `Sine` test tone takes its
  phase from the song position for the same reason. That's what lets tests
  compare renders bit for bit.
- `Locate` and `Loop` take musical positions, placed with the tempo map when
  they apply. A locate before the start of the song goes to the start.
- `EngineHandle::set_tempo_map` replaces the map at the start of the next
  `process` call. The playhead keeps its musical position, so a tempo change
  during playback doesn't jump, and the loop keeps its place in bars and
  beats. The old map goes back through the garbage queue, like an old graph.
- Nodes get the map in `ProcessContext::tempo`. Place musical events with it,
  as the metronome does: find the beats near the block with
  `TempoMap::beats_in`, then keep the ones whose `frame_at` falls in the
  block. Blocks don't end at tempo changes.
- Tempo changes are steps for now. Ramps (tempo automation, Tier 2) are a new
  kind of change inside `TempoMap`; nothing else in the engine needs to
  change for them. Project files need a new format version for them (see
  [project-format.md](project-format.md)).

**Looping.** With a loop region set, the playhead jumps back to the loop start
when it reaches the loop end: the block ends at the loop end's frame and the
next one starts at the loop start's. A playhead before the region plays into
it, and one already past the end plays on. A region shorter than a frame
doesn't loop.

**Metronome.** `compile()` adds a `nodes::Metronome` with key
`compile::keys::METRONOME`, feeding the cue bus, not the master. It clicks
on every beat of the time signature while the transport plays, accenting
each bar's first beat, and a click rings for 30 ms even if the playhead
jumps or stops. `Project::metronome` sets whether it starts on; switch it
with the `Metronome::ENABLED` parameter, like any other parameter.

**The cue output.** A graph can have a cue output as well as its output:
`Graph::set_cue`. The engine adds it to the device's buffer after the
output, so you hear it, but it's not in the mix, and nothing that records or
exports the master will have it. `compile()` sums the metronome into a cue
bus, `compile::keys::CUE`. It plays through the same two channels as the
master until the engine has more (see
[architecture.md](architecture.md#known-gaps)).

**Taps.** A graph can also have taps, nodes whose output an offline render
takes as well as the graph's: `Graph::add_tap`, numbered in the order
they're added. `Engine::process_with_taps` renders the output, without the
cue, into one buffer and each tap into one of its own. An export's stems
are taps ([app.md](app.md#exporting-audio)): `compile_with_stems` taps the
tracks it's given where they feed their bus, after their fader and their
slots and before the output stage that mutes them, so mute and solo don't
cut them. `Engine::process` and the device's callbacks leave taps alone.

**Setting up from a project.** The graph carries the metronome setting. The
tempo map and the loop are transport state, so send them when a project
opens and whenever they change:

```rust
handle.set_tempo_map(project.tempo.clone())?;
let region = project.looping.then_some(project.loop_region);
handle.send(Event::Transport(TransportCommand::Loop(region)))?;
```

## Compiling graphs

`Compiler::spawn` starts the compiler thread. Each job is a closure that
builds a graph and the `EngineConfig` to prepare it for:

```rust
let mut compiler = Compiler::spawn();
let job = compiler.compile_project(handle.config(), &project);
// Later, such as once per UI frame:
while let Some(compiled) = compiler.try_recv() {
    if compiled.job >= job {
        handle.swap_graph(compiled.graph?)?;
    }
}
```

- Jobs are numbered from 1, and `Compiler::compile` and `compile_project`
  return the number. Each `Compiled` result carries its job's number, so
  the control thread can tell a graph for a project it has since closed, or
  for an engine it has replaced, from the one it's waiting for.
- When several jobs are waiting, only the newest runs. The jobs it overtook
  have no results.
- A job that panics, such as a plugin that fails to load, stops the thread,
  and `try_recv` or `recv` raises the panic again on the control thread.
  `recv` waits for the next result, and panics if every job is done, rather
  than waiting forever.

The app's `Session` holds back what it sends the engine for a newly opened
project until its graph comes, so nothing plays before the graph
([commands.md](commands.md#running-commands-in-the-app)).

## Graph swaps

`EngineHandle::swap_graph` queues a prepared graph. The engine installs it at
the start of its next `process` call, before rendering anything:

- Each node in the new graph takes over from the old node with the same key
  (`Node::adopt`): oscillator phase, filter memory, plugin instances. A node
  with a new key starts fresh. `compile::keys` derives keys from model ids, so
  a track's nodes keep their identity when the project is recompiled.
- The old graph goes back to the control thread through the garbage queue.
  Call `EngineHandle::collect_garbage` regularly, such as once per UI frame:
  while 16 old graphs are waiting, the engine holds further swaps (and the
  events queued behind them).
- A graph must be prepared for the engine's `EngineConfig`. `swap_graph`
  refuses one prepared for a different sample rate or block size.

A graph compiled from a snapshot taken before a parameter change has the old
value. So the handle remembers, for each parameter, the value sent for it
that applies last, and sends it again after each swap. **Every parameter
change goes through `send` or `send_at`**, even when it's also written to the
model. Values sent again are never counted as late.
`EngineHandle::forget_params` forgets them, for when the graphs to come
belong to another project, whose nodes have values of their own.

## Delay compensation

A node with latency, such as a plugin that looks ahead, says how many frames
late its output is: `Node::latency`, read once the node is prepared.
`Graph::prepare` then lines up every path through the graph:

- Each node's output is as late as the latest path into it, plus its own
  latency.
- Wherever paths of different lateness meet at a node, such as tracks at the
  master's bus or a group's, or sends at a return's, the earlier ones go
  through a `nodes::Delay` that makes up the difference. So a track with a
  latent plugin lines up with a dry track to the sample.
- The cue output gets a delay that lines it up with the output, or the
  output one that lines it up with the cue, whichever is earlier, so the
  metronome clicks in time with what plays. Taps line up the same way: the
  output, the cue and each tap are delayed to the latest of them, so an
  export's stems line up with its mix to the sample.
- `PreparedGraph::latency` says how late the output is in the end, and
  `Stats::latency` reports the running graph's. The audio device adds its
  own output latency on top. So what you hear left the clip players that
  much before `Stats::position`, which is where a playhead that follows the
  sound would subtract it.
- A delay carries on with what the delay in the same place held in the
  graph it replaces, so a recompile doesn't drop out. Its place is the
  connection it's on, named by the keys of the nodes at either end. When
  the latency changes, the path has to jump in time: the new delay keeps
  the newest frames it has room for, and a longer one plays silence first,
  for the frames the old one didn't hold.
- A node whose latency changes, as a plugin's can when one of its settings
  does, needs a new graph, and the new graph compensates for the new
  latency.

Two things aren't delayed. Notes sent with `Graph::send_notes` or
`send_side_notes` arrive at an instrument when they're due, so its sound
lines up as long as nothing latent comes before it on its track, which is
the usual chain. And meters measure
where they are, before the delays at the buses, so a latent track's meter
moves later than a dry one's.

## Bypassing devices

Each device's node sits in a `switch::Switch`, which has the device's key
and bypasses it as the device's Bypass says (`Device::bypassed`). A
bypassed device stops, and what came into it plays on in its place,
delayed by its latency, so nothing else in the mix moves and no new graph
is needed.

- While the device plays, the switch plays it and keeps a dry copy of
  what comes into its main input, delayed by the device's latency, so the
  copy lines up with the device's output frame for frame. A side input,
  such as a compressor's sidechain, goes to the device but never plays in
  its place ([Wires](#wires)).
- Bypassing crossfades from the device to the dry copy over 10 ms, the
  time automation's steps take, and the device's tail goes with the fade.
  Once the fade is over, the switch stops the device (`Node::stop`). From
  then on the device gets each block's events through `Node::flush`
  instead of rendering, so its parameter changes still land, and only the
  dry copy costs anything.
- Bringing the device back resets it at once (`Node::reset`): its delay
  lines, filters, envelopes and voices empty, and its parameters at their
  values, without a ramp. It renders from that frame, and the crossfade
  back to it starts its latency later, so a device that looks ahead has
  caught up before it's heard.
- The switch's parameter is `Switch::BYPASS`, `ParamId(u32::MAX)`: 1.0
  bypasses and 0.0 brings back. CLAP (`CLAP_INVALID_ID`) and VST3
  (`kNoParamId`) keep that id from plugins, so it's never one of theirs,
  and device nodes ignore it. `keys::param` gives a Bypass lane
  (`ParamAddress::Bypass`) to the switch, which takes it as events
  (`Switch::automate`), a switch on each step.
- A switch applies to what comes into the device from its event's frame,
  so the fade starts at the output the device's latency later, where that
  frame comes out: after delay compensation, a lane switches on the frame
  it asks for. Where the device starts or stops partway through a block,
  it renders that part as a block of its own, so the switch renders the
  same samples however the blocks fall. Up to 16 switches can be on their
  way through a device at once, and one more replaces the last, so the
  latest lands.
- In a swap, a switch carries on from the one it replaces
  (`Node::adopt`): its device's state, the dry copy and any fade under
  way. Once a lane stops moving it, it goes back to the project's Bypass.
- A bypassed instrument is silent, and its notes end with the fade. Notes
  that start while it's bypassed aren't heard: it comes back fresh, and
  the next note plays.

A deactivated device (`Device::active` false) is out of the signal
altogether, latency and all, so every path that meets its track moves,
and each change is a new graph. Its switch (`Switch::deactivated`) is
still in the graph, though not in the strip: it never starts its device,
so a plugin stops on the audio thread as the graph takes over, and its
node goes on taking parameter changes through `flush`. A deactivated
instrument still takes the track's notes, so they don't go to an
instrument after it. A MIDI effect's node sits in a `switch::NoteSwitch`
instead, which bypasses it in the notes' way ([MIDI effects](#notes)).

## Parameter smoothing

A parameter that would click when it jumps ramps to each new value instead:
`nodes::Smoothed` moves to it in a straight line over 10 ms, starting on
the frame of the event. `Gain`'s gain, pan and mute, and the `Sine` test
tone's level, ramp this way.

- A ramp counts frames, never blocks, so a change on a given frame makes
  the same samples however the blocks fall and whichever thread runs the
  node. The renderers stay bit-identical.
- A node calls `Smoothed::prepare` from `Node::prepare`, for the sample
  rate, `set` for each event on its frame, and `next_value` once per frame.
  While it's settled, it can `skip` the block and use one value for it, as
  `Gain` does to copy, scale or clear its input as cheaply as before.
- A node that hasn't made a sound yet takes new values at once, so a new
  track starts at its gain, not with a fade from silence.
- A new node carries on the ramp of the node it replaces in a swap
  (`Smoothed::take_over`, from `Node::adopt`), so recompiling in the middle
  of a fade doesn't jump.

## Automation

A parameter's automation is compiled into the graph and read at the song
position, never sent through the message queue, so it loops, locates and
follows tempo changes as clips do.

- `compile()` makes an `automation::Automation` for each lane that isn't
  overridden: its points, in beats, and its `Scale`. `keys::param` says
  which node has the parameter and which `ParamId` it is: a channel's
  fader, a `Gain`, for its volume (`Gain::GAIN`) and its pan
  (`Gain::PAN`), a send's `Gain` for its level, a device's node for its
  parameter `n` (`ParamId(n)`), and a modulator's channel's `Modulation`
  node for the modulator's parameter `n` (`modulation::param_id`).
  `Graph::automate` gives it to the node.
- Whoever reads it places the points through the block's tempo map
  (`ProcessContext::tempo`), each on the frame `TempoMap::frame_at` gives.
  Between two points the value moves in a straight line in the lane's
  scale: in the fourth root of the gain for faders and sends, as their
  faders spread, in ratios on a log scale, and evenly otherwise. Two points
  at the same place make a step, and on its frame the value is the one
  after it.
- **A node that reads its own** keeps it (`Node::automate` returns `None`)
  and works the value out frame by frame with an `automation::Param`, as
  `Gain` does for its gain and pan, and the built-in devices for each of
  their parameters ([Built-in devices](#built-in-devices)). A `Param` follows its automation in
  place of the value set, which ramps over 10 ms as `Smoothed` does and
  applies again once the automation goes. Between points it follows the
  line exactly. Wherever the automation jumps, at a step, where the
  playhead locates or loops, or where a new graph brings automation the
  old one hadn't, the value glides there over 10 ms instead, so it doesn't
  click. Modulation (`Param::modulate`) moves the value along the
  parameter's scale after the ramp and the glide, ramping to each new
  amount over 32 frames, a modulator's tick
  (`automation::MODULATION_RAMP`), and the sum stays within the
  parameter's range, so automation, modulation, MIDI mapping and
  extensions can all address the same parameter.
- **Other nodes** hand their automation back, and the graph sends them its
  values as `Param` events (`automation::Feed`): on the frame of each point
  (up to 16 a block), every 32 frames of engine time between them, and on
  a block's first frame when the playhead has jumped or the graph is new,
  each only when the value changed. They come after the engine's own
  events on the same frame, so they win. Plugins get theirs this way, and
  smooth their parameters themselves: a CLAP plugin gets values in its own
  range, a VST3 plugin its normalized ones.
- The events fall on the same frames however the blocks fall, so the
  renderers stay bit-identical with automation at any block size
  (`crates/engine/tests/automation.rs`).
- Moving an automated control by hand overrides its lane
  ([commands.md](commands.md#automation)), and the next graph leaves the
  lane out, so the parameter ramps from where the automation left it to
  the value set.

## The mixer

`compile()` builds every track as a channel strip, and the master too, with
built-in slots where native stages sit: the clip stage now, console
modelling in Tier 2.

```text
source → [before device 0] → device 0 → … → [pre-fader] → fader → [post-fader] → [bus feed] → output stage → bus
```

- **Sources.** An audio or MIDI track's source is its clip player, with the
  track's test tone feeding it if it has one (see [Clips](#clips)). A group
  or return track's source is its bus, where what feeds it sums, along with
  its own test tone if it has one.
- **The fader** (`compile::keys::track_gain`, and `keys::master_gain` for the
  master's) is a `Gain` with the channel's gain and pan. Pan is a balance
  control (`musicable_model::mixer::pan_gains`): the channel on the side it
  turns towards stays at unity, and the other falls in a straight line to
  silence. A centred channel passes unchanged, exactly, and panning never
  boosts.
- **The output stage** (`keys::track_output`) is a `Gain` at unity, after the
  fader and the slots. Mute and solo cut the track's output there, and the
  track's meter measures it. The master's output stage
  (`keys::master_output`) is the graph's output.
- **Buses.** A track feeds the master or a group track
  (`Project::output_of`), or with `Bus::SendsOnly`, nothing: it's heard
  through its sends and the tracks that take its taps alone, and its output
  stage stays, metering what it would feed. Every bus sums through the
  `compile::Summing` kind the compiler picks, so console mode can add a
  modelled one; there's only `Summing::Plain` so far.
- **Sends** (`keys::send`) are `Gain`s at the send's level, feeding a return
  track's bus. A pre-fader send takes the signal after the pre-fader slot,
  unpanned. A post-fader send takes it after the post-fader slot, so it
  follows the fader, the pan and the clip stage.
- **No loops.** The model only allows routing that can't loop:
  `Project::can_feed` and `can_send`. Groups can feed groups, but not ones
  that feed them; return tracks feed the master and have no sends; sends go
  to return tracks, one per return. `output_of` and `sends_of` leave out
  what breaks the rules, so even a hand-edited file compiles. A wire, or a
  track's input from a tap, can close a loop with them, and has rules of its
  own ([Wires](#wires), [Inputs from taps](#inputs-from-taps)).
- **Devices** (`Track::devices`, `Master::devices`) sit between the source
  and the pre-fader slot, in order, each keyed by its `DeviceId`
  (`keys::device`), so a device keeps its state when it moves. Plugins
  (`DeviceKind::Plugin`) process outside the engine, in plugin instances
  the app holds: `compile_with_devices` takes `Devices`, a `NodeMaker` for
  each plugin device, and a plugin device it has none for, such as one
  whose plugin isn't installed, passes its signal through
  ([plugins.md](plugins.md#plugins-on-tracks)). Two more kinds are for
  tests: `DeviceKind::TestGain`, and `DeviceKind::TestInstrument` (see
  [Clips](#clips)). On a MIDI track, the first device that plays notes
  gets them, even one without a node, so they don't go to another
  instrument after it.

**Slots.** The slots (`compile::Slot`) belong to the channel, not to its
device list: one in front of each device, then pre-fader, post-fader and bus
feed.

- `native_stage()` in `compile.rs` says what sits in each. So far that's
  the channel's clip stage (`nodes::Clipper`), in its post-fader slot, when
  its clip type (`Track::clip_type`, `Master::clip_type`) is `Hard` or
  `Soft`. Hard cuts the signal off at full scale. Soft leaves everything up
  to half scale (-6 dB) alone and bends smoothly towards full scale above
  it, never quite reaching it. Console mode (Tier 2) fills slots from
  `Track::console` the same way.
- An empty slot adds no node, so a strip with every slot empty is exactly
  the graph it was before slots existed. Filling one, such as changing a
  clip type, is an ordinary recompile and swap.
- A stage's key comes from its track id and slot (`keys::track_slot`), so it
  keeps its state across swaps like any other node. Moved to another slot,
  it starts fresh. The slot in front of a device is keyed by the device's
  id instead, so it goes with the device, along the chain or to another
  track, as the device's own node does.

**Mute and solo.** `Project::silenced` works out which track outputs and
sends are cut, and `compile()` builds that into each output stage and send
as its `Gain::MUTE`. A muted track's output and all its sends are cut. While
any track is soloed, only the signal paths through a soloed track are
heard, as in Live:

- A soloed group solos the tracks that feed it.
- A soloed track passes, and so does everything downstream of it: its group,
  the returns it sends to, and theirs.
- Soloing a return plays what the sends put into it, without the tracks'
  own outputs: the wet signal alone.
- A solo-safe track, as return tracks start out, passes as if it were
  soloed, without starting a solo itself.

Cutting outputs and sends rather than whole tracks is what lets one path go
quiet while another plays on. A change to mute or solo needs no new graph:
the app sends `Gain::MUTE` to each output stage and send that changes.

**Parameters.** The fader's and the send's `Gain::GAIN` and `Gain::PAN`, and
the output stage's and the send's `Gain::MUTE`, all ramp
([Parameter smoothing](#parameter-smoothing)). Everything else in the mixer,
such as where a track's output goes or its clip type, changes the graph's
shape and recompiles.

**Meters.** Each channel's output stage measures what it passes into a
`meter::Meter`: the peak and RMS level of each channel, and the highest
sample since the meter was last cleared. The readings live in atomics, so
the UI reads them without waiting on the audio thread, and the audio thread
never waits on the UI. The peak falls back at 20 dB a second once the signal
drops, the RMS averages over about 0.3 s, and the highest sample holds
until `Meter::clear_highest`.

A meter measures each block in stretches of up to 256 frames, and keeps
each stretch's readings with the engine time it ends at, in a ring of 256
slots, one for each stretch, which reaches back 1.4 s at 48 kHz and the
longest lead at 192 kHz. `Meter::level_at(time)` reads the last stretch
that ended by `time`, so while the engine renders ahead, the UI reads the
level of the frame the device plays (`Stats::time`), not of the one
rendered last, up to the lead later. Each slot's time stands in for a
sequence number: the writer sets it to `u64::MAX` before it changes the
readings, and to their time after, and a reader that sees it changing, or
sees it change while it reads, goes to the slot before. Until the first
stretch a meter measured ended, it reads silence, as it starts, so a meter
new to a graph that went in ahead of what's heard, such as a new track's,
stays still until its sound is heard; a time its ring has gone round since,
which nothing from by then is left for, reads the latest. `Meter::level` is
the latest, and the highest reading is always the latest. An engine that
takes over from another starts its time again from 0, so the session calls
`Meters::restart` first, which forgets every reading but the highest, as the
old engine's would read as the new one's.

- `Meters` keeps a meter for each track and one for the master.
  `compile_metered(project, meters)` puts them in the graph's output stages;
  call `Meters::follow` first, so new tracks get one. `compile()` is the
  same with meters nobody reads.
- A meter carries on across swaps: the new graph's output stage gets the
  same `Arc<Meter>`. One node measures into each meter, so its readings
  aren't raced.
- The app's `Session` keeps the `Meters` and compiles with them, and the
  mixer reads them every frame (see [app.md](app.md#the-mixer)).
- `Meters` also keeps a `meter::Reading`, an atomic number, for each of the
  modulators' outputs and each parameter routings move, which `follow`
  keeps as it does the meters. A reading keeps its times as a meter does:
  `Reading::set(time, ..)` and `get_at(time)`, which is none before the
  first, so the window shows a new Macro's handle at its control's value
  until then. The `Modulation` nodes set them at each tick
  ([Modulation](#modulation)), and the window shows them on the modulator
  strip's handles and the controls they move (see
  [app.md](app.md#the-modulator-strip)).

## Wires

A wire (`musicable_model::routing::Wire`) takes the signal at a point on a
channel, a tap, into a device's side input, a port, such as the
compressor's and the gate's Sidechain ([Built-in
devices](#built-in-devices)) or a plugin's side inputs
([plugins.md](plugins.md#side-inputs)). A note wire takes a MIDI track's
notes into an instrument's or a MIDI effect's note input instead
([Notes](#notes)). `compile()` adds the wires once every strip is built, so
a wire can join any two:

- **Taps.** As it builds each strip, the compiler notes where each tap's
  signal is. A track's Pre FX is the head of its strip, its source. A
  device's tap is what its switch puts out, or what comes into it where
  it's deactivated. Post FX is the last node before the fader, where
  pre-fader sends take their signal, and Post Fader the last after the
  post-fader slot. The master has Post FX and Post Fader. Every tap comes
  before the output stage, where mute and solo cut, so a muted track's
  wires carry on: a muted ghost kick still ducks the bass. A MIDI track's
  Pre FX is notes, and so is the point after each MIDI effect in front of
  its instrument (`Project::carries_sound`), which only a note wire takes.
- **The wire** (`keys::wire`) is a `Gain` at the wire's gain, fed by its
  tap, and feeding its device's switch on a side input: 0 for the
  Compressor's or the Gate's Sidechain, and for a plugin's side input
  (`Port::PluginInput`) the plugin's id for it, which its nodes mix into
  that port. Its `Gain::GAIN`
  ramps as a send's does, a lane (`ParamAddress::Wire`) automates it, and
  modulators move it from silence to +24 dB (`Gain::with_max_gain`), from
  the channel of the device it feeds ([Modulation](#modulation)). Wires
  into the same port sum there.
- **Ports.** `Graph::connect_to` feeds a node's input `port`:
  `InputPort::Main`, the sum every node hears, which `connect` feeds, or
  `InputPort::Side(n)`. `Graph::prepare` puts each node's inputs in order
  of port, the main input's first, and the node reads them through
  `Inputs`: `iter` and `get` give the main input's, and `side(n)` those of
  side input `n`. A node without a side input never looks at one.
- **Delay compensation** lines up a side input as it does any path that
  meets another at a node ([Delay compensation](#delay-compensation)):
  whichever of the main input and the key is earlier waits for the other,
  so the key comes in with the beat it belongs to, to the sample. Where
  the key is the later one, say through a linear-phase EQ on the kick, the
  keyed track waits for it, and the master lines the rest of the mix up
  with that.
- **The switch** passes its device every input, side inputs too, for each
  part of a block it renders on its own (`Inputs::range`). Its dry copy is
  what comes into the main input, so a bypassed compressor plays its own
  input, never its key. A deactivated device's switch is out of the strip,
  so the wires into it have nothing to feed, and the compiler leaves them
  out.
- **Which wires play.** `Project::live_wires` gives the wires the engine
  plays: each whose ends the project has and whose tap carries what its
  port takes, sound or notes, unless it would close a loop with the
  outputs, the sends and the wires before it. In a project the commands
  edit, that's all of them, since they refuse a wire that would make a
  loop, but even a hand-edited file compiles.

**Loops.** Outputs and sends can't loop on their own, but a wire can close
a loop with them, such as a compressor keyed from a group its own track
feeds, from the master, or from after itself. `Project::check_wire`
follows the signal point to point, not track to track, so two tracks can
key each other from their Pre FX, which comes before either's devices. The
commands refuse a wire that would close a loop, and an edit that moves a
device, an output or a send takes itself back if `Project::route_in_a_loop`
finds one, through a wire or a track's input ([commands.md](commands.md#wires)).

**Edits.** A wire's gain is a parameter, sent to its `Gain` as a send's
level is. Adding a wire, removing one or moving it to another tap changes
the graph's shape and recompiles, and so does a note wire's channel.

## Inputs from taps

An audio track's input can be a tap on another track (`Input::Tap`), the
same points a wire takes, or the master's Post Fader, which is Resampling
(`Input::RESAMPLING`). A MIDI track's can be another MIDI track's notes
(`Project::carries_notes`): at its Pre FX, from its clips and its input,
after one of its MIDI effects, what that plays, or at its Post FX, what
reaches its instrument. It can also be the notes a plugin on any track
plays out as it renders, after it ([Notes](#notes)). The compiler adds
them once the strips and wires are built:

- **Monitoring.** The track's `InputMonitor` takes the tap on its side
  input 0 (`InputMonitor::with_side_input`) and plays it as it would an
  interface's input, with the same monitoring. Delay compensation lines it
  up, so the track plays the other track's sound in time with it, whatever
  latency either has.
- **Resampling.** The master's Post Fader feeds no node: the track records
  it but never plays it (`InputMonitor::resampling`). It plays its clips
  whatever its monitoring, and nothing while it's armed and the transport
  records: its monitor cuts what goes into its devices, and a
  `nodes::ResamplingCut` after them (`InputMonitor::cut`,
  `keys::track_cut`) what they ring on with, so its output, its sends and a
  track taking its Pre FX or its Post FX have none of it. A point after one
  of its devices has only what the device rings on with. Both cut on the
  frame recording starts, without the 10 ms ramp, so none of the track is
  in what it records, and come back on a ramp after. The cut follows the
  monitor's arming, which the monitor leaves it in an atomic after each
  block, so it has no parameters of its own.
- **Notes.** A MIDI track taking another's notes has the node that plays
  them at the tap send them to its own monitor's side input for notes
  (`Graph::send_side_notes`), which plays them with its clips' as it would
  a MIDI input's, with the same monitoring. At Pre FX that's the other
  track's `MidiMonitor`; after one of its MIDI effects, the `NoteSwitch` of
  the last one up to there that plays, or the monitor where none does, so
  after a deactivated one the notes are those that come into it; and at
  Post FX, the last of them all, whose notes reach the instrument. The
  other track's clip player sends its notes to its monitor even if it has
  no instrument, and a MIDI effect plays without one, so a track can play
  one that has none. After a plugin that plays notes out, the plugin's
  switch sends its made notes (`Graph::send_side_made_notes`), so the
  track renders a stage later. They're taken before mute and solo, as a
  tap is.
- **Record taps.** Each audio track's tap gets a record tap, keyed by the
  tap (`keys::record`), so tracks taking the same one share it, and each
  MIDI track's a note record of the notes it takes, keyed by the tap too,
  apart (`keys::record_notes`) ([Recording](#recording)).
- **Which inputs play.** `Project::live_inputs` gives the inputs the engine
  plays: each whose ends the project has and whose tap carries what the
  track takes, unless it would close a loop with the outputs, the sends and
  the inputs before it, in track order. Resampling never closes one. The
  wires come after them, so `live_wires` leaves out a wire that would close
  a loop through an input. In a project the commands edit, that's all of
  them, since they refuse an input that would make a loop
  (`Project::check_input`).

## Clips

An audio or MIDI track's source is its clip player, `clips::AudioClips` or
`clips::MidiClips` (`keys::track_clips`), at the head of its strip, with the
track's test tone feeding it if it has one. Both play from the song
position, like everything else: what a clip plays on a frame depends on
where the frame is in the song, never on how the playhead got there or how
the blocks fall. So clips loop, locate and render the same, bit for bit, in
both renderers.

**Audio clips.** `AudioClips` plays each clip's file for the clip's
length. An unwarped clip plays it at the file's own speed, from the frame
its `offset` gives, whatever the tempo. A warped clip follows the tempo:
its warp's markers pin frames of the file to beats of the clip's own time,
which starts at its `offset`, and `Placed::pieces` cuts the song's frames
into pieces that each play the file at one speed, from a tempo change or a
marker to the next. In Re-Pitch, a piece is resampled at its speed, so the
audio's pitch follows it; one at the file's own speed, at the device's
rate, plays sample for sample. In Slice, the file is cut into slices, each
played at the file's own speed, or as transposed, from where its start
lands (`clips/slice.rs`), as Live's Beats plays them (see **Slice** below).
In Granular, it plays as short overlapping grains, each at the file's own
speed, or as transposed, from where it lands (`clips/granular.rs`), as
Live's Tones and Texture play them (see **Granular** below). In Spectral,
it plays segments of the file stretched ahead of time by Signalsmith
Stretch, or with High quality on, Rubber Band (`clips/spectral.rs`), as
Live's Complex and Complex Pro stretch them (see **Spectral** below).

- Mono files play in both channels, and files with more than two channels
  play their first two.
- A clip that runs past the end of its file is silent from there. So is a
  clip whose file the graph doesn't have, such as one that's missing.
- Fades are quarter sines, worked out for each frame from the clip's edges.
  Where one clip fades out over the same frames as another fades in, they
  crossfade at constant power. Clips that overlap sum.
- A file at another sample rate is resampled (`resample.rs`): a
  Kaiser-windowed sinc, 32 zero crossings each side, about 90 dB down by
  Nyquist, passing up to about 41% of the lower rate (18 kHz from 44.1 kHz).
  Every output frame's weights come from where it falls in the file alone.
  A warped piece that plays faster than the file widens the sinc to filter
  what would alias, at most eight times
  (`Resampler::with_step_widened_at_most`), so a piece's reach into the
  file stays bounded however fast it plays.
- A clip's gain follows its Gain curve where it has one, worked out for
  each frame in the clip's own time: the file's seconds for an unwarped
  clip and beats for a warped one, so the curve stays with the audio.
- Each piece plays as one or more `Run`s: stretches of song frames that
  read the file at one speed from one place, faded one way. A Re-Pitch
  piece is one run, a Slice piece the runs of its slices, which can
  overlap, and a Granular piece the runs of its grains, two at a time. A run reads sample for sample where it moves a frame of the file
  a frame, forwards or backwards, rounding with `nearest` (halfway down, so
  positions worked out two ways round the same), and resampled otherwise.

**Slice.** A clip in Slice keeps its slices (`slice::Slices`), worked out
as the graph compiles, and plays each from the song frame its start lands
on.

- Slices start at the clip's transients in sight and the file's start, or
  on a grid of notes in the clip's own time from its 1.1.1, as Preserve
  says. A clip whose audio isn't analysed yet is cut on sixteenths. A bar
  is as long as the song's signature at the clip's start makes it, so a
  change of signature recompiles the graph. With a Preserve curve, each
  grid cuts, and the transients do, where the curve holds them.
- A slice reads its stretch of the file at the file's own speed, times its
  transpose and detune, from the first song frame on or after its start
  until the next slice's. It reads Transpose, Detune, Transient Loop Mode
  and Transient Envelope from the clip's curves where it starts, or else
  from the warp, so automation moves them a slice at a time.
- Each slice fades in over 10 ms just before its start, as the one before
  fades out: equal-gain raised cosines, at most half as long as the slice
  before, as it plays and of its audio, so the transient lands at full
  level, with no click. Where two slices meet seamlessly, with no envelope,
  the same pitch and reading the same frames, as at the file's own speed,
  there's no crossfade, so the clip plays the file sample for sample, as
  Re-Pitch would. A crossfade much shorter bends a low note sharply enough
  to click: over 2 ms, a 50 Hz sine bends up to 26 times as sharply as its
  own wave does, and over 10 ms less than 4 times.
- A slice runs out once it would read half a frame or more past its audio
  before the next starts, as where the song plays it slower. Its audio then
  stops 5 ms short of the next slice's start (`GUARD_SECONDS`), or a quarter
  of the way back in a shorter slice, as a transient found a hair after its
  hit starts leaves the start of the hit there, which would click as the
  audio ends and at every turn of a loop. Transient Loop Mode fills the gap:
  Off fades out over up to 10 ms as the audio ends; Forward loops from the
  middle of the slice, or from half a second before its end where that's
  later, crossfading over 10 ms at each seam; and Back-and-Forth turns back
  at the loop's end and its start, crossfading over 10 ms from the reading
  one way to the reading the other, each crossfade centred half of one
  before the slice's last frame, so it never reads the next slice's audio,
  or half of one before the loop's start. Turning on the spot, as it first
  did, kinked a low note, bending it 300 times as sharply as its own wave,
  which clicked dully. A seam or turn is no longer than a quarter of the
  loop each time through, and a loop shorter than 8 ms plays as Off.
- Below 100, Transient Envelope fades each slice from its start, by e over
  its length times 10^((envelope − 60) / 20), and no quicker than in a
  millisecond.
- The last slice fades out where the file's end lands, unless it plays
  through to it.
- A stream holds from the earliest frame the slices read at the block's
  start, or come back to as they loop, and with the loop's set of chunks
  free, from where the next slice starts reading, which can be further on
  than a stream keeps ahead. A read that finds its chunks in memory doesn't
  hint the stream, so two slices crossfading don't pull it back and forth.

**Granular.** A clip in Granular keeps its grains (`granular::Grains`),
placed as the graph compiles, for the song's tempo map, on grid frames, and
kept as centres in the clip's own time, where they stay.

- The grains are centred on a lattice half the Grain Size apart, from the
  clip's own time 0, so splitting or moving a clip leaves its grains where
  they were. With a Grain Size curve, each step is half the size where it
  starts. The lattice runs on past the clip's ends, so the grains that play
  there have their neighbours either side.
- Each grain reads the file from where the warp has the song at its
  centre, at the file's own speed times its transpose and detune, read from
  the clip's curves at its centre or else the warp, resampled as any run
  is. It rises through a raised cosine from the grain before's centre to
  its own and falls to the next's, so two grains play on every frame and
  their gains sum to one.
- With Follow pitch off, Fluctuation moves each grain's centre up to
  0.4 × fluctuation of the way to its nearer neighbour, and where it reads
  up to fluctuation × half its size either way, drawn by SplitMix64 from its
  place on the lattice and a fixed seed, so it's the same every time and in
  every copy, and a split doesn't change it.
- With Follow pitch on, where the graph has the file's pitch marks
  (`granular::PitchMarks`, from [analysis](analysis.md#pitch-marks), which
  `Media::insert_marks` gives it), each run of marks has a chain of grains,
  walked from its first mark: each a whole number N of periods on from the
  one before, N = max(1, round(half the Grain Size / period)), reading from
  the mark nearest where the warp has the song at its centre, as TD-PSOLA
  does, so the periods of the grains that overlap line up. Chains keep two
  grains beyond the clip's ends, and the lattice grains within half a step
  of a chain's span give way to it, with no two grains closer than 0.5 ms
  where they meet. Elsewhere, and until the app has the marks, the grains
  keep to the lattice, unspread.
- Where the song plays the file at its own speed wherever the clip plays,
  and nothing transposes, the clip plays the file as Re-Pitch does, sample
  for sample at the file's rate.
- When the tempo changes, the grains keep their places in the clip's own
  time, so they move with its beats, until the app compiles the graph again
  for the new tempo, as it does for Spectral.
- A stream holds from the earliest frame its grains read in the next tenth
  of a second, so a grain about to start can read before those playing.
  Grains read further apart the longer they are and the further their
  speed is from the warp's, so a clip in Granular gets a stream that holds
  as much more of the file as its grains read apart at once, in steps of
  twice as far (`granular::stream_spread`): none to 0.5 s, and 16 s for
  half-second grains four octaves up with Fluctuation at 100.

**Spectral.** A clip in Spectral plays audio rendered ahead. Its
stretcher, Signalsmith Stretch, or Rubber Band with High quality on
(`musicable-stretch`, [architecture.md](architecture.md#crates)),
remembers what it played, so run live from wherever playback started, it
would render a song position differently each time. `spectral::Plan::new`
plans the clip for the song's tempo map as the graph builds, and
`Media::insert_renders` gives the graph its `Renders`: the plan, and a
`Rendered` slot for each of its segments, which the app fills as they
render ([app.md](app.md#the-render-cache)).

- A plan cuts the file every 4 to 8 seconds: on the strongest transient
  from 4 to 8 seconds after the last cut, the one nearest 6 seconds among
  equals, or else on a lattice 2 seconds apart, so an edit moves few cuts.
  Cuts closer in the song than two crossfades merge. It plans only the
  segments the clip plays, and as far beyond as the resampler reaches.
- Segments are in grid frames, the song's frames at the file's rate. The
  player resamples them to the device's rate, as it resamples files, so a
  new device rate renders nothing again, and plays them under the clip's
  gain and fades.
- Each segment renders from a fresh stretcher, with a fixed seed, so it
  renders the same every time on the same build (`spectral::render`). It
  seeks into the file a block and an interval before its first call's
  input, drops a block of output while the stretcher settles, and then
  makes an interval of output a call, about 30 ms. A call's input runs on
  from the last call's to where the song has the file once the stretcher's
  latencies are allowed for: up to frame ⌊position(T + output latency) +
  input latency⌋ for a call whose output ends on grid frame T.
- Each call takes its pitch from where its output is heard: Transpose,
  Detune and Formants, from the clip's curves or else its warp, so drawn
  automation renders into the segments. Formants sets the stretcher's
  formant factor to the pitch ratio to the power of 1 − formants / 100,
  with its pitch compensation on. The stretcher measures the formants as on
  a voice at 250 Hz (`FORMANT_BASE` in `crates/stretch/src/lib.rs`), as
  Signalsmith Stretch's own guess at a voice's pitch comes to almost
  nothing for some voices, which then come out far quieter.
- The stretcher links each band's phase to the bands four or five along,
  31 to 36 Hz away, except below 1 kHz where the energy dips between them
  to under a quarter of the weaker band's, over each band's lowest of the
  last three spectra (`SPLIT_LONG_STEPS_BELOW`, a change Musicable made to
  the vendored header). A low note's harmonics are about that far apart,
  and linking two of them split each into tones a band apart, which made a
  bass under 50 Hz sound split and beating. The dip tells two harmonics
  apart from one sound spread over both bands, such as a click, and the
  lowest of three spectra keeps a bass's dips through a drum hit over it.
- A segment's key (`SegmentPlan::key`) hashes all its render depends on:
  the plan's and the stretcher's versions, the seed, the rate, the
  channels, its length, where it reads from and how fast, and each call's
  input and pitch. It leaves out where the segment is in the song, so a
  copy of the clip a whole number of frames on, at the same tempo, plays
  the same audio. Frame positions round with 10⁻⁴ of a frame of slack, so
  the copy's arithmetic comes out the same.
- With High quality on, each segment renders instead in one run of a fresh
  Rubber Band stretcher on its R3 engine (`spectral/high_quality.rs`), over
  the stretch of the file it plays and a quarter of a second of the file
  either side. The run's key frames, where the warp's markers and the
  song's tempo changes fall and at the segment's ends, put each frame of
  the file out on the grid frame it plays on. A run renders offline, at one
  setting, where Transpose, Detune, Formants and Envelope hold still over
  it, and otherwise in real time, still ahead of playback, with the
  settings taken every 256 frames of the file and each hop at those
  nearest the frame it's centred on. It's given its input a hop at a time
  (`RubberBand::wants`), as R3 works its ratio out once a call and looks
  at its key frames as each call starts.
- High quality's Formants set R3's formant scale to the pitch ratio to the
  power of −formants / 100, or none where that's 1, and its Envelope
  scales R3's formant envelope, its cepstral cutoff, by envelope / 128,
  which is one of Musicable's changes to the library
  (`crates/stretch/vendor/README.md` lists them). Its key hashes Rubber
  Band's version, the run's span and output, its key frames and its
  settings. R3 puts a stretched attack a little early, the more so the more
  it stretches: about 3 ms at one and a half times as long, and 7 ms at
  twice.
- Two segments crossfade over the 5 ms before the cut between them, with
  equal-gain raised cosines, so the attack after the cut plays at full
  level.
- As playback enters a segment, the player decides how it plays: rendered,
  where its slot is filled and the plan is fresh, which `Plan::is_fresh`
  says by comparing the song's tempo changes up to the clip's end with the
  plan's, and else live. A locate or the loop's jump decides afresh.
- The live stretch is one Signalsmith stretcher, with High quality on too,
  made as the graph is prepared, that runs up to an interval ahead of
  playback and reads its input as Signalsmith's render would, from the
  file in memory or streaming. Where the next segment is ready, the
  player hands over to it at the cut, through the same crossfade.
- When the song's tempo changes, the transport keeps the beat, so the
  playhead's frame moves. The player moves what it was playing by the grid
  frames the playhead moved, so it plays on without a break: segments it
  was playing rendered crossfade out over 5 ms as the live stretch comes
  up, and the live stretch reads on to where the new tempo has the file.
  The app plans the clip again for the new tempo and renders the new
  segments.
- In a graph swap, `adopt` takes over the old player's live stretch and
  how it was playing each segment both plans have, and crossfades out of
  the old plan's segments that the new one hasn't. It keeps those until
  the new player is dropped, off the audio thread.
- Where the song plays the file at its own speed wherever the clip plays,
  and nothing transposes, the plan has no segments (`Plan::is_passthrough`),
  and the clip plays the file as Re-Pitch does, sample for sample at the
  file's rate.
- Playing, the live stretch included, neither locks nor allocates: the
  stretcher sizes its buffers as it's configured, and the RTSan job runs
  the engine's Spectral tests, for what the allocation checker can't see in
  its C++ ([How it's checked](#how-its-checked)).

**Audio files** reach the graph as `media::Source`s, one for each clip, in a
`media::Media`: `compile_with_media(project, meters, media)`. The engine
doesn't read files itself. The app opens them with `musicable-io`'s decoders
and gives the engine an `AudioReader` for each (see
[app.md](app.md#audio-files)).

- `Source::Whole` is a file in memory (`AudioData`), a channel at a time.
  The app loads a file whole when it's up to 2²¹ frames long, about 44 s at
  48 kHz, as the graph builds on the compiler thread.
- `Source::Stream` is a file streaming from disk (`Stream`). A `Streamer`
  runs the disk thread. Each stream keeps two sets of chunks of 8,192
  frames in memory: about a second of the file from where its clip reads
  next, and 0.4 s from where it reads after the loop jumps back, each the
  longer by how far apart its clip's reads can be at once
  (`Streamer::stream_spread`), which is 0 but in Granular. Every
  block, playing or not, the player says where that is (`Stream::hint`), and
  the disk thread loads the most urgent chunks of every stream first.
- Reading a stream copies from chunks that are loaded, taking each with a
  compare-and-swap: no locks, no allocation, no I/O. A chunk that isn't
  loaded yet plays silence and counts in `Stream::missed`.
- With `Streamer::spawn(true)`, reads wait for the disk instead, for
  rendering offline, as the null test and exports do: a render is then the
  same however fast the disk is. A chunk outside both sets, which the disk
  thread won't load, is missed there and then rather than waited for, so a
  render can't hang, and misses it the same every time.
- A file that fails to read plays silence where it failed, and
  `Stream::failed` says so.
- A stream follows one clip, so two clips of the same long file get one
  each. A rebuilt graph gets the same sources (the app keeps them), so a
  stream carries on across swaps.

## Previews

The engine plays a sound on its own, beside the song, for the browser's
previews (`preview.rs`). `EngineHandle::preview` sends it a `Preview`, made
with `Preview::new(id, source, config)` off the audio thread, where it
allocates what it needs, from a `Source` read whole or streaming, and
`EngineHandle::stop_preview` stops it.

- One preview plays at a time, once, from its start, at its file's own
  speed: resampled as clips are, mono in both channels. Another, or
  stopping, fades the one playing out over 128 frames, so it doesn't
  click; the next starts at once.
- The engine adds it to its output where it adds the cue output, after
  the graph, so it plays whether the song does or not, never goes through
  the master, and never reaches an export's taps
  (`Engine::process_with_taps` leaves it out).
- A preview that streams waits until the stream has its first frames in
  memory. Where the disk falls behind after that, it plays silence, as a
  clip does.
- `EngineHandle::set_preview_gain` sets how loud previews play. A change
  ramps over a block; the first block plays at the gain set.
- A preview starts on the next callback, whatever the lead: the engine
  plays previews as the device asks, on top of what it rendered ahead.
- `Stats::preview` says which preview is playing, by the id it was made
  with, and the frame of its file it's at, for the browser's playhead.
- A preview that has finished or faded out goes back through the garbage
  queue, as a replaced graph does, to be dropped off the audio thread. The
  engine takes a new one only when the queue has room for the two it may
  let go of.

## Notes

Notes are `NoteEvent`s, at sample offsets like other events:
`NodeEventKind::Note`, or `Event::Note` from outside the graph. `On` has
the key, the velocity and the MIDI channel a note wire gave it, if one did
(`None` otherwise), `Expression` changes one dimension of the note's
expression (pitch bend in semitones, pressure and timbre from 0 to 1), and
`Off` has the release velocity. Each names its note by `NoteId`, so notes on
the same key can sound at once, each with its own expression, as MPE, VST3
and CLAP allow.

Controllers travel with the notes: `NodeEventKind::Controller` moves a MIDI
controller, such as the mod wheel or the sustain pedal, to a value from 0 to
1, for every note the node plays, or with a channel, for those on it, and
it holds until the next move. It goes
where notes go (`Notes::send_controller`, and `NodeEventKind::is_played`
for either), in order with them, and takes room in a block as a note does.

- `Graph::send_notes(from, to)` sends a node's notes to another, with or
  without an audio connection between them. Before a block renders, each
  node whose notes go somewhere plays them for the block
  (`Node::play_notes`), after the nodes whose notes it gets. It gets those,
  and any from outside the graph, in `ctx.events`, so a MIDI effect between
  a clip player and an instrument can play them on, changed. A route that
  would make a loop is refused.
- `Graph::send_side_notes(from, to)` sends them to the node's side input
  for notes instead, as `NodeEventKind::SideNote` and `SideController`,
  which a node without one passes over: another track's notes coming into
  a MIDI track's monitor ([Inputs from taps](#inputs-from-taps)).
- `Graph::add_note_record(key, node)` keeps what a node plays in each
  block, for the `TapWriter` with the key (`PreparedGraph::record_notes`):
  a MIDI track's notes, for the tracks recording them
  ([Recording](#recording)). Writers with different keys can record the
  same node, which keeps one record for them all.
- A node whose notes go nowhere, such as an instrument, gets the notes sent
  to it with its other events, in `process`.
- An instrument's voice can ring on after its note is let go. A node says
  which notes' voices it ended in the block it last rendered
  (`Node::ended_voices`), for modulators that run a copy for each voice,
  which stop it when the voice ends. CLAP plugins and the built-in
  instruments say when their voices end. Nothing reads it yet.
- A node can send `Notes::CAPACITY` (512) note events in a block. The rest
  are dropped, and counted in `Stats::events_dropped`.
- `Graph::plays_notes(node)` has a node play its notes even with nowhere
  to send them, as a MIDI effect without an instrument after it does, so
  it keeps its time and its record.
- Each node that plays notes, or makes them as it renders, keeps track of
  the notes it has sounding where its notes go, up to `Notes::CAPACITY`,
  and the switches it holds on there, such as the sustain pedal, on no
  channel and on each of the 16. When a swap takes a route away, the node
  ends the notes it had sounding along it, at the start of the next block,
  and lets go of the switches, which would hold them on, so nothing hangs
  where its notes no longer go (`Notes::release_all` does both at once, as
  a switch does). Then the switches it holds on go on along the routes the
  swap adds, so a pedal a track's clips hold down holds whichever way
  their notes go.

**MIDI clips.** `MidiClips` sends the notes of a MIDI track's clips, with
their expression, to the track's first instrument (`Track::instrument`),
through its MIDI effects if it has any ([below](#notes)).

- A clip plays the notes from its `offset` in its own time, for its length.
  A note still sounding when the clip ends stops there.
- A note plays from its start. One the playhead lands in the middle of,
  after a locate or at the loop, doesn't play: notes aren't chased, as in
  Live. When the playhead jumps or the transport stops, the notes sounding
  are let go.
- A note's id is its clip's id and its own (`clips::note_id`). When a
  rebuilt player takes over, it lets go of the notes it doesn't have
  sounding as they are: a note deleted, moved, transposed, or shortened to
  end before the playhead. A note made longer plays on.
- A clip's controller curves move their controllers as it plays, from its
  `offset` for its length. Unlike notes, controllers are chased: wherever
  playback starts or jumps, the player first sends each controller the
  value its last move before there left it at, from any clip, so the song
  sounds as it does played from its start. Before a controller's first
  move, the player leaves it alone.
- Controllers keep their values when a clip ends or the transport stops,
  as MIDI's do, but a switch (64 to 69, such as the sustain pedal) held on
  is let go then, so no notes ring on. A recompiled player sends only what
  its curves hold differently from what the last one sent, so drawing a
  curve while the song plays moves the controller at once.
- At the same moment, a switch a clip lets go of comes first, then
  controllers' moves, then notes ending, then notes starting, with each
  note's expression after its start. So the next clip's pedal holds, and a
  note that starts with the pedal goes down held.

**MIDI input.** `midi::MidiNotes` turns MIDI 1.0 channel messages into
notes, each with an id of its own. A note takes its channel's pitch bend,
in the range RPN 0 sets (2 semitones either way until it does), its
pressure, or its key's own, and its slide (CC 74). It starts with them
where they're away from rest. Taking every channel, it reads MPE: channel
1 is the master of a lower zone of the other 15 until an MPE configuration
message (RPN 6) sets the zones. Members bend 48 semitones until they're told
otherwise, and the master's bend bends every note in its zone on top of the
note's own. Taking one channel, it reads that channel as a keyboard plays
it. All Notes Off and All Sound Off (CC 123, 120) end a channel's notes,
and Reset All Controllers (CC 121) takes its expression back to rest. Other
control changes move controllers for every note (`Played::Controller`),
from any channel it takes: all but slide, the registered and non-registered
parameters and their data entry (6, 38 and 96 to 101), and the channel mode
messages. Reset All Controllers lets go of the switches held on as well. It
knows what each controller holds (`MidiNotes::controllers`), so a recording
can start with them where they are.

A `MidiMonitor`'s notes have ids from 2^63 up, clear of clips' notes. When
a swap gives a track another input, the notes held on the old one end at
the next block. Another track's notes come into its side input with their
own ids, which it gives new ones from the same count, so they can't clash
with its clips' or each other's, and it ends them, and lets go of the
switches they held, when it stops playing its input, as it does a MIDI
input's. The monitor passes the clips' note ends and expression on
whether the clips play or not, since instruments pass over notes they don't
know, so a note started before the monitor came along still ends.

**MIDI effects.** A MIDI track's MIDI effect plugins
(`DeviceKind::is_midi_effect`), before its instrument, play its notes on,
changed: an arpeggiator plays a chord a note at a time. The compiler puts
them in the notes' way, in order: the clips' player plays through the
track's monitor, if it has one, to the first MIDI effect, each plays to
the next, and the last to the instrument, and to the track's modulators,
so an Envelope starts on the notes the instrument gets
([Modulation](#modulation)). Each is in a
`switch::NoteSwitch`, with the device's key, out of the signal: it plays
no sound, and none goes through it.

- The switch plays its device in the notes pass (`Node::play_notes`),
  with the block's notes and parameter changes, and sends on what the
  device plays. The modulation of its parameters comes as the block
  renders, after the notes have played, so the switch keeps it for the
  device's next block (256 at most), which takes it at its first frame.
- Bypassing (`Switch::BYPASS`) passes the notes on unchanged instead, and
  stops the device, which takes its events through `flush`, until it
  comes back afresh (`Node::reset`). Either way, the switch ends the notes
  sounding where it lands, and lets go of the switches held on there, such
  as the sustain pedal, so none hangs: the device's, which it plays no
  more, and those passed on, whose ends it would never hear. What plays
  from there starts from what the controllers that come in hold, as the
  switch keeps them for each channel (`midi::Held`), so a pedal held down
  on the track holds on: bypassed, they go on as they are, and the device
  brought back hears them before the rest of its first part. Where the
  device starts or stops partway through a block, it plays that part of
  the block as a block of its own.
- A deactivated MIDI effect is out of the notes' way, as a deactivated
  device is out of the signal, and its switch (`NoteSwitch::deactivated`)
  stays in the graph, so a plugin stops on the audio thread, and goes on
  taking its parameter changes. One the notes can't reach, after the
  instrument or on a chain without notes, compiles as deactivated:
  commands never put one there (`Project::check_order`,
  `Project::fit_device`), but a file can.
- A plugin's notes get ids of the engine's own, from 2^63 up, so they
  can't clash with the notes played into it ([plugins.md](plugins.md#notes-and-the-transport)).
  A monitor's ids start there too, but a MIDI effect's notes replace the
  notes it gets, so no node gets both.
- Another MIDI track can take what a MIDI effect plays, after it, and what
  reaches the instrument, at Post FX ([Inputs from taps](#inputs-from-taps)).

**Note wires.** A note wire (`Port::Notes`) plays a MIDI track's notes on
an instrument or a MIDI effect on another track: the track's MIDI To. Each
that plays (`Project::live_wires`) gets a `midi::NoteWire` (`keys::wire`),
which the node playing the notes at its tap sends them to, as for a track's
input ([Inputs from taps](#inputs-from-taps)), and which sends them on to
the device's switch. An instrument plays them with its own track's, and a
MIDI effect plays them through itself to the devices after it. A wire into
a deactivated device, or a MIDI effect out of the notes' way, has nothing
to play into, as an audio wire into a deactivated device has nothing to
feed, and the compiler leaves it out. The modulators of a track hear the
notes that reach its first instrument, wired ones too.

- Each note goes on under an id of its own, with the wire's id in it, from
  3 × 2^62 up, so the notes of two wires and those of the track's own
  never clash where they meet.
- With a channel (`Wire::channel`), every note and controller goes on it,
  and a plugin plays it there, as a part of a multi-timbral instrument
  ([plugins.md](plugins.md#notes-and-the-transport)). The built-in
  instruments play every channel alike. Without one, the notes go on with
  none, as a track's own do. Only a note wire gives notes a channel: a
  monitor takes another track's notes on none, as it does a MIDI input's,
  and the notes a MIDI effect plays have none of their own.
- Mute and solo cut a note wire as they cut its track's output
  (`Project::silenced`), through its `NoteWire::MUTE`, which is
  `Gain::MUTE`, so the session sends it with the output stages' gates.
  Cut, it ends the notes sounding through it and lets go of the switches it
  held on, and open again, it sends what the controllers hold, and notes
  play from the next that starts.
- A wire that takes over in a swap carries on with the notes it had
  sounding. On another channel, it lets go of the switches on the old one
  and sends what the controllers hold on the new.

**Notes made while rendering.** A plugin can play notes out as it
renders: an instrument its arpeggios, or a note detector the notes it
hears in its sound (`PluginDevice::makes_notes`: a note output, on a
plugin that isn't a MIDI effect). It has them only once it has rendered,
after the notes pass. `Graph::send_made_notes(from, to)` and
`send_side_made_notes` send them on: the node hands them over
(`Node::made_notes`) once it has rendered the block, and what takes them
plays them in the same block, never one late.

- Such routes put the graph in stages. What takes made notes renders in a
  stage after the node that made them, and every other node in the
  earliest stage its inputs, the notes it gets and its modulators allow.
  Each stage in turn has its players play their notes, its controllers
  run and its nodes render, and then the notes its nodes made go into the
  events of the stages after it (`PreparedGraph::run`). A graph without
  made notes is one stage, as it always was. Both renderers render a
  stage at a time ([Parallel rendering](#parallel-rendering)).
- Made notes can't close a loop: a route that would lead back to the node
  that makes them, by sound, notes or modulation, would have no stage to
  start in, so the graph refuses it, as it refuses any route added later
  that would close one through made notes. The project's flow catches
  the sound and the notes before then, but not modulation, so the
  compiler has a group's or the master's modulators skip the notes made
  by a plugin they move, or by one before it.
- The compiler routes a plugin's made notes from the point after it
  (`Tap::Device`) to a MIDI track's input, a note wire or a note record,
  as it routes what a MIDI effect plays. A node hears that its notes go
  somewhere (`Node::makes_notes`) as the graph is built, so a plugin whose
  notes go nowhere keeps none.
- The switch passes its device's made notes on. A bypassed device makes
  none: as it stops, the notes it made that still sound end, on the frame
  it stops, and the switches it held on, such as the sustain pedal, come
  up, so none hangs. A deactivated one is out of the signal, and makes
  none.
- A plugin keeps its notes by engine time as it renders, so those it made
  in the parts of a block a switch renders apart each land where it
  played them ([plugins.md](plugins.md#notes-and-the-transport)).

**The test instrument.** `nodes::TestInstrument` (`DeviceKind::TestInstrument`)
stands in for instruments until plugin hosting. It plays each note with a
sine at its pitch, bent by its pitch bend, with its second harmonic blended
in by its timbre, and louder with velocity and pressure. A note rises over
2 ms and rings out over 30 ms once let go, and while the sustain pedal is
down, notes let go sound on until it comes up. It adds what it plays to
what comes into it.

## Modulation

A channel's modulators, the Macros, LFOs and Envelopes on a track or the
master (`musicable_model::modulation`), play in a `modulation::Modulation`
node, which `compile()` gives each channel that has modulators or
parameters that modulators move (`keys::modulation`).

**The control pass.** A node that modulates other nodes' parameters says
so with `Graph::modulate(from, to, param)`, which returns the port `from`
sends through, counting from 0 for each node, and tells `to` that its
parameter is modulated (`Node::modulated`). `Graph::control` does the same
for a node with no ports yet. Once the notes have played and before any
node renders the block, the engine calls each such node's `Node::control`,
each after the nodes that modulate it. It gets the block's events there,
in `ctx.events`, instead of in `process`: its parameter changes, the notes
sent to it and the modulation sent to it. It sends what it makes through
its ports (`Controls::send`), which reach their nodes as
`NodeEventKind::Modulation` events among the block's others. A route that
would close a loop of nodes modulating each other is refused. Each port has
room for a value on every 32nd frame of engine time and 17 more in a block,
as a lane does, and the rest are dropped and counted in
`Stats::events_dropped`. The pass runs on the audio thread before any node
renders, so the renderers stay bit-identical with modulation
(`tests/parallel.rs`).

**Ticks.** A `Modulation` node works in ticks: on every 32nd frame of
engine time, as a lane sends values, on the frames of its events, up to 16
more a block, and on the first frame it plays. At each tick it takes the
events up to there, moves its modulators on, works out each routing, and
sends each parameter whose sum has changed its new one, which the
parameter ramps to over the next 32 frames. The ticks fall on the same
frames however the blocks fall, so a project modulates the same at any
block size. What moves a modulator's own parameters, another modulator's
routing or a lane, applies from the tick after, so modulators can move each
other, even round a loop, without the graph having one.

**Routings.** A routing takes one of its modulator's outputs, from 0 to 1,
or from -1 to 1 for an LFO that isn't unipolar, bends it with its curve,
multiplies it by another output on the channel if it has a scale, and by
its amount, and follows it over its lag, as a gentle low pass does: about
two thirds of the way in that time. Its amount is a share of its
parameter's range along the parameter's scale, so the node multiplies it by
the range's length there (`Scale::position` of the top less that of the
bottom): a quarter moves a cutoff a quarter of its octaves. A plugin's
parameter takes the amount in the plugin's own units. The routings to a
parameter go out as one offset, their sum. A parameter event changes a
routing's amount without a new graph (`modulation::amount_id`).

**Across channels.** A modulator reaches its own channel's parameters, a
group's also those of the tracks that feed it, and the master's every
track's (`Project::in_reach`). A routing to another channel's parameter
goes through that channel's node, which relays it: the routing's node sends
what it adds through a port to a relay of the other
(`Modulation::add_relay`), and the node there adds it to its own. So the
master's node runs first, then the groups', outer ones first, then the
tracks'. A routing that's bypassed, out of reach, or to a parameter whose
node isn't in the graph, such as a missing plugin's, moves nothing.

**Readings.** At each tick, a node also sets the readings `compile()` gives
it from the `Meters` (`Modulation::read_output`, `read_target`), for the
tick's engine time: where each
of its modulators' outputs is, and for each parameter of its own channel,
the sum it sends, the routings it relays included, as a share of the
parameter's range, or in a plugin's units. Only one node sets each, so
they aren't raced.

**The modulators.**

- A Macro's controls are its outputs: a knob from 0 to 1, and a button at 0
  or 1. Parameter events set them (`modulation::param_id`), as the app
  sends one for each change, and so do lanes.
- An LFO plays its shape, a sine, a triangle, a saw or a square, bent by
  its skew and started where its phase says, at its rate, or synced, a
  cycle each note value at the song's tempo. Free runs from when its node
  starts. Song locks it to the song position, so it holds still while the
  transport stands still. Note starts it again on notes, as its note
  trigger says, and each time its Trigger rises past half way, then waits
  its delay and fades in.
- An Envelope runs its delay, attack, hold, decay, sustain and release,
  each stage bent by its curve and left out at 0 ms, while its gate is
  open: while notes are held, its Gate Level is above half way, or the
  transport plays with its Transport switch on. One Shot leaves out the
  sustain.
- A note trigger of Auto acts as Always, unless a routing or a lane moves
  the modulator's trigger or gate, when it acts as Never
  (`Modulation::move_trigger`). A MIDI track's node gets the track's notes,
  through its input monitor (`Graph::send_notes`), and keeps track of 64
  held at once.

**Swaps.** A new node carries on from the one it replaces, by modulator and
routing id: each LFO's phase, each envelope's stage and level, each
routing's lag, the notes held and when it last ticked, so recompiling moves
nothing. The parameters it moves keep their modulation until its first
tick sends them every sum, and a parameter that nothing modulates any more
ramps its modulation away to nothing.

## Built-in devices

Musicable's own devices (`DeviceKind::Native`) are nodes in
`crates/engine/src/devices/`, one file for each kind. The model holds what
each kind is (`musicable_model::native`): its parameter table, with each
parameter's key, range, default, unit and scale, and what it does.
`devices::node` makes a device's node from the device as the project has
it, and a sampler's with the sample `Media` has for it, and `compile()`
puts it where the device sits in its chain, as it does a plugin. `devices::BUILT` lists the kinds that have a node, which the
browser offers; a kind without one passes the signal through, so a project
that has one still plays.

**Parameters.** A device's node takes `NodeEventKind::Param` with
`ParamId(n)`, where `n` is the parameter's place in its kind's table, and
the app sends one for each change (`Edit::NativeParam`). The node keeps its
parameters in `devices::Params`, which works each one out for every frame:

- The value set ramps to each new one over 10 ms, as `Smoothed` does
  ([Parameter smoothing](#parameter-smoothing)), and a new node carries on
  the ramps of the node it replaces (`Params::take_over`).
- A parameter the table marks `modulated` takes `NodeEventKind::Modulation`
  without a note: an amount along the parameter's scale
  (`Scale::position`), so in its units on a linear scale and in ratios on a
  log one, added to the value set on every frame from the event on, and
  kept in the parameter's range. It ramps in over 32 frames, from one of
  the modulators' ticks to the next ([Modulation](#modulation)), and before
  the node has rendered a frame it applies at once. A new node keeps the
  modulation of the one it replaces for the parameters that something
  still modulates (`Node::modulated`, `Params::modulated`), and ramps the
  rest away to nothing. Modulation for one note is the instruments' to
  handle.
- A parameter with automation follows it instead of the value set
  ([Automation](#automation)): each is an `automation::Param`, and the
  node's `Node::automate` hands the lane to `Params::automate`, which hands
  it back for a parameter the kind hasn't got. A kind whose node doesn't
  forward it gets its lanes as `Param` events instead, a value every 32
  frames, each ramped to over 10 ms.
- `Params::run` renders a block in spans that end where its events fall,
  and hands each span the values for each of its frames (`Values::frames`),
  and whether a parameter holds one value over the span
  (`Values::steady`), for a fast path. A device that moves a value its own
  way, as the delay fades to a new time and the reverb glides to a new
  size, reads where the value is heading on each frame and its modulation
  apart instead (`Values::target`, `Values::modulations`): the value set,
  without its ramp, or with automation the lane's value on the frame,
  without the glide after a jump (`automation::Param::render_with_targets`).
  An event applies before the span it starts, which gets the event too, for
  a note or a note's modulation. So events on one frame make spans without
  frames, which every kind takes in its stride: a swap sends a node every
  value set on it again, all on the next block's first frame
  ([Graph swaps](#graph-swaps)), and `devices::tests` sends each kind
  such spans.

So a change or a modulation lands on its frame, and a device renders the
same samples however the blocks fall, and on any thread: the parallel
renderer stays bit-identical (`tests/devices.rs` checks it), and so do
blocks of any size (`tests/automation.rs`). That holds only while a fast
path for a steady span gives exactly what working frame by frame would: a
frame on which a switch is off starts the EQ's band or the utility's DC
filter afresh, as a span that's off throughout does, and a band's sections
work in `f64` either way.

**Stopping.** While its device is bypassed or deactivated, a node takes
its events through `Node::flush`, which `Params::flush` applies without
rendering, and `Node::reset` brings it back with its lines, filters and
voices empty and its parameters at their values (`Params::reset`)
([Bypassing devices](#bypassing-devices)).

**The utility** (`devices::Utility`): a DC filter (a one-pole high-pass at
5 Hz), each side's polarity, the width (mid and side, exact at 100%), the
pan (the mixer's pan law) and the gain, in that order, with the mute
fading over the 10 ms ramp. Its tests check each stage exactly.

**The EQ** (`devices::Equalizer`): eight bands of state-variable filters
(Simper's trapezoidal SVF), whose responses equal the analog prototypes'
of the RBJ cookbook under the bilinear transform, matched at each band's
frequency; 24 and 48 dB cuts are Butterworth cascades of two and four
sections. `musicable_model::native::eq::response` works out the design's
response, and the test measures the node's, from its impulse response,
to within 0.001 dB of it from 20 Hz to 20 kHz at 44.1 and 96 kHz. A band
retunes on every frame while its shape, frequency, gain or Q moves, and
fades in and out over 10 ms when it's switched on or off. A sample goes
through all of a band's sections in `f64`, rounded to `f32` only after the
last.

**The compressor** (`devices::Compressor`): feed-forward, in decibels,
after Giannoulis, Massberg and Reiss (JAES, 2012). On each frame it takes
the louder channel's level, its peak or its power averaged over the attack
time, and reads how far to turn it down off the static curve
(`musicable_model::native::compressor::gain_reduction`: the ratio above
the threshold, eased in across the knee by a parabola). A one-pole filter
smooths that amount in dB, with the attack's coefficient while it grows
and the release's while it shrinks, so each is a time constant whatever
the level. Then the output gain, on the compressed signal only, and the
mix with the dry; at 0% the output is the input exactly. The power is
averaged in peak mode too, so the 10 ms fade of a switch to RMS finds it
ready, and a NaN or infinity going in doesn't stick in the detector. The
tests hold levels from -54 to 0 dB and find the node settled on the
designed curve to within 0.001 dB, for both detectors, hard and soft
knees; they follow the reduction frame by frame along the attack's and
the release's exponentials; and RMS reads a sine 3 dB under its peak.

**The key** (`devices::key`) is what the compressor's and the gate's
detectors follow. It's the device's own input, or with Sidechain on and
something wired to side input 0, the sum of what comes into that
([Wires](#wires)), crossfading as Sidechain ramps; with nothing wired, it's
the input whatever Sidechain says, so a device never keys from silence. A
high-pass and a low-pass shape it, each a Butterworth section of the EQ's
state variable filter, 12 dB an octave. At the ends of their ranges, 20 Hz
and 20 kHz, they cut nothing, so they're left out: one fades out over 10
ms as it gets there, and in again as it leaves, so the key doesn't jump.
Listen plays the key in place of the output, crossfading as it ramps.
Where the key is the input untouched and Listen is off, the device runs as
it did before keys. The tests measure the filters' responses against the
EQ's design to within 0.001 dB, check that a side input turns the input
down by its own level and that the key is the input until something's
wired, that Listen plays the key, filtered, and that a filter's fade
doesn't jump, and render the same with a key however the blocks fall.

**The gate** (`devices::Gate`): opens on the frame the louder channel's
peak reaches the threshold, and closes once the peak has stayed more than
`return` dB under it for the hold. Its gain fades in a straight line, from
the floor to 1 over the attack and back over the release; a fade that
starts part of the way takes that part of the time, and open, the signal
passes untouched. A new gate starts open, so it lets a signal through
until there's a quiet spell. Its tests check the hold, the fades, the
floor and the return frame by frame, and that one loud channel opens both.
Its peak is the key's, as the compressor's level is, and its tests check
that a side input opens it on its own peak, that Listen plays the side
input, and that a key low-pass keeps it shut on highs.

**The delay** (`devices::Delay`): a line for each channel, long enough for
the longest time, 5 s, read between frames by cubic (Catmull-Rom)
interpolation, which is exact on a whole frame. A synced time is its
note's length at the tempo where the playhead is (`ProcessContext::tempo`
at the transport's position), and moves on the frame a tempo change plays.
A new time, whether it's set or automated, synced to a new note or tempo,
or switched between the two, fades in over 50 ms from a second read of the
line as the first fades out, a raised cosine, and one that comes during a
fade waits for it to end, so a lane sweeping the time steps it every 50
ms; modulating the time moves both reads at once, which bends the echoes'
pitch. What goes back into the lines goes through the cuts, the EQ's 12 dB
cuts, and saturates softly above full scale only. `delay::repeat_response`
in the model works out what each echo keeps of the last, and the reference
test measures each echo of a click against it, to within 0.001 dB over the
spectrum; the others check the times to the frame, across tempo changes
too, that a new time never clicks, and that full feedback can't run away.

**The reverb** (`devices::Reverb`): a feedback delay network of eight
lines behind a diffuser, much as Geraint Luff builds one in "Let's Write a
Reverb" (ADC 2021). Four steps of short delays, each step mixed by a
Hadamard matrix, smear the low cut and pre-delayed signal into a dense
wash, and the network's lines go round again through a Householder matrix.
Both matrices are orthogonal, so the tail loses only what each line takes
away on its way round: a gain for the decay, the same per second whatever
the line's length, and a one-pole low pass that takes as much again at the
damping frequency. The lines' lengths come to whole frames, and a new
size, set or automated, glides them there by no more than a sixteenth of a
frame a frame, bending the tail's pitch by about a semitone at most. The
tail's level makes up for the size, so rooms of any size ring about as
loud. The reference tests read the decay off the tail's energy decay curve
(Schroeder's backward integral): to within 3% of the decay set in the
octave around 250 Hz, from 0% to 100% size at 44.1 to 96 kHz, and to
within 5% of half of it at the damping frequency. The low cut's effect on
the tail matches its design to within 0.001 dB.

**Instruments** (`devices/instrument.rs`): the synth and the sampler play
the notes in `ctx.events`, each on a voice of its own, from its start until
its sound ends. While the sustain pedal is down (`Controller::SUSTAIN`, from
0.5), a note let go plays on as if held until the pedal comes up. A note
past the device's `voices` takes the place of the voice let go longest ago,
then of one the pedal holds on, or else of the oldest, and that voice fades
out over 5 ms beside the new one, so taking it doesn't click: an instrument
has a slot for each of its up to 32 voices and as many again for voices
fading out, all made with the node. Each note bends on its own (its
`Dimension::PitchBend` expression, in semitones), and modulation for one
note (`Modulation` with its id) moves that note's voice alone, for the
first 48 parameters of the table, added on every frame to the values the
device has. A note's pressure and timbre move nothing yet, and nor do the
other controllers: modulation for one note is how a modulator will bring
pressure and timbre to a parameter. Each voice has an attack, decay,
sustain and release envelope that curves as an analog one does, heading a
little past each stage's end and stopping there, so every stage takes its
time to the frame, and a 12 dB state variable filter (the EQ's SVF) whose
resonance runs from a Q of 0.707 to 25, retuned on every frame its cutoff
moves. A voice renders the same
whatever blocks it falls in: what it keeps from frame to frame, such as
its filter's tuning, is a function of what it's from alone. When a voice's
sound ends, the instrument says so (`Node::ended_voices`), as a CLAP plugin
does, so the engine hears when each note is over. Their tests check each
part against what it's meant to do: the filters' responses against the
analog prototypes' to within 0.001 dB, the envelopes' stages to the frame.

**The synth** (`devices::Synth`): two oscillators and white noise, mixed at
their levels, through the filter (low pass 12 and 24 dB, the 24 a
Butterworth cascade of two sections, high pass and band pass) into the amp
envelope. The triangle, the saw and the square are band-limited with
two-sample PolyBLEPs where the saw and the square jump and PolyBLAMPs where
the triangle turns (Välimäki, Pekonen and Nam; Esqueda, Välimäki and
Bilbao), which makes each wave its ideal wave smoothed by a triangle two
samples wide: the test measures the first 19 harmonics of each at the
ideal's level times sinc²(f/fs) to within 0.001 dB, and what folds back
from above half the rate, for D#6 at 48 kHz, under −95 dB below 5 kHz for
the triangle and −65 dB for the saw and the square. The filter envelope
moves the cutoff by `filter_env` semitones at its peak, and key tracking
by the note's distance from C4, on every frame of each voice. A note glides
from the last note's pitch, in semitones, over `glide` ms. The noise starts
from a seed of each note's own, so a session plays the same each time.

**The sampler** (`devices::Sampler`) plays its `devices::Sample`, which the
app loads whole from the device's file (`NativeDevice::sample`) when it
builds a graph, on the compiler's thread, sharing it among samplers of the
same file, and hands to `compile()` in `Media` by device: a sampler without
one is silent. A voice reads the sample from `start` at a speed set by the
note's distance from `root`, `transpose`, `detune` and its bend, and by the
sample's rate against the engine's, between the sample's frames through a
Kaiser-windowed sinc 32 frames wide; the sinc passes the frames themselves
through exactly where a position falls on one, so a note at the root at
the sample's own rate plays it bit for bit. Reading faster than the
sample's rate, the sinc widens (as the clips' resampler does), so what
wouldn't fit under half the engine's rate is filtered out rather than
folded back; an octave or more up, the voice reads a copy of the sample
made at half its rate, two octaves up one at a quarter, and so on, eight
in all with the sample itself. The tests measure tones read slower and
faster, from samples at 44.1 and 48 kHz, at their levels to within 0.001
dB, with what images or folds back 80 dB down, and each copy's band to
within 0.001 dB, with what's above it 85 dB down. Looping, a note plays on
from `end` back to `loop_start` until its sound ends, crossfading with equal
power over the last `loop_fade` ms of the sample into the material before
`loop_start`, as far as the loop and that material allow, so the jump
doesn't click; settings that move take effect at the next pass. In
"One-Shot" mode a note plays from `start` to `end` whatever its length,
without looping, as drums want. Then the filter, if it's on, and the amp
envelope. Start, end and loop points fall on the sample's frames. A
sample's first two channels play, and a mono one plays on both sides.

**Adding a kind.** Write its node in a file of its own in `devices/`, make
it in `devices::node`, add it to `BUILT`, and forward `Node::automate` to
`Params::automate`. Its parameters are in its table already, and the
format and commands handle them. Test it with `devices::testing::Run`,
which renders a node over a signal in blocks of any length with events on
given frames, under the real-time checks, at a given tempo and with the
transport playing if it follows the song (`Run::with_tempo`,
`Run::playing`), and measure it against what it's meant to do, as the EQ's
test does: a reference test, not a snapshot of what it happens to do. Give
its parameters lanes in `tests/automation.rs` too, which plays them in
blocks of any size.

## Parallel rendering

`ParallelRenderer` runs each block's nodes on a pool of worker threads and
the audio thread together, each node as soon as its inputs are done. It
schedules per node, not per track: a track's devices still run one after
another, but tracks run side by side up to the bus where they meet, even
when they all feed the same groups and return tracks. Its output matches
`ReferenceRenderer`'s bit for bit, since every node sums its inputs in
connection order whichever thread runs it.

```rust
let renderer = ParallelRenderer::new(ParallelOptions::default());
let monitor = renderer.monitor(); // for the control thread
let (engine, handle) = Engine::with_renderer(config, Box::new(renderer));
```

Create and drop the renderer off the audio thread, since that starts and
stops its workers. When one renderer replaces another, drop the old one
first on Windows, so its workers leave their real-time slots to the new
ones (see Threads, below).

**Threads.** By default there is one worker per physical core, minus one for
the audio thread, which works too. On Windows, workers join the MMCSS "Pro
Audio" class at critical priority, as the WASAPI stream's audio thread does
(see [audio.md](audio.md)). Windows gives a process 32 threads in MMCSS
and turns away any more with `ERROR_TOO_MANY_THREADS`. (The ignored test
`mmcss_places` in `parallel/priority.rs` counts them: 32 on Windows 11
25H2.) A thread turned away stays at normal priority, where a shared block
can wait on it, and joining fails quietly. So by default there are no more
than 24 workers on Windows, which leaves places for the audio thread and a
few for audio drivers and plugins. Elsewhere
workers don't raise their priority yet: Linux needs real-time scheduling
rights, and on macOS they should join the audio device's workgroup, which
needs the audio backend.

Workers run nodes with denormals flushed to zero, as the audio thread
does (see [Denormals](#denormals)), so a node's output doesn't depend on
which thread ran it.

**A block.** `Graph::prepare` works out which nodes each node feeds and the
longest path from each node to the end of the graph, so the audio thread
doesn't have to. Then each block:

1. The audio thread publishes the block and wakes the workers. It wakes two,
   each of those wakes two more, and so on, so no thread makes more than two
   wake-up calls, and only as many workers wake as the graph can keep busy.
   A worker rendering ahead starts its blocks the same way, from its own
   place in the tree as well as the audio thread's. The audio thread works
   on such a block only if it comes to help, so the worker counts in its
   place: as many threads work on the block as on one the audio thread
   starts.
2. Threads take nodes without inputs from a shared counter, longest path
   first. A thread that finishes a node counts it off the nodes it feeds. Of
   those that are now ready, it runs the one with the longest path itself
   and puts the rest on its own queue. A thread with nothing to do steals
   from another thread's queue.
3. The block ends when every node has run. The thread that started it keeps
   working until then.

A graph with notes made while rendering ([Notes](#notes)) renders a
stage at a time, each as a block of its own: a stage's nodes without
inputs in it start it, only its own connections count, and whether it's
shared is judged for each of the first four stages apart.

**Between blocks.** A worker with nothing to do spins for
`ParallelOptions::spin` (20 µs by default), then sleeps until the next block
wakes it. A worker that finds the block it woke for over, such as the
last one it started rendering ahead, waits again without waking any
others. The spin covers blocks that follow each other within one device
callback, such as at a loop end. Spinning through the gap between callbacks
would save the wake-ups, but it keeps every core busy (see the benchmarks
below).

**Small blocks.** A block runs on the audio thread alone when the block
before it took less than `ParallelOptions::inline_below` of work (100 µs by
default), or when nothing in the graph can run alongside anything else and
no node split its work into tasks in the block before (see Tasks, below).
Sharing pays from much less work than that on a desktop CPU, about 30 µs on
a Ryzen 9 3900X, but a block under 100 µs takes less than 8% of a 64-frame
period at 48 kHz on its own, and the workers can stay asleep. A new graph's
first block is always shared, to find out. A graph with more than
`ParallelOptions::max_nodes` nodes (16,384 by default) runs on the audio
thread alone too.

**Failures.** Workers are real-time threads: each runs its share of a block
inside `rt::scope`. A panic in a node on any thread, including a real-time
violation, is caught, the rest of the block runs, and the thread that
started the block raises the panic again when it ends: the audio thread,
or a worker rendering ahead, which hands it on to the audio thread.

**Tasks.** A node can split its own work into tasks with
`tasks::run(count, &task)`, as a plugin using CLAP's thread pool splits its
voices. In a shared block, the node's thread puts the tasks where other
threads can take them, wakes up to two sleeping workers, and takes tasks
itself until none are left. Each worker that takes one wakes up to two more
while tasks are left, and threads with no node to run take tasks too. Then
the node's thread waits for the tasks others took, and `run` returns.
Elsewhere, in `ReferenceRenderer` or a block on the audio thread alone, the
tasks run one after another on the node's thread, and the parallel renderer
shares the blocks after it, if they take enough work, even when nothing in
the graph can run alongside anything else. A task that calls `tasks::run`
runs its own tasks itself. Tasks follow the real-time rules and are checked
like nodes, and a panic in one comes out once every task has run.

**Counters.** `ParallelRenderer::monitor` returns a `ParallelMonitor`, which
reads `ParallelStats` from any thread: blocks shared, pipelined and rendered
alone, time spent running nodes, the wall time of shared blocks, and the
workers' time awake, which is the CPU they cost. `ParallelStats::cores_used`
is how many threads were running nodes, on average, during shared blocks.

**Benchmarks.** `cargo bench -p musicable-engine --bench scheduler` times
blocks of wide, deep and project-shaped graphs, with and without filled
channel strip slots, on 1, 2, 4, … threads up to one per physical core, or
up to the number in the `MUSICABLE_BENCH_THREADS` environment variable.
`--bench realtime` renders at a device's pace and prints tables: a 64-track
project scheduled per node against one thread per signal path (how Ableton's
documentation says Live does it), block times and worker CPU for different
spin settings, small graphs alone against shared, block lengths ahead of
the device, and pipelining (below): a 64-device chain, the 64-track
project and four tracks ahead in 1024-frame blocks and as an export in
4096, on one thread, shared whole and pipelined, with whether pipelined
played the same as one thread. On a Ryzen 9 3900X
(12 cores), a 64-track project whose tracks all send to two return tracks
rendered a 64-frame block in 118 µs per node, on 8.8 cores, and in 671 µs
per signal path, on 1: 5.7 times faster. Graphs with plenty to share ran 6.7
to 8.1 times faster on 12 threads than on one, and spinning longer than
20 µs between blocks didn't make blocks any faster.

**Rendering ahead.** With a lead set, the workers also render the engine
ahead of the device between callbacks: a second kind of work, which a
worker takes on when the engine asks and puts down when the audio thread
needs the engine ([Rendering ahead](#rendering-ahead)).

**Pipelining.** A long chain of devices, such as a mastering chain, renders
on one thread when its blocks render whole, however many there are: each
device waits for the one before. So the renderer can pipeline a block
(`ParallelOptions::pipeline`): split it into parts that run down the graph
at once, one device rendering a part as the device after it renders the
part before. Each part of a node is a piece of work of its own, ready once
that part of each of its inputs and its own part before have run, so each
node still renders its parts in order, and the schedule keeps a counter
for each part of each node. The thread that finishes a part goes on with
the node's next part, whose state it has warm, and leaves the parts it made
ready for the nodes after it to other threads.

- **The parts.** Each part is a block of its own to the node
  (`Block::part`): from its frame on, with the events in its frames moved
  to them, the song's position moved on while the transport plays, and the
  device inputs and MIDI messages there (the first part has any that came
  late, and the last any for after the block). A node's output for a part
  is those frames of its buffer, so nothing outside the renderer changes:
  the notes pass, modulation, automation, notes made while rendering and
  the rings all go a block at a time. A node that plays the same whatever
  blocks it renders, as rendering ahead already needs (see [Rendering
  ahead](#rendering-ahead)), plays the same pipelined, sample for sample.
  Audio views (`AudioRef`, `AudioMut`) hold an address and reach only their
  own frames of each channel, not the samples between them, so one thread
  writes a node's next part while another reads the part before.
- **How many.** Whole, a block takes about as long as the longer of its
  longest path and its nodes spread over the threads, counting each node
  alike. A graph whose longest path takes less than half as long again as
  its nodes spread out keeps the threads busy whole, near enough, and
  renders in one part: a 64-track project, or four tracks. Others take
  about twice as many parts as there are threads over how many of their
  nodes can run at once on average (the nodes over the most on any path),
  so twice as many pieces can run at once as there are threads and one that
  falls behind holds up none of the others: up to `PARTS` (16), none
  shorter than `SHORTEST_PART` (128 frames). A 64-device chain's 1024-frame
  block takes 8 parts of 128 frames on 4 threads or more. The nodes' loads
  ([Node timing](#node-timing)) could weigh them, for a project with one
  heavy master chain; that's for tuning on the PC.
- **Which blocks.** `Pipeline::Ahead`, the default, pipelines the blocks
  rendered ahead of the device, which it waits for only once the lead runs
  out. `Pipeline::Always` pipelines every block, as an export does, and
  `Pipeline::Never` none. It never pipelines a live half, which the device
  waits for a block at a time, nor a stage with a node that makes notes as
  it renders, which hands them on once it has rendered the whole block.
  Feedback loops, when the branching design brings them, are to render as
  one node in the pipeline.

In the benchmark's pipelining table on a 4-thread cloud machine, a
64-device chain rendered 3.3 times faster than on one thread ahead and 3.4
times in an export, where whole it rendered no faster, and played the same,
while the 64-track project and four tracks rendered whole as before. The
design's mark is four times on the 3900X's 12 threads.

**Two boards.** The pool has two boards, each with a block of its own, so
a graph's live half renders on the audio thread while workers render the
rest ahead: the main board for what the engine's ahead side renders, and
the live board for the live half, which the renderer's
`Renderer::live_renderer` renders on the same workers. Each board has its
own queues, counters and failure, and a thread works on one at a time.
One block at a time shares each board, and one thread at a time works
from lane 0: a renderer that would start a second block on a board, or a
second thread from lane 0, renders its block alone, so no use of the
renderers can race. While the live side renders a live half (the live
renderer hears of each as the live side installs it,
`Renderer::set_live_half`), a block rendered ahead leaves out the last
two workers, `KEPT`, which never take on rendering ahead either, so
the live half has the audio thread and them however much the rest takes:
all workers but two render ahead, as the [hybrid buffering
design](#rendering-ahead) has it. With three workers or fewer it leaves
out one fewer, so a worker still renders ahead. The live board's wake-up
tree starts from the last worker and goes down, so it wakes those two
first. A worker with nothing to do on the main board works on the live
board's block meanwhile, as the device waits for that, a node or a task
at a time, so it's back on the main board as soon as that has work, and a
thread helps only with the tasks of nodes on its own board. A block
rendered ahead while the live side renders no live half, or one the audio
thread renders as the device asks, has every worker.

A pipelined block takes no more parts than the lanes' queues hold pieces
of work: each lane's holds `ParallelOptions::max_nodes`, and a lane may
push every part of every node of the block, so a graph of a sixteenth of
that or more takes fewer than `PARTS`.

## Rendering ahead

With a lead (`EngineHandle::set_lead`), the engine renders the graph ahead
of the audio device, all but its live half: the live sources, such as an
armed track, and everything they reach, which render as the device asks
(below). Each device callback plays what was rendered ahead, and renders
the live half on top of it. The device waits on a copy and the live half,
whatever the rest of the mix costs, while the workers render the rest
between callbacks in longer blocks, with the lead to spare. So a mix too
heavy for a 64-frame period can still play at one, with a track armed.

```rust
handle.set_lead(Lead::Normal); // Off, the default, Short, Normal or Long
```

**The lead.** `Lead::Short` is about 20 ms, `Normal` about 50 ms and `Long`
about 200 ms, in whole frames (`Lead::frames`). While the transport plays,
the engine renders up to the lead ahead. While it stands still, it keeps
512 frames ahead, so Play starts within about 12 ms. It renders each block
as the device asks for it instead while the lead is off, before the first
graph, while the output or the cue output has more than two channels,
which is all its rings hold, and while anything in the half that renders
ahead is live (below). `Stats::lead` is how far ahead it was at the end of
the last callback, and `Stats::lead_target` how far ahead it aims to be.
`Stats::rendered` is how far it had rendered when the stats were read,
which the threads rendering ahead keep up to date between callbacks.

**Two sides.** `ahead.rs` splits the engine in two. The ahead side holds the
half of the graph that renders ahead, the renderer, the transport and the
events not yet due, and renders blocks as the engine always has. One
thread holds it at a time: a worker rendering ahead, or the audio thread
when it renders a block itself. The live side is the audio thread's own:
the graph's live half, the previews, the record taps' writers, and what it
plays from. Blocks rendered ahead go into rings for the live side to play:
the output and the cue output, each writer's record tap and notes, and the
transport's course, an entry wherever it doesn't run on, from which the
live side reports `Stats::position`, `playing` and `latency` for what it
plays, and gives the live half its transport. Each ring holds the longest
lead and a block, so the ahead side never writes where the live side
reads.

**Two halves.** `Graph::prepare` splits a graph in two: the live half, each
live source (`Node::live`) and each node set live (`Graph::set_live`: a
track set to Always Live, or a plugin that has to render in real time),
with everything they reach through their outputs, their notes, the notes
they make and their modulation, and the half that renders ahead, the rest.
Nodes that share state outside the graph stay on one side
(`Graph::render_together`): a track resampling the master keeps its
monitor and the cut after its devices together, since the cut follows the
monitor's arming a block on. A delay that delay compensation put on a
node's input renders on the node's side, and the whole graph is lined up
before it's split, so both halves have its latency. What crosses from the
half rendering ahead to the live half, sound, notes and modulation, goes
through a queue (`queue.rs`): the ahead side keeps it at its engine time,
and the live half reads it at the same time, so it comes in on the frame
it would have with the graph whole, with no latency added. The events a
node gets on a frame come in the same order too: the graph numbers each
node that plays notes, makes them or sends modulation, in the order they'd
run in the whole graph, and each event carries its sender's number. The
ahead side renders a block only while each queue has room for a block's
notes and modulation. A live block can span several blocks rendered
ahead, with more notes or modulation than a block has room for: what it
hasn't room for waits in the queue for the next live block, late, rather
than go. A graph without live sources is one half, which
renders ahead. `Graph::prepare_whole` keeps any graph whole, for an
engine that renders nothing ahead: with the lead off, every node renders
as the device asks, so a split graph would render its two halves one
after the other, each a block of its own, where a whole one renders them
in the same block, alongside each other, and pipelined together in an
export. The two render the same. `Compiler::compile_whole` prepares a
job's graph that way. With the lead on, a whole graph with a live source
renders nothing ahead. The compiler sets a track's clip player, input monitor,
bus and modulation live when the track is set to Always Live
(`Track::always_live`), so the whole of its strip renders live, its
modulators and macros too, while what feeds a group or a return
set to it renders ahead. It sets a device's switch live, while the device
is in the signal, when the app has it render live (`Devices::set_live`),
as for a plugin that must render in real time
([plugins.md](plugins.md#rendering-ahead)), so what plays into it renders
ahead, and what it plays into live.

**The live half** renders on the audio thread, stretch by stretch as each
callback plays, with the renderer `Renderer::live_renderer` gives: the
parallel renderer's shares it with the workers, on a board of its own
([Parallel rendering](#parallel-rendering)), and the others give a
`ReferenceRenderer`.
It renders frames the ahead side has rendered, from the rings, or, when
the lead runs out, right after the ahead side renders them as the device
asks. The ahead side tells the live side what it needs in a queue of its
own (`ToLive`), in order of engine time: each graph's live half, to install
on the frame the ahead side installed the rest, each tempo map from its
frame, and the events for the live half's nodes. The live side takes them
before each stretch, as far as the ahead side has rendered.

**Who renders ahead.** At the end of each callback, if the lead is short,
the engine asks the renderer's workers for more (`Renderer::render_ahead`).
One worker takes it on, starts each block from its own lane as the audio
thread does from lane 0, and the others help as they would with any block,
until the lead is whole. While the live side renders a live half, the
last two workers stay out of it, for the live half's blocks. A renderer without
workers, such as
`ReferenceRenderer`, has the audio thread render ahead at the end of each
callback instead. When a callback needs frames that aren't rendered yet,
the audio thread takes the ahead side and renders the next block itself,
as the device asks, and then the live half's. If a worker is in a block
then, the audio thread works on that block until it ends, and plays it
first. `Stats::short_blocks` counts the blocks the audio thread rendered
because the lead ran out, which is what a callback that drops out has
been doing.

**Block lengths.** Ahead, the engine renders in blocks of `AHEAD_BLOCK`,
1024 frames, where `EngineConfig::max_block` allows it and the lead is at
least twice as long. Shorter leads take blocks of 512 and 128 frames, and
then the device's period, so a worker is never long in a block the device
waits for. An app that renders ahead makes `max_block` at least
`AHEAD_BLOCK`. The `realtime` benchmark's ahead table (`cargo bench -p
musicable-engine --bench realtime -- ahead`) chose the length: on four
threads of a cloud machine, the 64-track project with sends took 288 ms to
render a second of audio in blocks of 1024 frames, against about 300 ms in
blocks of 256 or 512 and 350 ms in blocks of 64, while a chain of 64
devices took the same at every length, and four tracks the same at every
length from 128 on. ARA's renderers prefer 1024 too.

**What applies when.** What the ahead side has rendered can't be taken
back, so what's sent for as soon as possible applies at the furthest frame
it has rendered: transport commands, tempo maps, graphs, and events sent
with `send`. They're heard up to the lead, and a block, later than with the
lead off. An event sent for a time (`send_at`) lands on its frame, unless
that frame is rendered already: then it applies at the furthest frame, and
counts in `Stats::events_late`. The parameters a swap sends again apply at
the furthest frame too, or at their own time if that's later. Events for
a node in the live half apply as with the lead off: one sent with `send`
on the next frame the live side plays, as the ahead side hands it over as
it takes it in, at the start of each callback, and one sent for a time on
its frame, unless the live side has played it, as the ahead side hands it
over in the block its frame is in.

**Swaps.** Both halves of a graph take over on the same frame, the furthest
the ahead side has rendered: the ahead side installs the half it renders
there, and the live side installs the live half once it has played up to
there. Each node takes over from the node with its key, whichever half that
was in. Each player hands over what it had sounding wherever its notes go
now, in either half ([Notes](#notes)), and as the old graph's halves hand
over one after the other, what one lets go of goes before what's queued
already: a pedal one half lets go of comes up before a pedal the other
holds on goes down, so that one holds. If something that rendered live
renders ahead in the new graph, as when a track is disarmed, the ahead side
waits on that frame until the live side has played up to it and handed it
over, so the lead runs out once, and fills again. Events the live side
still holds for nodes the new live half hasn't go back to the ahead side.

**Live sources.** A node says whether it's a live source now with
`Node::live`: whether it plays what comes in from outside the graph as it
comes, which a block rendered ahead hasn't got. An `InputMonitor` is one
while it can monitor the device inputs, whether the transport lets it now
or not, and until it has faded them out, and a `MidiMonitor` while it can
monitor a MIDI input. A live source renders in the live half. An app that
arms a track sends the event, then builds the graph again with the track
armed, and until that graph comes, the track renders ahead. The graph asks
the nodes that can be live before and after each block. The ahead side
renders no event for one of them in its half, such as arming that track: a
block ahead ends before it, and the audio thread renders the block it's
due in, with the device inputs, so the track arms as it does with the lead
off: an audio track on the frame the event was sent for, a MIDI track from
the next block ([Monitoring inputs](#recording)). From then on the audio
thread renders every block as the device asks, until nothing in the half
rendering ahead is live, as once the new graph has the track in its live
half, and the lead fills again from the end of the callback where that
happens. Blocks rendered ahead have no device inputs or MIDI messages: the
live half gets the messages for every frame it plays, and the half
rendering ahead only in the blocks the audio thread renders itself. A
`MidiMonitor` that renders ahead, which isn't monitoring, misses the rest,
so it doesn't know what the input held meanwhile.

**What the app reads.** A block rendered ahead says so in
`ProcessContext::ahead`, and a meter or a reading keeps the engine time
each value is for ([Built-in devices](#built-in-devices)), so the app's
meters and modulation dots show what the device plays, not what was
rendered last. The app builds the engine with a `max_block` of at least
`AHEAD_BLOCK` ([app.md](app.md#audio)). `NodeLoads::sides` says which nodes
render live, and for each the key of what put it in the live half: its
own, for a live source or a node set live, or else one of the nearest of
those that reach it, a hop for each node between them but none for the
delays, which `Graph::prepare` works out as it finds what's live, from
all the sources at once. The app's Live marks say from it why each track
renders live ([app.md](app.md#cpu-figures)).

**Previews and writers.** Previews play on the live side, from the next
callback whatever the lead, on top of what was rendered. A record tap's
writer gets each frame as it plays, in order: straight from the graph in a
block the audio thread rendered, or from the live half for a tap there,
and from its ring in one rendered ahead. A writer added while the engine
is ahead gets silence for the frames rendered before it came, and one
that replaces a writer with the same key carries on where that one left
off. Notes go through a queue for each writer, and the ahead side renders
a block only while each queue has room for a block's notes
(`Notes::CAPACITY`), so a dense part shortens the lead rather than lose
notes. `Engine::process_with_taps` gets its taps only in blocks it renders
itself, so an offline render leaves the lead off.

**Short loops.** The course queue holds 1,024 entries, a few for each lead,
unless a loop shorter than a block plays, which makes one at each jump.
When it's full, the ahead side stops, and the audio thread renders as the
device asks until there's room.

**Failures and stopping.** A worker renders ahead inside `rt::scope`, as it
renders its share of a block. A panic there is caught, the ahead side
stops, and the audio thread raises the panic at the start of its next
callback, as it would one in a block it rendered. Dropping the engine waits
for any worker rendering ahead to stop.

**Tests.** `ahead/tests.rs` checks each of these, and a proptest checks the
whole: random graphs of probes, delays, monitored audio tracks and MIDI
tracks, some set live, with random callback lengths, leads, events, jumps,
swaps, tracks armed and disarmed as an app does it, tempo maps and MIDI
input, rendered ahead, on workers and on the audio thread, with their live
halves on the audio thread, against an engine with the lead off that
renders each graph whole and replays what both sides of the first applied,
on the frames they applied it (`Applied`), starting a block on a frame as
many times as the first did. Its probes play the same whatever blocks they
come in, as the two engines render different ones, and its MIDI tracks
are given the messages they got the first time. The two play the same, bit
for bit: the output, the writers' frames and notes, the timeline, the swaps
and the events for nodes a graph hasn't got. `graph.rs` checks which nodes
render on which side, and that a graph split in two renders what it
renders whole.

## Node timing

Both renderers time every node, every block, for the CPU figures the app
shows per device and per track ([app.md](app.md#cpu-figures)).
`PreparedGraph::loads` hands the control thread an `Arc<NodeLoads>`: a
`Load` for each node, by its key (`LoadKey::Node`), and for each delay that
delay compensation added, by the key of the node it comes after and the
`Joint` it's at (`LoadKey::Delay`), since a node can have several delays
after it. A `Load` keeps the time the node took, the frames it rendered,
and its longest block since the control thread last took it, in atomics
on a cache line of their own. Blocks rendered ahead are never the longest:
they have only to be ready before the lead runs out, not within the
device's period, and are up to 16 times as long. Only the thread running the node
writes them, with plain loads and stores, so timing costs no locked
instructions, apart from the longest block when it grows.

**The clock** is the cheapest the CPU has: the time-stamp counter on
x86-64, and the virtual counter on ARM64. Its rate is found once, as the
first graph is prepared (`load::ticks_per_second`): on ARM64 the system
keeps it in `CNTFRQ_EL0`, and on x86-64 it's measured against `Instant`
over 10 ms, between two readings of the counter that each take the
closest together of a few pairs of `Instant` readings around it, so that
neither a thread preempted mid-reading nor the computer sleeping skews it.
Other targets, and Miri, read `Instant` itself. On a 4-core Xeon VM a
reading took 8 ns, against 53 ns for `Instant::now` and `elapsed`.

**One reading a node.** A thread reads the clock once as it starts on a
block's nodes, and once after each node, so each node's time runs from the
end of the one before it on that thread, taking it from its queue
included. `Node::play_notes` and `Node::control` are timed on their own
and go towards the node's next block.

**Tasks.** A node's tasks that other threads take count towards the node:
each thread adds the time its task took to the node's load, which the
node's thread published with the tasks. The node's thread's wait for those
tasks counts towards its longest block, since the block lasts as long, but
not towards its time, which is the CPU the node took.

**Graph swaps.** As a new graph takes over on the audio thread
([Graph swaps](#graph-swaps)), each node's load carries on from the node
with the same key in the old graph, and each delay's from the delay at the
same joint, as their state does, so the figures don't jump. A delay at the
same joint after another node, such as the one after a tap when the taps
change, has another `LoadKey`, and starts again. Until a new graph's loads
have all carried on they read nothing, or only some of what they carry on,
so the engine then marks them installed (`NodeLoads::installed`), with a
release store that the control thread's acquire load pairs with, and the
app reads none before.

**What it costs.** The scheduler benchmark's 64-track project with every
channel strip slot filled, 463 nodes, took about 3.7 µs of its 1.40 ms
block to time on that VM, 0.26%, and the scheduler benchmarks didn't move
beyond their noise, so every block is timed. The tests
`each_node_reads_the_time_it_takes` (in `load.rs`) and
`nodes_read_the_time_they_take_on_any_thread` (in `parallel/tests.rs`)
check that a node that spins for a known time reads it within 5%, on each
renderer. Reading less fails at once. Reading more can also be the thread
losing its core to another test, between the meter's clock and the node's,
so a test that reads more tries again, up to three times in all
(`load::tests::timed`): a meter that's wrong is wrong every time.

## Real-time rules

`Node::process`, `Node::adopt`, `Node::play_notes` and `Node::control` run
on the audio thread, or on one of the parallel renderer's workers, which are
real-time threads too, and so do `Node::stop`, `Node::flush` and
`Node::reset`, which a device's switch calls, and everything they call.
They must not:

- allocate or free memory, including growing a `Vec`, cloning a `String` or
  dropping a `Box`;
- lock, or wait on anything: no mutexes, no blocking channel calls, no
  sleeping, no joining threads;
- do I/O: no printing, logging or file access.

Allocate when the node is built or in `Node::prepare`, which runs off the
audio thread and gets the sample rate and block size. In `adopt`, swap
resources with the old node instead of dropping them: the old node is dropped
later, on the control thread. A node can run on a different thread each
block, though never on two at once, so keep no state in thread-locals.

### How it's checked

- **Allocation.** `Engine::process` and `Engine::process_interleaved` run
  inside `rt::scope`. In debug builds with `rt::CheckedAlloc` as the global
  allocator, any allocation or free inside makes the call panic when it ends,
  naming the first one. Run with `RUST_BACKTRACE=1` to see where it happened.
  Every binary and test crate that runs the engine installs the allocator, and
  the tests assert `rt::checker_installed()`, so a missing allocator can't
  quietly turn the check off. CI runs the tests in debug builds, so the check
  is always on there. Release builds compile it out. The parallel renderer's
  workers run their share of each block inside their own `rt::scope`, and a
  violation on a worker fails the block on the audio thread.
- **Locks, sleeping and printing.** An uncontended Rust lock never leaves user
  space, so it can't be caught at run time. Clippy bans the lock types,
  `thread::sleep` and the print macros in the engine crate instead
  (`crates/engine/clippy.toml`), and CI runs Clippy with `-D warnings`.
- **Everything else, with RealtimeSanitizer.** The RTSan workflow
  (`.github/workflows/rtsan.yml`) runs the engine's and the plugins' tests
  on Linux, whenever they change, in a debug build made with
  [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html), nightly
  Rust's `-Zsanitizer=realtime`. There `rt::scope` tells RTSan where
  real-time code runs, and RTSan stops the process at any allocation, lock,
  sleep, system call or I/O inside, from any code: plugins' C and C++ code
  too, and the test plugins, whose allocations go through an allocator of
  their own. It can't see an uncontended Rust lock either.
  What real-time code does on purpose that RTSan would stop at runs in
  `rt::allowed`: waking a sleeping worker or the disk thread, and yielding
  while waiting for one or for a worker rendering ahead, which are system
  calls that don't block, and
  Rust's allocations, which the allocation checker records itself.
  `crates/engine/src/rt.rs` has the command that runs it.

### Denormals

Nodes run with denormal floats flushed to zero, as in every DAW. Denormals
are the floats closer to zero than `f32::MIN_POSITIVE`, and arithmetic on
them is many times slower on most CPUs. A decaying filter or reverb tail
produces them for as long as it rings, so without this a quiet track could
cost more CPU than a loud one.

The renderers run every node inside `rt::flush_denormals`: the reference
renderer on the audio thread, and the parallel renderer on the audio thread
and on every worker, so all of them do the same arithmetic and keep
matching. It sets MXCSR's flush-to-zero and denormals-are-zero bits on
x86-64 and FPCR's flush-to-zero bit on ARM64, and puts the old mode back
afterwards. A constant the compiler works out ahead of time can keep a
denormal that the same sum at run time would flush; that's the same on every
thread, but node code shouldn't rely on denormals either way.

## Limits

| What | Capacity | When it's full |
|---|---|---|
| Message queue | 1024 messages | `send` hands the event back in `QueueFull`, `swap_graph` the graph in `SwapError::QueueFull`, `set_tempo_map` the map in `QueueFull` |
| Pending events | 1024 events | New events are dropped and counted in `Stats::events_dropped` |
| Old graphs and tempo maps waiting | 16 | The engine holds swaps and tempo maps until `collect_garbage` runs |
| Nodes in a graph the parallel renderer shares | `ParallelOptions::max_nodes`, 16,384 by default | The audio thread renders the graph alone |
| Notes a node sends in a block | `Notes::CAPACITY`, 512 | The rest are dropped and counted in `Stats::events_dropped` |
| Modulation a node sends through each port in a block | A value on every 32nd frame of engine time, and 17 more | The rest are dropped and counted in `Stats::events_dropped` |
| Ticks a channel's modulators take on their events' frames | 16 a block, besides the grid | Later events wait for the next tick on the grid |
| Notes a channel's modulators keep track of | 64 held at once | Later notes still start them, but don't hold their gates open |
| Notes sounding in a MIDI clip player | 1,024 | It doesn't start more |
| Clips' notes sounding through a MIDI monitor | 1,024 | It doesn't pass more on |
| Notes the test instrument plays at once | `TestInstrument::VOICES`, 32 | A new note takes the place of the one that started longest ago |
| Latency delay compensation lines up | `Graph::MAX_LATENCY`, 262,144 frames (5.5 s at 48 kHz) | Later paths are lined up as if they were that late |
| Changes in the transport's course rendered ahead | 1,024 | The engine stops rendering ahead until the audio thread plays some |
| Notes rendered ahead for a writer | 1,024, with room kept for `Notes::CAPACITY` | The engine stops rendering ahead until the audio thread plays some |

A swap takes one queue slot, plus one for each remembered parameter of the new
graph. The handle remembers every parameter it has sent until
`forget_params`, which the app calls when it opens a project. All fine so
far; the parameter system gets reworked with plugin hosting.

## Notes for the next milestones

**Audio backend (M2).** Done in `crates/audio`: see [audio.md](audio.md).
These still hold for anything else that drives the engine.

- Render exactly the frames each callback asks for. Drivers don't always
  deliver the period that was requested, so set `max_block` from what the
  device actually delivers. Longer callbacks still work; they're split.
- Count every underrun and overrun with `Engine::record_xrun`. It shows up in
  `Stats::xruns`. Don't hide them.
- A new sample rate or block size means a new engine: create one, compile for
  its config, and swap. Its transport starts at the top of the song, so send
  it the tempo map and loop, and locate it to where the old engine was
  (`TempoMap::beats_at` of the old `Stats::position`).
- Drop the `Engine` off the audio thread, since that frees its graph. For
  example, hand it back to the control thread when the stream stops.

**App shell (M7).** Done in `crates/ui` and `crates/app`: see
[app.md](app.md). `musicable_app::engine` builds every engine the app plays
with the `ParallelRenderer`, and `Session::replace_engine` sets up a new
engine as the notes above describe, when the audio device needs one.

**Tier 1.** Done: [architecture.md](architecture.md#what-tier-1-built)
says where each part is.

**Tier 2.** [architecture.md](architecture.md#where-tier-2-starts) says
where its parts go, in the engine and around it, and what Tier 1 left for
them, such as the room for modulators and console mode.
