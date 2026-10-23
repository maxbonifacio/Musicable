# Plugins

How Musicable hosts plugins, CLAP and VST3, in `crates/plugins`, and how
the app puts them on tracks, in `crates/app`. ARA 2 comes after, on top of
VST3.
[app.md](app.md#plugins) says how plugins look and work in the app's
window.

Most of what follows holds for both formats, in CLAP's words. [VST3](#vst3)
says what's different there.

## The pieces

| Piece | Where | What it does |
|---|---|---|
| `Instance`: a `clap::Instance` or a `vst3::Instance` | Main thread | Loads a plugin's library, makes and initializes the plugin, activates it, and carries out everything else the format wants done on the main thread. `Instance::new` takes the format, and the app holds every plugin as an `Instance` |
| `NodeSource`, `ClapNode`, `Vst3Node` | Compiler and audio threads | `Instance::node_source` makes the nodes that process the plugin in the engine's graph |
| `scan` | A process of its own | Lists the plugins in a library, so one that crashes or hangs as it loads takes only the scanner down; `ScanCache` remembers what was found |
| `crates/test-plugins` | Tests | A gain and a synth, as a CLAP library and a VST3 module, that check the host keeps the formats' rules, and copies that crash, hang or fail to load |
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
  says "audio thread" inside a node's `process` and inside the plugin's
  thread pool tasks (a thread-local count), not for a thread fixed at the
  start, since the thread processing a plugin can change from block to
  block, and an ASIO driver can make its first call from another thread.
- **The thread pool.** A plugin that asks the host to run tasks while it
  processes gets them run on the renderer's threads, as many at once as are
  free, through the engine's `tasks::run` (see
  [engine.md](engine.md#parallel-rendering), Tasks). The thread processing
  the plugin takes tasks too, and waits until they're all done.

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
  parameter modulation, which offsets the value without changing it: for
  every voice, or, with a note id, for that note's voice alone.
- **From the plugin**, a change someone makes in its window, or one it makes
  itself, comes back from `poll` as `GestureBegin`, `ParamChanged` and
  `GestureEnd`. These become transactions, so undo covers the ones made in
  its window ([Plugins on tracks](#plugins-on-tracks)). A plugin
  that no node processes is flushed when it asks, so its changes come back
  without an audio device too.

`Instance::params` describes the parameters: id, name, module, range,
default, and whether each is stepped, hidden, read-only, a bypass,
automatable, modulatable, and modulatable for each note's voice on its own.
Ids stay the same from one version of a plugin to the next, so projects
store values by id.

## Notes and the transport

Notes go to the plugin's first note input, in the first of its note
dialects that Musicable speaks: CLAP's own, then MPE MIDI, then MIDI. Up to
512 notes can sound on a plugin at once.

- **CLAP's own notes** carry note ids and per-note expression: pitch bend
  as tuning, in semitones, pressure as pressure, and timbre as brightness.
- **MPE MIDI** puts each note on a channel of its own, in MPE's lower zone:
  channel 1 is the master, and channels 2 to 16 the members, with each
  note's pitch bend, pressure and timbre on its own channel as pitch bend
  (48 semitones either way, MPE's default), channel pressure and CC 74
  (MPE's slide). As the plugin activates, it hears the zone set up: MPE's
  configuration message for 15 members, then a bend range of 48 on each.
  A note's bend, pressure and slide go before its note on, at rest if the
  note doesn't move them (slide rests at 64), so it never starts with the
  last note's. A new note gets the member channel that's been free
  longest, and with all 15 busy, shares the one used longest ago.
- **MIDI** puts every note on channel 1, bending 2 semitones either way,
  MIDI's default, with pressure as each key's own (poly pressure). Pitch
  bend and slide are the channel's, so the last note to move them wins.
- MIDI notes start at velocity 1 or more, as a note on at 0 is a note off.
  `crate::midi` writes MIDI as the engine reads it from MIDI inputs, so a
  performance recorded from MIDI plays back as it was played.

**Controllers**, such as the mod wheel and the sustain pedal
([engine.md](engine.md#notes)), go as MIDI control changes on channel 1,
MPE's master channel, to a plugin that takes MIDI, whatever its notes go
as. A CLAP plugin that only takes CLAP's own notes doesn't get them, as
CLAP has no event of its own for them. A VST3 plugin gets them as
parameters ([below](#vst3)).

A plugin's voice for a note can ring on well after the note is let go, so
the node keeps track of each voice until the plugin says it ended (CLAP's
note end): up to 1,024 voices, forgetting the oldest past that. Modulation
for a note reaches its voice until then, by the note's id, or by its key
for a plugin that takes MIDI. The node hands back the notes whose voices
ended in each block (`Node::ended_voices`), for modulators that run a copy
for each voice.

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

## VST3

A VST3 module is a `.vst3` bundle, a folder with a binary for each
platform (`Contents/x86_64-win`, `Contents/MacOS`, `Contents/x86_64-linux`
and so on), or on Windows also a single `.vst3` file. It's loaded and
entered once per process, as CLAP libraries are, and its factory makes its
classes. Its audio module classes are its plugins, named by their class
ids: 32 hex digits, written as VST3 writes them on every platform.

- **Two parts.** A VST3 plugin is a component, whose audio processor
  processes, and an edit controller, which has the parameters' names and
  text, and the window. In most plugins the controller is an object of its
  own, and in some one object is both. The instance makes both and
  initializes them,
  connects them so they can talk through the host's messages, and gives the
  controller the component's state. VST3's UI thread is the main thread.
- **Activating** sets the plugin up for the engine's sample rate and block
  size, turns on its main audio buses and its first event input, activates
  it and starts it processing, all on the main thread.
- **Parameters** are normalized, from 0 to 1, whatever they stand for.
  `steps` is VST3's step count, and the module is the unit the parameter is
  in, with its parents', such as `Oscillators/Osc 1`. VST3 has no
  modulation, so nodes drop modulation events. Modulators will send a VST3
  plugin its value plus their modulation as ordinary changes instead,
  which never come back from `poll` (see below), so the project keeps the
  value without the modulation.
- **Changes from the app** go to the controller at once, but VST3's
  processor only takes changes as it processes. So an active plugin gets
  them in its next block, and an inactive one as it's activated, in a block
  of no frames, before it says its latency. A plugin that asked to restart
  for them, as for a new latency, is activated once more, since many only
  take their latency as they're activated. Saving and loading the state
  first make sure changes on their way reach the processor: they wait a
  few blocks for a node to hand them over, then hand them over themselves,
  holding the busy flag. A state saved while the plugin is inactive misses
  the changes since it was deactivated, which the device keeps anyway.
- **Changes from the plugin.** Edits in its window come back from `poll`,
  as for CLAP, and go on to the processor. What the processor reports, such
  as meters, and the graph's automation only go to the controller, for its
  window to show: they don't come back from `poll`.
- **Restarts.** A plugin asks the host to restart it with flags. A new
  latency, new buses, or reloading it whole deactivate and activate it
  again (`Restarted`). New parameter names or ids read the parameters again
  (`ParamsChanged`). New parameter values, and the plugin saying it's
  changed (`setDirty`), are `StateChanged`. A restart asked for while the
  plugin is inactive waits for it to activate, which is the restart.
- **Notes** go to the first event input, with note ids, which notes off
  name. Pitch bend is VST3's tuning expression, which goes from 120
  semitones down to 120 up; pressure is poly pressure, and timbre is
  brightness. The transport is VST3's process context, with the frames
  since the plugin was activated.
- **Controllers.** VST3 has plugins take MIDI controllers as parameters:
  as it activates, the instance asks the plugin's MIDI mapping
  (`IMidiMapping`) which parameter each controller moves, on MIDI's first
  channel of its first event input. A controller's move is then a change of
  that parameter, at its frame, and the controller hears of it too, so the
  plugin's window shows it. It doesn't come back from `poll`, so the
  project keeps the parameter's own value. A plugin without a mapping
  doesn't get controllers. A plugin without note expression doesn't get
  per-note pitch bend: VST3 has no MPE MIDI to fall back on.
- **State** is a `.vstpreset` file: the plugin's class id, the component's
  state, and the controller's if it's an object of its own. Loading checks
  the class id.
- **Windows** only go inside a window of Musicable's: an `HWND` on Windows,
  an `NSView` on macOS, an X11 window on Linux. `open_floating_window`
  fails. The scale goes to the plugin's window, except on macOS, which
  scales windows itself. A window that asks for a new size gets it at once,
  and `poll` says so, for Musicable's to follow.
- **Scanning.** `scan::vst3_dirs` has VST3's standard places, and the ones
  in `VST3_PATH`, which Musicable reads as it reads `CLAP_PATH`.

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
  plugin that isn't installed, or one that fails to load or start, says
  why, in the status bar and on its device, as in "Old Reverb
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
  instead. What a plugin changes on its own goes into the project without
  a step (`Document::follow`): a change outside a gesture while its window
  is closed, as when a MIDI controller turns a knob it learned, or in the
  second after it starts or is given a state or parameters
  (`PLUGIN_SETTLING`), as it takes them in. Some plugins say their state
  changed as they load, or as undo hands them one; were that a step, it
  would take the place of everything left to redo. A plugin that restarts
  with another latency gets a new graph, which compensates for it.
- **Saving.** `Session::saved_project` is the project with a fresh state
  from each plugin that has one, which is what the app writes. So the file
  has everything the plugins keep, and `params` is usually empty there.
- **Another engine**, for another sample rate or buffer size, activates
  every plugin again for it.
- **Automation.** A plugin's parameters can be automated like the mixer's,
  by id ([commands.md](commands.md#automation)). The arrangement offers
  those the plugin calls automatable and neither hides nor keeps
  read-only, with values in its own units: CLAP's range, VST3's normalized
  values. The graph sends the values to the plugin as parameter events
  ([engine.md](engine.md#automation)). Turning an automated parameter in
  the plugin's window overrides its automation, as moving a mixer control
  does, until it's re-enabled; while it follows its automation the project
  keeps the value it was last set to, and the plugin's window shows the
  automation's.

The app scans for plugins in the background as it starts, in CLAP's
places and VST3's ([Scanning](#scanning)). What the last
scan found is kept in `plugins.json` in the config folder, so the plugins
are there at once, and the scan only loads the libraries that are new or
changed since. It runs Musicable's own binary for each, as
`musicable scan-plugin LIBRARY` (`scan::child`).

**Windows.** `PluginWindows` shows a plugin's window when the devices panel
asks. On Windows the plugin's window goes inside a window of Musicable's,
which the main window owns, so it stays in front of the main window and
minimizes with it. It's as big as the plugin says, and follows the plugin
when it asks for another size; it doesn't resize by its border. Elsewhere,
and for a CLAP plugin that can't go inside another window, the plugin opens
a window of its own. A VST3 plugin's window only goes inside another, so
it opens only on Windows so far; elsewhere the app says it can't open yet.
Closing the window, or deleting the device, closes the plugin's window
first, then Musicable's.

## Scanning

The app scans each plugin library in a process of its own
(`scan::scan_isolated`), which calls `scan::child` and prints what it found
on a line of its own, so whatever a plugin prints as it loads can't be taken
for it. A library that crashes the scanner, or is still loading after 30
seconds, counts as bad, as does one that fails to initialize. `ScanCache`
remembers every library scanned, good or bad, with its time and size, so
the next scan only loads new and changed libraries, and a bad one isn't
loaded again until it changes or someone asks. `scan::clap_dirs` has CLAP's
standard places, and the ones in `CLAP_PATH`, and `scan::vst3_dirs` VST3's.
`scan::find_libraries` looks for `.clap` and `.vst3` libraries in them and
the folders below. A `.vst3` bundle is a library everywhere, and so is a
`.clap` one on macOS. Elsewhere CLAP's plugins are files, so a folder named
`.clap`, as Debian installs some plugins in, is searched like any other, as
CLAP asks.

## Limits

| What | Limit | When it's reached |
|---|---|---|
| Parameter changes waiting for a plugin's next block | 1,024 | The rest wait on the main thread, one per parameter, until the next `poll` |
| Parameter changes a VST3 plugin gets in one block | 4,096, for up to 1,024 parameters | The rest are dropped |
| Events a plugin gets in one block | 4,096 | The rest are dropped |
| Parameters a VST3 plugin reports in one block, such as meters | 256 | The rest are dropped. Each keeps its last value, which is what the controller shows |
| Events a plugin sends back between two polls | 4,096 | The plugin is told the queue is full |
| Notes sounding on a plugin | 512 | New notes are dropped |
| Voices the node keeps track of on a CLAP plugin, held or ringing on | 1,024 | The oldest is forgotten: modulation for its note goes nowhere, and its end isn't handed back |
| Latency | `Graph::MAX_LATENCY` | Delay compensation treats the plugin as that late |
| A plugin's timers | 10 ms or slower | Faster ones fire every 10 ms |
| Loading a library while scanning | 30 seconds | It counts as hung |

## Testing

`crates/test-plugins` builds a CLAP library with two plugins. Test Gain
multiplies by its Gain and delays by its Latency, which it reports, and
asks to restart when the latency changes. Test Synth plays each note as a
sine shaped by its note expression, as loud as its Level, which can be
modulated for each note's voice, says when each voice ends, and can render
its voices on the host's thread pool, noting which threads run them. It
takes notes as CLAP's own or as MIDI, holds them with the sustain pedal,
and keeps a log of the MIDI it gets. A copy whose name starts with `mpe`
takes only MPE MIDI, so tests can check that an MPE performance sounds as
it does through CLAP's note expression. Both
check which thread the host calls each function on, and in which state, and
count every breach. Tests drive them through hooks the library exports:
turning a knob in the pretend window, changing state that isn't a
parameter, asking for a callback or a resize.

The library is also a VST3 module with the same two plugins. There, Test
Gain is a component with a controller of its own, which tells the
component about state that isn't a parameter in a message, asks to restart
for a new latency, and has a meter, Peak, which the processor sets. Test
Synth is one object, plays notes with VST3's note expression, and maps
CC 7 to its Level through its MIDI mapping. Both
check VST3's rules, in the same count: what's called on which thread and in
which state, and that the host connects, terminates and lets go of each
object, in order, before the module exits.

Each test copies the library to a file of its own, or makes a VST3 bundle
of its own (`testing::copy_vst3`), so the plugins' counts are its own. A
copy whose name starts with `crash` crashes as it initializes, one starting
with `hang` never finishes, and one starting with `refuse` fails. The
scanner's test runs the test binary itself as the scanning process
(`tests::scan_child`).

The node is checked for allocations like every node. The plugin's own code
is checked by RTSan instead, as its allocations go through its own
allocator: the RTSan workflow runs these tests with it
([engine.md](engine.md#how-its-checked)). So the test plugins keep the
rules too. They read each block's parameter changes and events into
buffers made beforehand, and Test Synth's thread pool tasks tell threads
apart without a thread-local, whose copy for each thread a library loaded
at run time may allocate the first time the thread touches it.

The app's tests use the same plugins, in both formats:
[`crates/app/tests/plugins.rs`](../crates/app/tests/plugins.rs) plays them
through the session, saves and opens them again, undoes changes made in
their windows and redoes past the changes they make on their own, plays a
missing plugin, lines a latent plugin up with a dry
track to the sample, also after its latency changes, and runs the app's
binary as the scanner.
[`crates/app/src/app/plugins_on_tracks.rs`](../crates/app/src/app/plugins_on_tracks.rs)
does it all in the app's window, with the mouse and the keys, and puts a
VST3 plugin on a track and opens its window.

## Not done yet

- A plugin that a node has started processing hands back changes made in
  its window as it processes. If the audio device stops, they wait until
  it plays again, or until the project is saved, which saves the plugin's
  state.
- Audio ports beyond the main ones (sidechains), 64-bit processing, notes
  a plugin plays, and letting a quiet plugin sleep.
- Per-note pitch bend, pressure and timbre for a CLAP plugin that takes
  only plain MIDI, without MPE, and for a VST3 plugin without note
  expression: the first get the channel's, the last none.
- VST3 windows in the app on macOS, which needs an `NSView` of Musicable's
  to put them in, and on Linux, which needs the host's run loop too.
- ARA 2.
