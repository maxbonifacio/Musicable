# Agents

Agents, such as Claude Code, can control Musicable over MCP, the Model
Context Protocol: list and run its commands, and read the project, while
you watch each change happen in the window. Every change an agent makes is
a step in the undo history, named as yours are, so you can undo it like
your own. Agents connect from your computer only, through a pipe only your
account can open. They also get [the audio tools](#the-audio-tools), which
cut, convert and measure audio files, with or without Musicable running.

## Letting agents in

Nothing listens for agents until you let them in. **Options > Agents...**
opens the Settings window's Agents page, where **Let agents control
Musicable** switches it on, and back off. The app keeps the switch in
[settings.toml](../manual/src/customising/settings.md#the-settings-file)
(`[agents] enabled = true`), so it's on at the next start too. The page says
how it's going: off, listening with no agent connected, the agents
connected, or why it can't listen, such as another Musicable listening
already with the same config folder.

While an agent is connected, the status bar says so, with its last change:
"Agent: Move Notes", or "Agent connected" before it makes one. Clicking it
opens the Agents page. Agents keep working while the window is minimized:
the window answers their calls as it does its own frames, which eframe
runs while it's minimized too.

Switching agents off closes their connections. A call still waiting for an
answer then gets none, and the agent hears that Musicable stopped before it
answered.

## Adding Musicable to Claude Code

The Agents page has the command, with the app's own path in it, and a
**Copy** button:

```sh
claude mcp add musicable -- "C:\Users\you\Musicable\musicable.exe" mcp
```

When `MUSICABLE_CONFIG_DIR` picks the app's config folder, the command
passes it on with `-e`, as the bridge finds the app by its config folder.

Other agents' clients start the same program with `mcp` after it:
`musicable mcp` speaks MCP over its standard input and output, and passes
each call on to the Musicable that's running. It starts whether Musicable
runs or not, and while it can't reach Musicable, the tools say so, asking
you to open it and let agents in; it tries again at the next call, so the
agent needn't start over.

## The tools

A few tools cover every command, so a new command reaches agents with no
change to them. The others read what the window shows, but `get_log`, which
the bridge runs itself.

| Tool | Takes | Gives back |
|---|---|---|
| `list_commands` | `filter`?, text the id or the title holds | Each command's id, title, arguments with their types, whether it applies now if it needs no arguments, and its keys |
| `describe_command` | `id` | Its arguments as a JSON Schema, saying what each is for, whether it applies now, its keys, and whether it runs alone |
| `run_command` | `id`, `args`? | Its `result`, such as a new track's id, and `undo`, the name of the step it made, if it made one |
| `run_commands` | `name`, `commands`: each an `id` and `args`? | Each one's result, in `results`, and `undo`: they're one step, called `name` |
| `get_project` | nothing | The file and whether it's saved, the tempo map, the time signatures, the loop, the metronome and punch switches, the tracks with their mixer settings, devices, clips and takes, and their modulators and routings if they have any, the master, the wires, each from its tap to its device's port at its gain, the locators, and which parameters are automated. A track that takes its input from another track's tap has it as `mixer.set_input` takes it, `from` and `tap`, `after` or `resampling`, with the tap's `name`, and one that feeds nothing but its sends has the output `"sends_only"`. A bypassed device has `"bypassed": true`, and a deactivated one `"active": false`. A routing has the names the modulation commands take: `modulator`, `output`, `param`, `amount`, `curve`, `steps` if it's stepped, `scale` if it has one, `lag`, `"bypassed": true` if it is, and `"plays": false` while its parameter is out of its modulator's reach |
| `get_clip` | `clip` | Where it is, and its notes with their expression and its controllers' curves, in the clip's own time, or its audio file, with its warp (its markers, mode, transpose, detune, Slice's settings, Spectral's Formants, High quality and Envelope, and Granular's Follow pitch, Grain Size and Fluctuation) and its curves, by parameter, if it has them. An audio clip's warp has its `transients`, each its `frame`, `strength` and whether you `added` it, once analysis found them, or null |
| `get_device` | `device`, `filter`?, text the parameters' names hold | What it is and where, whether it's bypassed or deactivated, each parameter's number, name, value, range, default and unit (with a `filter`, only those whose names hold it, in any case), and its ports, a Compressor's or a Gate's Sidechain or a plugin's side inputs, by the plugin's ids and names for them, with the wires into each. A plugin's says whether it's `loaded`, the `problem` if it didn't load, whether it `has_window` and whether that's `window_open` |
| `get_modulator` | `modulator` | What it is and where, each parameter's number, name, value, range, default and unit, a Macro's knobs and buttons with their kinds, and its outputs, each with its range |
| `get_automation` | `param`, such as `track 3 volume`, `wire 1` or `modulator 6 param 2` | Its points, in beats and the parameter's units, and whether it's overridden |
| `get_editor` | nothing | What's selected, the selected track, which panel has the focus, the start marker, the grid, what's in sight, the piano roll's clip and selected notes, the Sample Editor's clip, selected warp markers and the parameter its lane shows, and the devices selected in the devices panel, of the selected track's, in the order they play; the piano roll's and the Sample Editor's `in_sight`, and the Sample Editor's `time` selection; which panel the clip editor shows, `clip_editor`; the `track_heights` you set, the tracks showing their `take_lanes`, the `automation_lanes` shown, by track, and what's on the `clipboards`: `clips` (how many and how long), `notes` and `devices`, or null |
| `get_history` | nothing | Each step's name, oldest first, how many are done, and what Undo and Redo would change |
| `get_performance` | nothing | What the project costs the CPU, as the Performance panel lists it, busiest first: each device's share of one core over the last second, `average`, whether it `renders` `ahead` of the device or `live`, and while live its longest block as a share of the buffer, `longest`, by its `device` id, `name` and `chain`, and the same for each channel's `clips`, `modulators` and `mixer`, and the `metronome`, in `rows`; the project's `total`; the transport bar's figures: whether an audio device is `playing`, the `audio` it plays, the audio thread's `load`, and the `dropouts`; and while Render Ahead is on, `render_ahead`: its `setting`, whether the project renders `ahead` now, how far ahead it was lately and aims to be, `lead_ms` and `target_ms`, the share of a core of what rendered ahead, `load`, and the blocks the audio thread had to render itself since the device started, `short_blocks`. Without an audio device, there are no rows, and `render_ahead` is null, as it is for the quarter of a second after the device starts or Render Ahead changes |
| `get_status` | nothing | What plays: whether the transport is `playing`, from the moment Play is pressed, and `recording`, the `playhead` in beats and `playhead_seconds`, and the `takes` being recorded, each a `track` with the `start` and `end` it covers so far; the `question` the window waits for you to answer, such as whether to save your changes, or null; and the work that lands later, with nothing to wait on: the audio files being analysed for warping, `analysing`, by their names in the project, the `rendering_segments` left, whether a graph is `compiling`, the `export` under way, with its `stage` (`preparing`, `rendering`, `normalising`), `pass` of `passes`, the share `done` and while it renders its `speed`, times real time, `scanning_plugins`, and the `library`'s folders `scanning`, with the files `found`, and `reading`, with the files `left`, and whether it's `finding_presets`. `idle` is true once none of that work is under way; `wait_for` waits until it is. `revision` goes up with every change to the project, undone and redone ones too, and when another project opens |
| `wait_for` | `until`, with `timeout` and `since` | Waits until what lands later has, reading the status every tenth of a second, then answers with `met`, the `waited_seconds` and the `status` as `get_status` reads it. `until` is `idle`; `stopped`, the transport not playing; `recorded`, nothing recording and the takes in the project; `analysed`, no audio file being analysed; `scanned`, no plugin scan and the library neither scanning, reading nor finding presets; `rendered`, no segments left to render and no graph compiling; `changed`, the project's `revision` other than `since`, or than when the wait began, as when you edit it; or `answered`, no `question` waiting for you. The status has to say so twice in a row, so work the window starts on the frame after a command, such as analysing the file it brought in, has started. `timeout` is in seconds, 60 unless it says, up to 600; when it runs out, `met` is false. With 0, it reads the status once. Cancelling the call stops the wait |
| `get_messages` | `clear`? | The status bar's `messages`, oldest first: errors and notices, such as a missing file or plugin, a device that failed, or an export's problems. With `clear`, it clears them too, as **Dismiss** does, so the next call gives only those that came after |
| `get_settings` | nothing | The `settings` as `settings.toml` has them, by section, with the profile in use's `behaviour`, `appearance` and `workspaces`; the keys `options.set` can change, in `changeable`; the `profile` in use; the settings `file`; what's `playing`: its `device` id and `name`, `driver_type` (`system` or `asio`), `api`, `sample_rate`, `buffer`, `latency_ms` and `inputs`, or null; and the `recording_offset_frames` of the device playing |
| `get_plugins` | `filter`?, text the name, the vendor or the id holds | The `plugins` the browser lists, for `device.add_plugin`: each its `format` (`clap` or `vst3`), `id`, `name`, `vendor`, `kind` (`instrument`, `midi_effect` or `audio_effect`) and `render_ahead` (`auto`, or `never` if you set it to render live); how many are `installed`; and whether they're `scanning` |
| `get_layout` | nothing | Each panel's `id` and `title`, in the View menu's order, the `window` it's in, `main` or a window of its own from 1, whether it's `hidden` and whether it's `in_sight`, rather than hidden or behind another tab; each window of its own's place (`at`, `size`, `maximized`) and `panels`; the `monitors`; and the `workspace` and `profile` in use. The `view.toggle_*` commands show and hide the panels |
| `get_windows` | nothing | Musicable's windows: the main window and the windows of their own that panels moved to, each with its `window`, `main` or its number from 1, its `title`, where it is (`at`) and how big (`size`), in pixels on the desktop, whether it's `minimized` or `focused`, and the `panels` in sight in it; the `dialogs` open over the main window: `Settings`, `Export Audio` and `Save Changes`; and the `plugins`' windows that are open, each with its `device`, its `title` and whether `get_screenshot` `can_screenshot` it |
| `get_screenshot` | `window`?, `panel`?, `device`?, a plugin device's id, `file`?, a full path to a .png file | A picture of a window, of a panel in it, or of a plugin device's window, as it's drawn, whatever covers it: the `window` and the `panel`, or the `device` and its window's `title`, and the `size` in pixels, then the picture as a PNG image, which the agent sees. With `file`, it's saved there too. egui takes the main window's pictures as it draws it. Windows.Graphics.Capture takes the windows of their own and plugins' windows, by their handles, so only on Windows so far; a plugin that opened a window of its own, rather than inside Musicable's, can't be taken. A minimized window can't be taken, and a panel only while it's in sight. Pictures of plugins' windows stay on the PC: never commit them |
| `get_ui_tree` | `window`?, `panel`?, `filter`?, `limit`? (300, at most 5000) | What's in a window, the main one unless `window` says, as a screen reader reads it: egui's AccessKit tree as of the window's next frame, nested. Each node has its `id` (hex, the same from frame to frame), `role` (`button`, `slider`, `check_box`, ...), and where given its `label`, `value`, `description`, `number` and `range`, `toggled`, `selected`, `expanded`, `disabled`, `focused`, `actions` (`click`, `increment`, `decrement`, `set_value`, `expand`, `collapse`, `context_menu`) and `at` and `size` in pixels in the window, as `get_screenshot`'s pictures. Plain containers, unnamed click areas and text runs are left out, their insides moved up. `panel` keeps what's in a panel's rect; `filter` keeps nodes whose label, value or description has the words, with all inside them and what they're in; past `limit`, `more` counts what's left out. Asking turns AccessKit on until the window is drawn, unless a screen reader has it on. Built in `crates/app/src/ui_tree.rs`, an egui plugin that reads each pass's output |
| `use_mouse_and_keys` | `window`?, `steps`, `watch`? (`panel`?, `filter`?, `limit`?) | Uses the mouse and keys in a window, only while **Let agents use the mouse and keys** is on (`settings.agents.mouse_and_keys`, which `options.set` can't reach). Steps, each an object named by its key: `move` (`seconds`?), `down`, `up`, `click` (`button`?, `count`? up to 3, `interval`? 0.1), `drag` (`to`, `button`?, `seconds`? 0.3), `hover` (`seconds`? 1; the doc's `view.hover`), `scroll` ([across, down] pixels, `at`?), `key` (a chord, as the keymap writes them), `hold` and `release` (modifiers), `text`, `drop` (full paths, `at`), `wait`; at most 200 steps and 120 seconds. Places are a node's hex id from `get_ui_tree`, its middle as of the last frame, or [x, y] pixels in the window. `crates/app/src/agent_hands.rs`, an egui plugin, turns them into egui events frame by frame; the main window's go in through the app's raw input hook first, so the keymap sees them. The user's own pointer, button, key, wheel or file input in any of Musicable's windows (not raw mouse motion) stops it, letting go of what it held, and the next call is refused until they've left it alone for 3 seconds. The answer has the `steps` done and `seconds` taken, and with `watch`, the `frames` whose tree changed, each with its `time` from the start: frames during a gesture |
| `play_midi` | `events` | Plays MIDI into Musicable Agent MIDI, a MIDI input agents have (`musicable_audio::play_agent_midi`, [audio.md](audio.md#midi-inputs)), at their times from now, and answers with the `messages` and `seconds` once the last has played. Events, each named by its key: `note` (`length`, `velocity`? 100, `release`? 64), `cc` (`value`), `bend` (-1 to 1), `pressure` (0 to 1; with `for`, a note's), `program`; each with `at`? seconds and `channel`? 1 to 16. MPE gives each note a channel of its own. At most 4000 events and 120 seconds. A MIDI track takes the input with `mixer.set_input` (`device` "Musicable Agent MIDI"); it plays only while the audio device does, on Windows |
| `get_levels` | nothing | The meters, as the mixer shows them: the `master`'s and each of the `tracks`', by `track` id and `name`: `peak` and `rms`, left and right, and the `highest` sample since the peaks were cleared, in dBFS to a hundredth, null for silence; `playing`, whether an audio device plays, and `moving`, whether a meter shows a signal; each modulator's outputs, in `modulators`, by `modulator`, `output` and `value`; and how far routings move each parameter they move, in `moved`, by `param` and `by`. The meters move only while an audio device plays, and fall back for a few seconds after the music stops |
| `get_command_log` | `since`?, `last`? | The commands run this session, oldest first: yours, from the menus, keys and panels, and agents'. Each entry has its number `n`, its `time` in UTC, `by`, `user` or the agent's name, and the `command` and its `args`, or for `run_commands` the `batch`'s name and its `commands`, and the `error` if it failed. It gives the last 100 unless `last` says, up to 10,000, and with `since`, only the entries after that number. `count` says how many commands have run, and `file` is the session's log in the `command logs` folder of the config folder, one entry a line, which keeps the latest 20 sessions |
| `list_audio_devices` | nothing | Lists the devices again: the `driver_types` this build plays through, the `outputs`, each with its `id`, `name`, `driver_type`, whether it's the system's `default` and whether it's `playing`, for `options.set_audio_device`, and the audio `inputs`, with their `channels`, and `midi_inputs`, by the ids `mixer.set_input` takes |
| `get_log` | `lines`? (100, at most 1000), `level`? (`error`, `warn` or `info`), `since`?, a time in UTC such as `2026-10-31T05:00:00Z` | The latest `entries` of `musicable.log` and `musicable.old.log` in the config folder, oldest first, those as bad as `level` or worse, logged at `since` or after: each its `time`, `level`, `target`, such as `wgpu_hal` or `panic`, and `message`, a panic's over several lines with its calls; the `file`; and whether `earlier` entries were left out. The bridge reads the file itself, so it works while Musicable is closed, and after it stopped |

What the tools give back is JSON, as text. A call that fails, such as one
with arguments a command doesn't take, or a command that doesn't apply now,
is a tool error, with a sentence that says why, and changes nothing. As
they connect, agents also get a few lines on how the tools go together:
positions and lengths are numbers of beats from the start of the song,
notes' positions are in their clip's time, gains are linear, and a command
without arguments acts on the selection, as a key does.

Arguments are checked against each command's parameters
([commands.md](commands.md#arguments-and-results)) before anything runs,
on the server's thread. Then the window runs the command as a menu would,
and the panels and plugin windows it asks for show. Reads take what the
window has between frames, so they never see a change half made.

### What agents can run

Every command, but those that only show a window or ask you something,
which an agent can't see or answer, and Quit: the Settings window's pages,
the command palette, Search in Browser, Rename, Load Sample, and Save As,
Rename and Delete of workspaces (`Command::agents`). `musicable api` lists
those they can run.

The file commands take files by their full paths, so nothing depends on
where Musicable was started from, and skip the dialogs:

- `file.open` (`file`) opens a project, and `file.save_as` (`file`) saves
  it, adding `.musicable` to a name without it. `file.save` saves the
  project to its file, once it has one. They give back the file.
- `file.new`, `file.new_demo` and `file.open` ask you about unsaved
  changes first, in the window, which asks for your attention, flashing
  in the taskbar.
  The agent's call waits for your answer, and hears if you cancelled.
  Until you answer, agents' other commands fail, saying the window is
  waiting for you, and their reads go on.
- `file.import_audio` (`files`, `track`?, `at`?) puts audio files on a
  track, all of them or none if one can't be read, and gives back the
  clips, as one step.
- `file.export_audio` (`file`, `range`?, `bits`?, `sample_rate`?,
  `dither`?, `normalize`?, `tail`?, `mix`?, `stems`?, `tracks`?) renders
  the song to the file, in the format its extension says, `wav`, `aiff` or
  `flac`, over the `range`, `song`, `loop` or `selection`, or else what the
  window starts out with: the time selection, or else the song. What the
  other arguments leave out is as the export window last had it. It writes
  the mix unless `mix` is false, and with `stems`, each of `tracks`, or of
  all the tracks, on its own, next to the file, named after it. The call
  waits for the export, and gives back the files; your export settings
  stay as they were.

### Settings

Settings aren't steps in the undo history, so an agent that changes one
puts it back when it's done. The commands that change them run alone, and
answer with the setting's `key`, what it `was` and what it is `now`; the
status bar says an agent changed it, as for its other commands.

- `options.set` (`key`, `value`) changes a setting by its name in
  `settings.toml`, such as `audio.render_ahead`, `recording.capture_minutes`,
  `analysis.fast`, `behaviour.play_from` or `export.bits`, to `value`, as
  text: `long`, `true`, `10`, or `none` to empty one that can be. It goes
  through the same code as the Settings window, and checks the value as
  reading the file would. `get_settings` lists those it can change, in
  `changeable`: not the theme, the workspaces, the profiles, the agents'
  switch or the plugins' marks, which are yours.
- `options.set_render_ahead` (`render_ahead`: `off`, `short`, `normal` or
  `long`) is `options.set` for Render Ahead.
- `options.set_audio_device` (`driver_type`?, `device`?, `buffer`?,
  `sample_rate`?, `exclusive`?) plays through another device, by its id or
  name from `list_audio_devices`, or the same one with another buffer, rate
  or mode, as the Audio page's Apply does. A new driver type without a
  device takes the system's default output, or the first ASIO driver. If
  the device won't open, the one before plays on, and the call fails saying
  why. An ASIO driver's buffer is set in its own panel.
- `transport.reset_dropouts` counts dropouts from zero again, as clicking
  **Dropouts** does, and `mixer.clear_peaks` clears every channel's highest
  peak, as clicking one does.
- `options.set_keys` (`command`, `keys`) gives a command its keys in place
  of the ones it has, as the Keys page does, and saves them to the keymap
  file: each as `list_commands` gives them, such as `Ctrl+Shift+M`, or
  `Delete in arrangement` for one that works only while that panel has the
  focus. A key another command has in the same place is taken from it.
  `options.reset_keys` (`command`) gives it its default keys again. Both
  answer with the `command`, its keys as they `was` and are `now`, and the
  keys `taken`, each with the command it was taken `from`.
- `workspace.save_as` (`name`) keeps the panels' layout as a new workspace,
  and `workspace.rename` (`name`) renames the one in use, as the dialogs
  you'd be asked in do. They answer with the `workspace`.

The code is in
[`crates/app/src/app/changing_settings.rs`](../crates/app/src/app/changing_settings.rs)
and [`changing_keys.rs`](../crates/app/src/app/changing_keys.rs).

### One step for several commands

`run_commands` runs commands in order, as one step in the undo history
under the name it's given, such as "Humanize", so you undo them at once.
Each acts on what those before it left. If one fails, none of them
changes anything, and the error says which one failed and why
(`Registry::run_batch`, in
[commands.md](commands.md#arguments-and-results)). Undo, Redo, Go to Step
and the file commands run alone, never among others.

## The audio tools

`musicable mcp` also has a tool for each audio command, which works on
audio files rather than the project: `audio_info`, `audio_cut`,
`audio_join`, `audio_convert`, `audio_gain`, `audio_loudness` and
`audio_find`. It runs them itself, so they work while Musicable is closed
or doesn't let agents in, and they never touch the open project. People
run the same commands as `musicable audio <command>`, and the
[manual](../manual/src/reference/audio-commands.md) describes each one, on
a page made from the code.

- **One registry of their own.** Each is an `AudioCommand` in
  [`crates/app/src/toolkit.rs`](../crates/app/src/toolkit.rs), beside the
  project's commands rather than among them: they make no steps in the
  undo history, have no keys, and need no window, engine or project. Each
  has parameters, checked as the project commands' are, an example on
  files the tests make, and the JSON Schema of what it gives back, which
  its tool has as its output schema.
- **Run by the bridge.** The bridge takes them as `LocalTools`
  ([`crates/mcp/src/local.rs`](../crates/mcp/src/local.rs)), so the mcp
  crate needn't know them, lists them after the app's tools, and runs each
  call on a thread of its own. A call sends progress notifications, at
  most four a second, if the agent's client asked for them, and stops when
  the agent cancels it, leaving nothing written. The tools are annotated
  as reading or writing files, and as working on this computer alone.
- **What they give back** is structured content, and the same JSON as
  text: the unit in each field's name (`peak_dbfs`, `integrated_lufs`),
  `null` for silence, positions as seconds and frames, and each file a
  command writes described as `audio_info` describes files. Lists hold 200
  items at most. `audio_info` and `audio_loudness`, which take several
  files, say why each one that failed did, and go on with the others.
- **Paths are whole.** The bridge's working folder is wherever the agent's
  client started it, so the tools refuse a relative path. `musicable
  audio` takes paths in the folder it's run in.
- **Files are safe.** A command never writes over a file without
  `overwrite`, and never to a file it reads, even with it. It writes
  `NAME.part`, and renames it once it's whole, so a file that's there is
  whole; one that fails or is cancelled removes it. Cuts, joins and
  conversions that only change the format, widen the samples or make a
  mono file stereo copy them exactly, read the new file back, and say
  whether it's `exact`.

Loudness is measured as [analysis.md](analysis.md#loudness) describes.

## `musicable api`

`musicable api` prints the commands agents can run as JSON, without
opening the app: each one's id, title, the JSON Schema of its arguments,
its default keys, and whether it runs alone. Under `audio_commands` it
lists the audio commands too: each one's id, title and tool, and the JSON
Schemas of its arguments and of what it gives back. Scripts and agents can
read it to find their way around without a running Musicable.

```json
{
  "commands": [
    {
      "id": "transport.set_tempo",
      "input_schema": {
        "additionalProperties": false,
        "properties": {
          "at": {
            "description": "Where the tempo to set is: the playhead without it. (a number of beats)",
            "type": "number"
          },
          "bpm": {
            "description": "The tempo, in beats per minute. (a number)",
            "type": "number"
          }
        },
        "required": ["bpm"],
        "type": "object"
      },
      "title": "Set Tempo"
    }
  ],
  "musicable": "0.2.0"
}
```

## How it's built

- [`crates/mcp`](../crates/mcp) has the server, the bridge and the pipe,
  on the official Rust SDK for MCP (`rmcp`). It depends on the model and
  the commands only, so it has no window or engine.
- **The server** (`Host`) runs on a thread of its own, with Tokio, while
  you let agents in. It serves each agent that connects the tools, checks
  a call's arguments, and hands the call to the window through a channel,
  waking it. The window answers with `musicable_mcp::answer`, in
  `App::logic`, through the `Window` trait, which the app implements on
  its session (`crates/app/src/app/agents.rs`). The answer goes back on a
  one-shot channel the window can keep, as it does while it asks about
  unsaved changes or an export runs.
- **The pipe.** On macOS and Linux it's a socket, `agents/socket` in the
  config folder, in a folder only you can open, and each end checks that
  the other runs as you. On Windows it's a named pipe,
  `\\.\pipe\musicable-agents-<your SID>-<a hash of the config folder>`,
  which only your account can open, which turns other computers away, and
  which the bridge checks your account made, so another account can't
  stand in for Musicable by making it first. Nothing listens on the
  network. A second Musicable with the same config folder can't listen
  while the first does, and says so.
- **The bridge** (`musicable mcp`) lists the tools itself, so an agent
  sees them while Musicable is closed, and passes calls on through the
  pipe, as the agent's client named itself. It connects as the client
  starts, so the status bar shows the agent at once, and again at a call
  after Musicable has closed or stopped letting agents in. The audio
  tools' calls, and `get_log`'s, it runs itself
  (`crates/app/src/bridge_tools.rs`).

### Testing

- [`crates/commands/src/every_command.rs`](../crates/commands/src/every_command.rs)
  runs every command's example as an agent runs it too, with its arguments
  as JSON, and checks that a batch is one step, or none if one of its
  commands fails, with random runs ([commands.md](commands.md#testing-every-command)).
- [`crates/mcp/src/tests.rs`](../crates/mcp/src/tests.rs) calls the tools
  over an in-memory connection, with a window answering on the test's
  thread, as the app's does: listing, describing, running, batches, wrong
  arguments, reads, and a call the agent gave up on. It also runs the
  bridge through a real pipe, with Musicable starting, stopping and
  starting again, and calls a bridge's own tools with nothing on the pipe,
  following their progress, and cancelling one.
- [`crates/app/src/toolkit/tests.rs`](../crates/app/src/toolkit/tests.rs)
  runs every audio command's example from the command line and as a tool,
  checks both give the same, that it fits the output schema, and that the
  files it wrote read back as it says. It checks cuts, joins and
  conversions sample for sample in every format io reads, loudness and
  true peak against EBU Tech 3341's cases, and each of the rules for
  files, cancelling and failing part way. A command without an example
  fails it.
- [`crates/app/src/app/controlled_by_agents.rs`](../crates/app/src/app/controlled_by_agents.rs)
  drives the whole window with `egui_kittest`: it lets agents in on the
  Agents page, checks the status bar, runs, undoes and batches commands,
  saves and opens files, answers the question about unsaved changes,
  exports, and reads the status and the messages.
- [`crates/app/tests/agents.rs`](../crates/app/tests/agents.rs) starts the
  real `musicable mcp`, as a client does, and speaks MCP with it, through
  a real pipe to a session, and runs the audio tools and reads the log
  with nothing listening, on Windows, macOS and Linux in CI. It runs
  `musicable audio` too, in text and in JSON, and checks its exit codes.

## Not done yet

- `musicable --headless`, which would let agents in without a window.
- Agents hearing of your changes as they're made, rather than reading the
  project again.
- Extensions, which will run in WebAssembly, on the same commands.
- More audio tools: spectra, matching and comparing two files, and
  warping a file as a clip would, for A/B tests of warping; then pitch,
  pictures and test signals. And MP3 and AAC files, which io doesn't read
  yet.
