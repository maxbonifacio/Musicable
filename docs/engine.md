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
  render each block (see [Parallel rendering](#parallel-rendering)).
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
- `NodeEventKind::Modulation` offsets a parameter from its value, as a
  modulator does, and the next one for the parameter replaces it. With a
  note id, it offsets the parameter for that note's voice alone, on top of
  the offset for every voice. CLAP plugins get it as parameter modulation,
  per note too. Nothing sends it yet.
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

**Monitoring inputs.** `compile_with_inputs` gives each audio track an
`InputMonitor` after its clip player (key `compile::keys::track_input`),
which plays the track's input in place of its clips: always with
`Monitor::In`, with `Auto` while the track is armed and the transport is
stopped or recording, and never with `Off`, going over in 10 ms. An
`InputLayout` says where each input is among the engine's device inputs,
by the ids the audio backend gave them, and which input the default one is.
A track whose input isn't laid out monitors silence. Arming and monitoring
are the node's parameters (`InputMonitor::ARMED`, `MONITOR`), so changing
them doesn't rebuild the graph. `compile` and `compile_with_devices` lay out
no inputs.

A MIDI track has a `MidiMonitor` instead, with the same key and parameters,
between its clip player and its instrument, on the way the notes go. It
turns the messages its input takes into notes ([MIDI input](#notes)) and
plays them as players expect of an instrument track: always with `In`,
while the track is armed with `Auto`, whether the transport plays or not,
and never with `Off`. The clips play too, except with `In` and while an
armed `Auto` track records over them. The notes sounding end when either
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
  metronome clicks in time with what plays.
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

Two things aren't delayed. Notes sent with `Graph::send_notes` arrive at an
instrument when they're due, so its sound lines up as long as nothing latent
comes before it on its track, which is the usual chain. And meters measure
where they are, before the delays at the buses, so a latent track's meter
moves later than a dry one's.

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

## Clips

An audio or MIDI track's source is its clip player, `clips::AudioClips` or
`clips::MidiClips` (`keys::track_clips`), at the head of its strip, with the
track's test tone feeding it if it has one. Both play from the song
position, like everything else: what a clip plays on a frame depends on
where the frame is in the song, never on how the playhead got there or how
the blocks fall. So clips loop, locate and render the same, bit for bit, in
both renderers.

**Audio clips.** `AudioClips` plays each clip's file from the clip's
`offset` in it, at the file's own speed, for the clip's length.

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
- Tier 1 doesn't warp: a clip plays at its file's speed whatever the tempo.

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
  next, and 0.4 s from where it reads after the loop jumps back. Every
  block, playing or not, the player says where that is (`Stream::hint`), and
  the disk thread loads the most urgent chunks of every stream first.
- Reading a stream copies from chunks that are loaded, taking each with a
  compare-and-swap: no locks, no allocation, no I/O. A chunk that isn't
  loaded yet plays silence and counts in `Stream::missed`.
- With `Streamer::spawn(true)`, reads wait for the disk instead, for
  rendering offline, as the null test does and exporting will: a render is
  then the same however fast the disk is.
- A file that fails to read plays silence where it failed, and
  `Stream::failed` says so.
- A stream follows one clip, so two clips of the same long file get one
  each. A rebuilt graph gets the same sources (the app keeps them), so a
  stream carries on across swaps.

## Notes

Notes are `NoteEvent`s, at sample offsets like other events:
`NodeEventKind::Note`, or `Event::Note` from outside the graph. `On` has
the key and velocity, `Expression` changes one dimension of the note's
expression (pitch bend in semitones, pressure and timbre from 0 to 1), and
`Off` has the release velocity. Each names its note by `NoteId`, so notes on
the same key can sound at once, each with its own expression, as MPE, VST3
and CLAP allow.

Controllers travel with the notes: `NodeEventKind::Controller` moves a MIDI
controller, such as the mod wheel or the sustain pedal, to a value from 0 to
1, for every note the node plays, and it holds until the next move. It goes
where notes go (`Notes::send_controller`, and `NodeEventKind::is_played`
for either), in order with them, and takes room in a block as a note does.

- `Graph::send_notes(from, to)` sends a node's notes to another, with or
  without an audio connection between them. Before a block renders, each
  node whose notes go somewhere plays them for the block
  (`Node::play_notes`), after the nodes whose notes it gets. It gets those,
  and any from outside the graph, in `ctx.events`, so a MIDI effect between
  a clip player and an instrument can play them on, changed. A route that
  would make a loop is refused.
- A node whose notes go nowhere, such as an instrument, gets the notes sent
  to it with its other events, in `process`.
- An instrument's voice can ring on after its note is let go. A node says
  which notes' voices it ended in the block it last rendered
  (`Node::ended_voices`), for modulators that run a copy for each voice,
  which stop it when the voice ends. CLAP plugins and the built-in
  instruments say when their voices end. Nothing reads it yet.
- A node can send `Notes::CAPACITY` (512) note events in a block. The rest
  are dropped, and counted in `Stats::events_dropped`.

**MIDI clips.** `MidiClips` sends the notes of a MIDI track's clips, with
their expression, to the track's first instrument (`Track::instrument`).

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
the next block. The monitor passes the clips' note ends and expression on
whether the clips play or not, since instruments pass over notes they don't
know, so a note started before the monitor came along still ends.

**The test instrument.** `nodes::TestInstrument` (`DeviceKind::TestInstrument`)
stands in for instruments until plugin hosting. It plays each note with a
sine at its pitch, bent by its pitch bend, with its second harmonic blended
in by its timbre, and louder with velocity and pressure. A note rises over
2 ms and rings out over 30 ms once let go, and while the sustain pedal is
down, notes let go sound on until it comes up. It adds what it plays to
what comes into it.

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
  without a note: an amount in the parameter's units, added to the value
  set on every frame from the event on, and kept in the parameter's range.
  It applies at once, without a ramp, since a modulator moves smoothly by
  itself. Modulation for one note is the instruments' to handle.
- `Params::run` renders a block in spans that end where its events fall,
  and hands each span the values for each of its frames (`Values::frames`),
  and whether a parameter holds one value over the span
  (`Values::steady`), for a fast path. An event applies before the span it
  starts, which gets the event too, for a note or a note's modulation.

So a change or a modulation lands on its frame, and a device renders the
same samples however the blocks fall, and on any thread: the parallel
renderer stays bit-identical (`tests/devices.rs` checks it).

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
retunes on every frame while its frequency, gain or Q moves, and fades in
and out over 10 ms when it's switched on or off.

**Instruments** (`devices/instrument.rs`): the synth and the sampler play
the notes in `ctx.events`, each on a voice of its own, from its start until
its sound ends. A note past the device's `voices` takes the place of the
voice let go longest ago, or of the oldest if none is, and that voice fades
out over 5 ms beside the new one, so taking it doesn't click: an instrument
has a slot for each of its up to 32 voices and as many again for voices
fading out, all made with the node. Each note bends on its own (its
`Dimension::PitchBend` expression, in semitones), and modulation for one
note (`Modulation` with its id) moves that note's voice alone, for the
first 48 parameters of the table, added on every frame to the values the
device has. A note's pressure and timbre move nothing yet: modulation for
one note is how a modulator will bring them to a parameter. Controllers
(`NodeEventKind::Controller`) move nothing yet either, the sustain pedal
among them, so a note let go with the pedal down is released all the same.
Each voice has an attack, decay, sustain and release envelope that curves
as an analog one does, heading a little past each stage's end and stopping
there, so every stage takes its time to the frame, and a 12 dB state
variable filter (the EQ's SVF) whose resonance runs from a Q of 0.707 to
25, retuned on every frame its cutoff moves. A voice renders the same
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
it in `devices::node`, and add it to `BUILT`. Its parameters are in its
table already, and the format and commands handle them. Test it with
`devices::testing::Run`, which renders a node over a signal in blocks of
any length with events on given frames, under the real-time checks, and
measure it against what it's meant to do, as the EQ's test does: a
reference test, not a snapshot of what it happens to do.

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
violation, is caught, the rest of the block runs, and the audio thread
raises the panic again when the block ends.

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

`Node::process`, `Node::adopt` and `Node::play_notes` run on the audio
thread, or on one of the parallel renderer's workers, which are real-time
threads too, and so does everything they call. They must not:

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
  while waiting for one, which are system calls that don't block, and
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
| Notes sounding in a MIDI clip player | 1,024 | It doesn't start more |
| Clips' notes sounding through a MIDI monitor | 1,024 | It doesn't pass more on |
| Notes the test instrument plays at once | `TestInstrument::VOICES`, 32 | A new note takes the place of the one that started longest ago |
| Latency delay compensation lines up | `Graph::MAX_LATENCY`, 262,144 frames (5.5 s at 48 kHz) | Later paths are lined up as if they were that late |

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
where plugins, delay compensation, automation and recording go in the
engine, and what's missing for them. The mixer and clips are built (see
[The mixer](#the-mixer) and [Clips](#clips)).
