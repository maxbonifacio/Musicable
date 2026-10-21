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
 EngineHandle ── message queue: events, swaps ──► Engine
              ◄── garbage queue: old graphs ─────
              ◄── atomics: Stats ────────────────
```

- The **control thread** (the app's main thread) owns the model, a
  `Compiler` and the `EngineHandle`.
- The **compiler thread** turns model snapshots into `PreparedGraph`s:
  `compile()` builds a `Graph` and `Graph::prepare` orders it and allocates
  its buffers. When several snapshots are waiting it compiles only the newest.
- The **audio thread** owns the `Engine` and calls `Engine::process` or
  `Engine::process_interleaved` once per device callback.

Both queues are lock-free single-producer, single-consumer ring buffers
([rtrb](https://docs.rs/rtrb)). Nothing on the audio thread locks, allocates
or frees.

## One device callback

1. The engine takes everything waiting in the message queue. Graph swaps are
   installed (see below) and events go into a store sorted by time.
2. It renders the frames the device asked for, in blocks of at most
   `EngineConfig::max_block` frames. A callback can be any length: longer ones
   are split, and a block also ends early wherever the transport changes.
3. For each block it applies the transport changes that are due, gives each
   node its parameter events with their offsets into the block, runs the
   renderer and copies the output to the device buffer.

## Time and events

**Engine time** counts the frames rendered since the engine started. It never
stops or jumps. `Stats::time` is the engine time at the end of the last
callback. The **song position** (`TransportState::position`) is separate: it
advances only while the transport plays, and `Locate` sets it.

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
- Transport commands are `Play`, `Stop` and `Locate`. Since a block ends where
  the transport changes, `ProcessContext::transport` holds for every frame of
  the block, and while playing, frame `i` is at song position `position + i`.

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

## Real-time rules

`Node::process` and `Node::adopt` run on the audio thread, and so does
everything they call. They must not:

- allocate or free memory, including growing a `Vec`, cloning a `String` or
  dropping a `Box`;
- lock, or wait on anything: no mutexes, no blocking channel calls, no
  sleeping, no joining threads;
- do I/O: no printing, logging or file access.

Allocate when the node is built or in `Node::prepare`, which runs off the
audio thread and gets the sample rate and block size. In `adopt`, swap
resources with the old node instead of dropping them: the old node is dropped
later, on the control thread.

### How it's checked

- **Allocation.** `Engine::process` and `Engine::process_interleaved` run
  inside `rt::scope`. In debug builds with `rt::CheckedAlloc` as the global
  allocator, any allocation or free inside makes the call panic when it ends,
  naming the first one. Run with `RUST_BACKTRACE=1` to see where it happened.
  Every binary and test crate that runs the engine installs the allocator, and
  the tests assert `rt::checker_installed()`, so a missing allocator can't
  quietly turn the check off. CI runs the tests in debug builds, so the check
  is always on there. Release builds compile it out.
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
| Message queue | 1024 messages | `send` hands the event back in `QueueFull`, `swap_graph` the graph in `SwapError::QueueFull` |
| Pending events | 1024 events | New events are dropped and counted in `Stats::events_dropped` |
| Old graphs waiting | 16 graphs | The engine holds swaps until `collect_garbage` runs |

A swap takes one queue slot, plus one for each remembered parameter of the new
graph. Parameter changes are instant, with no smoothing yet, and the handle
never forgets a parameter it has sent. All fine for Tier 0; the parameter
system gets reworked with plugin hosting.

## Notes for the next milestones

**Audio backend (M2).**

- Render exactly the frames each callback asks for. Drivers don't always
  deliver the period that was requested, so set `max_block` from what the
  device actually delivers. Longer callbacks still work; they're split.
- Count every underrun and overrun with `Engine::record_xrun`. It shows up in
  `Stats::xruns`. Don't hide them.
- A new sample rate or block size means a new engine: create one, compile for
  its config, and swap.
- Drop the `Engine` off the audio thread, since that frees its graph. For
  example, hand it back to the control thread when the stream stops.

**Transport and musical time (M3).**

- `TransportState` and `TransportCommand` are a minimal start: play, stop and
  locate, in samples. Tempo, musical position, the loop range and the
  metronome go there.
- Keep one transport state per block. A loop wrapping around has to end the
  block, as `Locate` does.
- The `Sine` test tone takes its phase from the song position, so a position
  renders the same samples however the blocks fall. That's what lets tests
  compare renders bit for bit.

**Parallel scheduler (M4).**

- Implement `Renderer` and pass it to `Engine::with_renderer`. The contract:
  process every node once per block, each after all of its inputs, with the
  same `ProcessContext` and events as `ReferenceRenderer`.
- `PreparedGraph`'s fields are visible inside the crate. Build per-graph
  scheduling data in `Graph::prepare`, so it's done off the audio thread.
- Worker threads are real-time too: run each block's work inside `rt::scope`.
- Nodes sum their inputs in connection order, whatever the schedule, so the
  parallel renderer should match `ReferenceRenderer` bit for bit. Test it
  that way.
