# The engine

How `crates/engine` turns the project model into sound: which thread does
what, how time and events work, and the rules for code on the audio thread.

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
- The **compiler thread** turns model snapshots into `PreparedGraph`s:
  `compile()` builds a `Graph` and `Graph::prepare` orders it and allocates
  its buffers. When several snapshots are waiting it compiles only the newest.
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
   renderer and copies the output to the device buffer.

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
`compile::keys::METRONOME`, feeding the master for now (it belongs on a cue
output, once there is one). It clicks on every beat of the time signature
while the transport plays, accenting each bar's first beat, and a click rings
for 30 ms even if the playhead jumps or stops. `Project::metronome` sets
whether it starts on; switch it with the `Metronome::ENABLED` parameter, like
any other parameter.

**Setting up from a project.** The graph carries the metronome setting. The
tempo map and the loop are transport state, so send them when a project
opens and whenever they change:

```rust
handle.set_tempo_map(project.tempo.clone())?;
let region = project.looping.then_some(project.loop_region);
handle.send(Event::Transport(TransportCommand::Loop(region)))?;
```

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

## Channel strips

`compile()` builds every track as a channel strip, with built-in slots where
native stages will sit: clip types in Tier 1 and console modelling in Tier 2.

```text
source → [before device 0] → device 0 → … → [pre-fader] → fader → [post-fader] → [bus feed] → bus
```

- The slots (`compile::Slot`) belong to the channel, not to its device list.
  Tracks have no devices yet, so only the pre-fader, post-fader and bus feed
  slots occur.
- An empty slot adds no node, so a strip with every slot empty is exactly the
  graph it was before slots existed. Every slot is empty in Tier 0. Filling
  one, such as turning a clip type on, is an ordinary recompile and swap.
- A stage's key comes from its track id and slot (`compile::keys::track_slot`),
  so it keeps its state across swaps like any other node. Moved to another
  slot, it starts fresh.
- Every bus, including the master, sums through the `compile::Summing` kind
  the compiler picks. Tier 0 only has `Summing::Plain`. Console mode can add a
  modelled one.
- The model has the fields from the start, so project files carry them from
  format version 1: `Track::clip_type` (only `Off` so far) and
  `Track::console`, a placeholder.

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
stops its workers.

**Threads.** By default there is one worker per physical core, minus one for
the audio thread, which works too. Workers join the MMCSS "Pro Audio" class
on Windows. Elsewhere they don't raise their priority yet: Linux needs
real-time scheduling rights, and on macOS they should join the audio device's
workgroup, which needs the audio backend.

Workers run with the default floating-point environment. Nothing sets
flush-to-zero or denormals-are-zero on the audio thread yet. Anything that
does must set the workers the same way, or a node's output would depend on
which thread ran it and stop matching the reference renderer; that needs an
option in `ParallelOptions`.

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

**Small blocks.** Waking workers costs more than it saves when there's little
to do. A block runs on the audio thread alone when the block before it took
less than `ParallelOptions::inline_below` of work (100 µs by default), or
when nothing in the graph can run alongside anything else. A new graph's
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
channel strip slots, on 1, 2, 4, … threads up to one per physical core.
`--bench realtime` renders at a device's pace and prints tables: a 64-track
project scheduled per node against one thread per signal path (how Ableton's
documentation says Live does it), block times and worker CPU for different
spin settings, and small graphs alone against shared. On a 4-core cloud VM,
a 64-track project whose tracks all send to two return tracks rendered 2.3×
faster per node than per signal path, using 3.5 cores instead of 1. The
defaults above come from that VM, so check them on real hardware.

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

## Limits

| What | Capacity | When it's full |
|---|---|---|
| Message queue | 1024 messages | `send` hands the event back in `QueueFull`, `swap_graph` the graph in `SwapError::QueueFull`, `set_tempo_map` the map in `QueueFull` |
| Pending events | 1024 events | New events are dropped and counted in `Stats::events_dropped` |
| Old graphs and tempo maps waiting | 16 | The engine holds swaps and tempo maps until `collect_garbage` runs |
| Nodes in a graph the parallel renderer shares | `ParallelOptions::max_nodes`, 16,384 by default | The audio thread renders the graph alone |

A swap takes one queue slot, plus one for each remembered parameter of the new
graph. Parameter changes are instant, with no smoothing yet, and the handle
never forgets a parameter it has sent. All fine for Tier 0; the parameter
system gets reworked with plugin hosting.

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

**App shell (M7).**

- Play through the `ParallelRenderer` (see Parallel rendering above): build
  the engine with `Engine::with_renderer` before `start`, and keep its
  `ParallelMonitor` for a CPU meter.
- Open and save projects with `musicable_io::project::load` and `save`. Their
  errors say what's wrong in words a user can act on.
- `musicable_app::Session` sends the tempo map and loop when a project opens
  and whenever a transaction changes them (see Musical time above, and
  [commands.md](commands.md)).
- Show the playhead from `Stats::position`, in bars and beats with
  `TempoMap::beats_at` and `TempoMap::bar_at`.
