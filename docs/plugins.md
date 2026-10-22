# Plugins

How Musicable hosts plugins: CLAP so far, in `crates/plugins`, and how the
app puts them on tracks, in `crates/app`. VST3 comes next on the same
pieces, then ARA 2 on top of VST3. [app.md](app.md#plugins) says how
plugins look and work in the app's window.

## The pieces

| Piece | Where | What it does |
|---|---|---|
| `Instance` | Main thread | Loads a plugin's library, makes and initializes the plugin, activates it, and carries out everything else CLAP wants done on the main thread |
| `NodeSource`, `ClapNode` | Compiler and audio threads | `Instance::node_source` makes the nodes that process the plugin in the engine's graph |
| `scan` | A process of its own | Lists the plugins in a library, so one that crashes or hangs as it loads takes only the scanner down; `ScanCache` remembers what was found |
| `crates/test-plugins` | Tests | A gain and a synth that check the host keeps CLAP's rules, and libraries that crash, hang or fail to load |
| `Plugins` | The app's main thread | An instance for each plugin device in the open project, kept in step with it both ways (`crates/app/src/plugins.rs`) |
| `PluginScan` | A thread of the app's | Scans in the background, each library in a process of its own, and keeps what it found (`crates/app/src/plugin_scan.rs`) |
| `PluginWindows` | The app's main thread | The plugins' own windows, and Musicable's windows they go inside (`crates/app/src/plugin_windows.rs`) |

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
  rebuilding, so delay compensation takes the new latency. Activating is
  the restart a plugin asked for until then, so a plugin given another
  latency before it first activates, as a saved project's plugins are,
  doesn't restart again once it's playing.
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
  plugin's next block, at its first frame. A plugin that no node
  processes, inactive or not started yet, as when there's no audio device,
  gets it at once, through its `flush`: on the main thread, which is its
  audio thread while it's active, holding the busy flag. Plugin parameters
  don't go through the engine's message queue: a graph swap there resends
  every remembered parameter, and plugins have thousands.
- **From the graph**, a node's `NodeEventKind::Param` events reach the
  plugin at their offsets, and `NodeEventKind::Modulation` as CLAP's
  parameter modulation, which offsets the value without changing it.
- **From the plugin**, a change someone makes in its window, or one it makes
  itself, comes back from `poll` as `GestureBegin`, `ParamChanged` and
  `GestureEnd`. These become transactions, so undo covers them. A plugin
  that no node processes is flushed when it asks, so its changes come back
  without an audio device too.

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
`load_state` gives it back. A project keeps it as it is, in base64
([project-format.md](project-format.md#track)).

## Windows

`open_window` puts the plugin's window inside a native window of Musicable's
(an `HWND` on Windows) at a scale, and `resize_window` asks it to take a
new size. A plugin that can't go inside another window gets one of its own
(`open_floating_window`). When the plugin asks for a new size or to close,
`poll` says so.

## Plugins on tracks

A plugin device names its plugin by format and id, with the name it had
when it was added, and keeps its settings: the state the plugin last saved,
and the parameters set since ([project-format.md](project-format.md#track)).
The session's `Plugins` holds an `Instance` for each plugin device in the
open project, on a track or on the master:

- **Loading.** A device's plugin loads once the app knows it's installed
  (`Session::set_installed_plugins`, from a scan), and gets the device's
  settings: its state, then the parameters. It's activated for the engine,
  and the compiler builds its nodes into the graph
  (`compile_with_devices`, with a `NodeMaker` for each device). Until it
  loads, and if it never does, the device passes the signal through. A
  plugin that isn't installed, a VST3 plugin, or one that fails to load or
  start says why, in the status bar and on its device, as in "Old Reverb
  isn't installed, or Musicable hasn't found it: it plays nothing", and
  isn't tried again until the next scan.
- **Following the project.** Adding or deleting a device, or undoing that,
  loads or drops its instance: a device that comes back loads afresh, with
  its settings. A change to the settings, from `device.set_param` or undo,
  goes to the plugin as far as it differs from what the plugin has: a new
  state through `load_state`, then the parameters through `set_param`.
- **Following the plugin.** `Session::tick` polls the plugins every frame.
  A parameter someone turns in the plugin's window is set in the project
  in a transaction named after the plugin, such as "Change Test Gain", and
  a whole gesture is one step in the undo history. When the plugin says its
  state changed, as when someone picks a preset in its window, the session
  saves the state into the project, as a step of its own, or at the end of
  the gesture it's in. A plugin without a state has its parameters read
  instead. A plugin that restarts with another latency gets a new graph,
  which compensates for it.
- **Saving.** `Session::saved_project` is the project with a fresh state
  from each plugin that has one, which is what the app writes. So the file
  has everything the plugins keep, and `params` is usually empty there.
- **Another engine**, for another sample rate or buffer size, activates
  every plugin again for it.

The app scans for plugins in the background as it starts. What the last
scan found is kept in `plugins.json` in the config folder, so the plugins
are there at once, and the scan only loads the libraries that are new or
changed since. It runs Musicable's own binary for each, as
`musicable scan-plugin LIBRARY` (`scan::child`).

**Windows.** `PluginWindows` shows a plugin's window when the devices panel
asks. On Windows the plugin's window goes inside a window of Musicable's,
which the main window owns, so it stays in front of the main window and
minimizes with it. It's as big as the plugin says, and follows the plugin
when it asks for another size; it doesn't resize by its border. Elsewhere,
and for a plugin that can't go inside another window, the plugin opens a
window of its own. Closing the window, or deleting the device, closes the
plugin's window first, then Musicable's.

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

The app's tests use the same plugins:
[`crates/app/tests/plugins.rs`](../crates/app/tests/plugins.rs) plays them
through the session, saves and opens them again, undoes changes made in
their windows, plays a missing plugin, lines a latent plugin up with a dry
track to the sample, also after its latency changes, and runs the app's
binary as the scanner.
[`crates/app/src/app/plugins_on_tracks.rs`](../crates/app/src/app/plugins_on_tracks.rs)
does it all in the app's window, with the mouse and the keys.

## Not done yet

- A plugin that a node has started processing hands back changes made in
  its window as it processes. If the audio device stops, they wait until
  it plays again, or until the project is saved, which saves the plugin's
  state.
- The thread pool runs a plugin's tasks one after another on the thread
  processing it, which CLAP allows. Sharing the renderer's workers is next.
- Audio ports beyond the main ones (sidechains), 64-bit processing, MPE
  MIDI, notes a plugin plays, and letting a quiet plugin sleep.
- VST3, then ARA 2.
- A CI job with [RTSan](https://clang.llvm.org/docs/RealtimeSanitizer.html).
