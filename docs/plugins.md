# Plugins

How Musicable hosts plugins: CLAP so far, in `crates/plugins`. VST3 comes
next on the same pieces, then ARA 2 on top of VST3. Plugin devices in
projects, the device panel, the plugin browser and plugin windows in the
app come with the next part of N5; this page grows with them.

## The pieces

| Piece | Where | What it does |
|---|---|---|
| `Instance` | Main thread | Loads a plugin's library, makes and initializes the plugin, activates it, and carries out everything else CLAP wants done on the main thread |
| `NodeSource`, `ClapNode` | Compiler and audio threads | `Instance::node_source` makes the nodes that process the plugin in the engine's graph |
| `scan` | A process of its own | Lists the plugins in a library, so one that crashes or hangs as it loads takes only the scanner down; `ScanCache` remembers what was found |
| `crates/test-plugins` | Tests | A gain and a synth that check the host keeps CLAP's rules, and libraries that crash, hang or fail to load |

## Threads

CLAP names two threads, and Musicable's are:

- **The main thread** is the thread that made the `Instance`: the app's
  main thread. Everything but processing happens there: creating,
  initializing and destroying the plugin, activating and deactivating it,
  its parameters' names, values and text, its state, its latency, its
  window, its timers, and the callbacks it asks for.
- **The audio thread** is wherever a node processes the plugin: the audio
  thread, or one of the parallel renderer's workers. The host's thread check
  says "audio thread" inside a node's `process` (a thread-local count), not
  for a thread fixed at the start, since the thread processing a plugin can
  change from block to block, and an ASIO driver can make its first call
  from another thread.

A plugin can call some of the host's functions from threads of its own:
asking to restart, for a callback, to flush its parameters, or to resize its
window. Those only set a flag and wake the app (`Waker`), and the main
thread acts on them in `Instance::poll`.

## A plugin's life

The plugin is inactive once it's made and initialized. `activate` gets it
ready for the engine's sample rate and block size, and its nodes start it
processing on their first block. A plugin is only ever in one place at a
time: an instance's nodes and its main thread share a busy flag, and
whoever holds it has the plugin. A node that can't take the flag, or finds
the plugin inactive, failed, or set up for a different sample rate or a
shorter block, plays silence.

- **Activating** reads the plugin's audio and note ports, allocates the
  buffers, and reads its latency, up to `Graph::MAX_LATENCY`. Blocks can be
  any length from 1 frame to the engine's `max_block`.
- **Deactivating** waits for a node that's processing the plugin to finish
  its block, stops the plugin processing, and deactivates it. CLAP lets any
  thread be the audio thread as long as only one is at a time, so the main
  thread stops processing itself, holding the busy flag.
- **Restarting.** A plugin that changes its latency or its ports asks to
  restart. `poll` deactivates and activates it again, and says so with
  `PluginEvent::Restarted`. If the latency changed, the graph needs
  rebuilding, so delay compensation takes the new latency.
- **Failing.** A plugin that fails to start processing is deactivated on
  the next `poll`, which says why.
- **Destroying.** Dropping the `Instance` closes the plugin's window,
  deactivates the plugin and destroys it. A node left over plays silence.
  If a node has been stuck in the plugin's `process` for five seconds, the
  plugin is left alone, and its library stays loaded.

Each library is loaded once per process and unloaded when nothing uses it
any more: every instance and every node holds it.

## Parameters

- **From the app**, `Instance::set_param` queues the change for the
  plugin's next block, at its first frame. An inactive plugin gets it at
  once, through its `flush`. Plugin parameters don't go through the
  engine's message queue: a graph swap there resends every remembered
  parameter, and plugins have thousands.
- **From the graph**, a node's `NodeEventKind::Param` events reach the
  plugin at their offsets, and `NodeEventKind::Modulation` as CLAP's
  parameter modulation, which offsets the value without changing it.
- **From the plugin**, a change someone makes in its window, or one it makes
  itself, comes back from `poll` as `GestureBegin`, `ParamChanged` and
  `GestureEnd`. These become transactions, so undo covers them.

`Instance::params` describes the parameters: id, name, module, range,
default, and whether each is stepped, hidden, read-only, a bypass,
automatable and modulatable. Ids stay the same from one version of a plugin
to the next, so projects store values by id.

## Notes and the transport

Notes go to the plugin's first note input. A plugin that takes CLAP's own
notes gets note ids and per-note expression: pitch bend as tuning, in
semitones, pressure as pressure, and timbre as brightness. One that only
takes MIDI gets note on and note off on channel 1, without per-note
expression until MPE MIDI comes. Up to 512 notes can sound on a plugin at
once.

Every block gives the plugin the transport at its first frame: the tempo,
the position in beats and in seconds, the time signature and where the bar
started, whether it's playing, and the loop.

## State

`save_state` asks the plugin for its state, in its own format, and
`load_state` gives it back. Projects will store it as it is.

## Windows

`open_window` puts the plugin's window inside a native window of Musicable's
(an `HWND` on Windows) at a scale, and `resize_window` asks it to take a
new size. A plugin that can't go inside another window gets one of its own
(`open_floating_window`). When the plugin asks for a new size or to close,
`poll` says so.

## Scanning

The app scans each plugin library in a process of its own
(`scan::scan_isolated`), which calls `scan::child` and prints what it found
on a line of its own, so whatever a plugin prints as it loads can't be taken
for it. A library that crashes the scanner, or is still loading after 30
seconds, counts as bad, as does one that fails to initialize. `ScanCache`
remembers every library scanned, good or bad, with its time and size, so
the next scan only loads new and changed libraries, and a bad one isn't
loaded again until it changes or someone asks. `scan::clap_dirs` has CLAP's
standard places, and the ones in `CLAP_PATH`.

## Limits

| What | Limit | When it's reached |
|---|---|---|
| Parameter changes waiting for a plugin's next block | 1,024 | The rest wait on the main thread, one per parameter, until the next `poll` |
| Events a plugin gets in one block | 4,096 | The rest are dropped |
| Events a plugin sends back between two polls | 4,096 | The plugin is told the queue is full |
| Notes sounding on a plugin | 512 | New notes are dropped |
| Latency | `Graph::MAX_LATENCY` | Delay compensation treats the plugin as that late |
| A plugin's timers | 10 ms or slower | Faster ones fire every 10 ms |
| Loading a library while scanning | 30 seconds | It counts as hung |

## Testing

`crates/test-plugins` builds a CLAP library with two plugins. Test Gain
multiplies by its Gain and delays by its Latency, which it reports, and
asks to restart when the latency changes. Test Synth plays each note as a
sine shaped by its note expression, and can render its voices on the
host's thread pool. Both check which thread the host calls each function
on, and in which state, and count every breach. Tests drive them through
hooks the library exports: turning a knob in the pretend window, changing
state that isn't a parameter, asking for a callback or a resize.

Each test copies the library to a file of its own, so the plugins' counts
are its own. A copy whose name starts with `crash` crashes as it
initializes, one starting with `hang` never finishes, and one starting with
`refuse` fails. The scanner's test runs the test binary itself as the
scanning process (`tests::scan_child`).

The node is checked for allocations like every node. The plugin's own code
isn't: its allocations go through its own allocator. RTSan will check that.

## Not done yet

- Plugin devices in projects, the commands that add, remove and change
  them with undo, the compiler's plugin nodes, and the app's device panel,
  plugin browser and plugin windows: the next part of N5.
- The thread pool runs a plugin's tasks one after another on the thread
  processing it, which CLAP allows. Sharing the renderer's workers is next.
- Audio ports beyond the main ones (sidechains), 64-bit processing, MPE
  MIDI, notes a plugin plays, and letting a quiet plugin sleep.
- VST3, then ARA 2.
- A CI job with [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html).
