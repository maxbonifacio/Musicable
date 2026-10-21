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
 EngineHandle ── message queue: events, swaps, tempo maps ──► Engine
              ◄── garbage queue: old graphs and tempo maps ──
              ◄── atomics: Stats ────────────────────────────
```

- The **control thread** (the app's main thread) owns the model, a
  `Compiler` and the `EngineHandle`.
- The **compiler thread** (`musicable-compiler`) turns model snapshots into
  `PreparedGraph`s: `compile()` builds a `Graph`, and `Graph::prepare`
  orders it, calls each node's `prepare` and allocates its buffers. When
  several snapshots are waiting it compiles only the newest. A node that
  takes its time to prepare, as a plugin can, holds up this thread only.
- The **audio thread** owns the `Engine` and calls `Engine::process` or
  `Engine::process_interleaved` once per device callback.
- With the `ParallelRenderer`, **worker threads** help the audio thread
  render each block (see [Parallel rendering](#parallel-rendering)).

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
   node its parameter events with their offsets into the block, runs the
   renderer and copies the output to the device buffer, then adds the cue
   output (see below) on top.

## Time and events

**Engine time** counts the frames rendered since the engine started. It never
stops or jumps. `Stats::time` is the engine time at the end of the last
callback. The **song position** (`TransportState::position`) is separate: it
counts samples from the start of the song, advances only while the transport
plays, and jumps on `Locate` and at the loop end. `Stats::position` and
`Stats::playing` report it to the control thread.

- `EngineHandle::send` applies an event as soon as possible: at the start of
  the engine's next `process` call.
- `EngineHandle::send_at(time, ..)` applies it at that engine time, exactly on
  its frame. An event for a time already past applies at the start of the next
  `process` call and counts in `Stats::events_late`.
- Events due at the same time apply in the order they were sent.
- A parameter event finds its node by `NodeKey` in the graph that is running
  when the event is due. If no node has the key, it counts in
  `Stats::events_unmatched`.
- A node gets its events for a block in `ProcessContext::events`, in order,
  each with an `offset` into the block. It applies each from that frame on.
- Transport commands are `Play`, `Stop`, `Locate` and `Loop`. Since a block
  ends where the transport changes and where the loop jumps back,
  `ProcessContext::transport` holds for every frame of the block, and while
  playing, frame `i` is at song position `position + i`.

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

## The mixer

`compile()` builds every track as a channel strip, and the master too, with
built-in slots where native stages sit: the clip stage now, console
modelling in Tier 2.

```text
source → [before device 0] → device 0 → … → [pre-fader] → fader → [post-fader] → [bus feed] → output stage → bus
```

- **Sources.** An audio or MIDI track's source is its test tone, for now. A
  group or return track's source is its bus, where what feeds it sums, along
  with its own test tone if it has one.
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
  (`Project::output_of`). Every bus sums through the `compile::Summing` kind
  the compiler picks, so console mode can add a modelled one; there's only
  `Summing::Plain` so far.
- **Sends** (`keys::send`) are `Gain`s at the send's level, feeding a return
  track's bus. A pre-fader send takes the signal after the pre-fader slot,
  unpanned. A post-fader send takes it after the post-fader slot, so it
  follows the fader, the pan and the clip stage.
- **No loops.** The model only allows routing that can't loop:
  `Project::can_feed` and `can_send`. Groups can feed groups, but not ones
  that feed them; return tracks feed the master and have no sends; sends go
  to return tracks, one per return. `output_of` and `sends_of` leave out
  what breaks the rules, so even a hand-edited file compiles.
- **Devices** (`Track::devices`, `Master::devices`) sit between the source
  and the pre-fader slot, in order, each keyed by its `DeviceId`
  (`keys::device`), so a device keeps its state when it moves. The only kind
  is `DeviceKind::TestGain`, for tests, until plugin hosting.

**Slots.** The slots (`compile::Slot`) belong to the channel, not to its
device list: one in front of each device, then pre-fader, post-fader and bus
feed.

- `native_stage()` in `compile.rs` says what sits in each. So far that's
  the channel's clip stage (`nodes::Clip`), in its post-fader slot, when
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
  it starts fresh.

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

- `Meters` keeps a meter for each track and one for the master.
  `compile_metered(project, meters)` puts them in the graph's output stages;
  call `Meters::follow` first, so new tracks get one. `compile()` is the
  same with meters nobody reads.
- A meter carries on across swaps: the new graph's output stage gets the
  same `Arc<Meter>`. One node measures into each meter, so its readings
  aren't raced.
- The app's `Session` keeps the `Meters` and compiles with them, and the
  mixer reads them every frame (see [app.md](app.md#the-mixer)).

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
2. Threads take nodes without inputs from a shared counter, longest path
   first. A thread that finishes a node counts it off the nodes it feeds. Of
   those that are now ready, it runs the one with the longest path itself
   and puts the rest on its own queue. A thread with nothing to do steals
   from another thread's queue.
3. The block ends when every node has run. The audio thread keeps working
   until then.

**Between blocks.** A worker with nothing to do spins for
`ParallelOptions::spin` (20 µs by default), then sleeps until the next block
wakes it. The spin covers blocks that follow each other within one device
callback, such as at a loop end. Spinning through the gap between callbacks
would save the wake-ups, but it keeps every core busy (see the benchmarks
below).

**Small blocks.** A block runs on the audio thread alone when the block
before it took less than `ParallelOptions::inline_below` of work (100 µs by
default), or when nothing in the graph can run alongside anything else.
Sharing pays from much less work than that on a desktop CPU, about 30 µs on
a Ryzen 9 3900X, but a block under 100 µs takes less than 8% of a 64-frame
period at 48 kHz on its own, and the workers can stay asleep. A new graph's
first block is always shared, to find out. A graph with more than
`ParallelOptions::max_nodes` nodes (16,384 by default) runs on the audio
thread alone too.

**Failures.** Workers are real-time threads: each runs its share of a block
inside `rt::scope`. A panic in a node on any thread, including a real-time
violation, is caught, the rest of the block runs, and the audio thread
raises the panic again when the block ends.

**Counters.** `ParallelRenderer::monitor` returns a `ParallelMonitor`, which
reads `ParallelStats` from any thread: blocks shared and rendered alone, time
spent running nodes, the wall time of shared blocks, and the workers' time
awake, which is the CPU they cost. `ParallelStats::cores_used` is how many
threads were running nodes, on average, during shared blocks.

**Benchmarks.** `cargo bench -p musicable-engine --bench scheduler` times
blocks of wide, deep and project-shaped graphs, with and without filled
channel strip slots, on 1, 2, 4, … threads up to one per physical core, or
up to the number in the `MUSICABLE_BENCH_THREADS` environment variable.
`--bench realtime` renders at a device's pace and prints tables: a 64-track
project scheduled per node against one thread per signal path (how Ableton's
documentation says Live does it), block times and worker CPU for different
spin settings, and small graphs alone against shared. On a Ryzen 9 3900X
(12 cores), a 64-track project whose tracks all send to two return tracks
rendered a 64-frame block in 118 µs per node, on 8.8 cores, and in 671 µs
per signal path, on 1: 5.7 times faster. Graphs with plenty to share ran 6.7
to 8.1 times faster on 12 threads than on one, and spinning longer than
20 µs between blocks didn't make blocks any faster.

**Hybrid buffering (Tier 2).** Tracks that aren't monitored live can render
ahead in larger blocks. The compiler would split their part of the graph
off, the same worker pool would render it between live blocks into
lock-free queues, and source nodes in the live graph would read those. The
pool then needs a second kind of work, which workers take when no live
block needs them and put down when one does.

## Real-time rules

`Node::process` and `Node::adopt` run on the audio thread, or on one of the
parallel renderer's workers, which are real-time threads too, and so does
everything they call. They must not:

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
- **Not covered yet:** allocations and system calls in C or C++ code, such as
  plugins. When plugin hosting lands we can add
  [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html), which catches
  those (but not uncontended Rust locks).

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

**Tier 1.** [architecture.md](architecture.md#where-tier-1-starts) says
where clips, plugins, delay compensation, automation and recording go in the
engine, and what's missing for them. The mixer is built (see
[The mixer](#the-mixer)).
