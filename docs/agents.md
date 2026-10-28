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
change to them. The others read what the window shows.

| Tool | Takes | Gives back |
|---|---|---|
| `list_commands` | `filter`?, text the id or the title holds | Each command's id, title, arguments with their types, whether it applies now if it needs no arguments, and its keys |
| `describe_command` | `id` | Its arguments as a JSON Schema, saying what each is for, whether it applies now, its keys, and whether it runs alone |
| `run_command` | `id`, `args`? | Its `result`, such as a new track's id, and `undo`, the name of the step it made, if it made one |
| `run_commands` | `name`, `commands`: each an `id` and `args`? | Each one's result, in `results`, and `undo`: they're one step, called `name` |
| `get_project` | nothing | The file and whether it's saved, the tempo map, the time signatures, the loop, the metronome and punch switches, the tracks with their mixer settings, devices, clips and takes, and their modulators and routings if they have any, the master, the wires, each from its tap to its device's port at its gain, the locators, and which parameters are automated. A track that takes its input from another track's tap has it as `mixer.set_input` takes it, `from` and `tap`, `after` or `resampling`, with the tap's `name`, and one that feeds nothing but its sends has the output `"sends_only"`. A bypassed device has `"bypassed": true`, and a deactivated one `"active": false`. A routing has the names the modulation commands take: `modulator`, `output`, `param`, `amount`, `curve`, `steps` if it's stepped, `scale` if it has one, `lag`, `"bypassed": true` if it is, and `"plays": false` while its parameter is out of its modulator's reach |
| `get_clip` | `clip` | Where it is, and its notes with their expression and its controllers' curves, in the clip's own time, or its audio file, with its warp (its markers, mode, transpose, detune, Slice's settings, Spectral's Formants, High quality and Envelope, and Granular's Follow pitch, Grain Size and Fluctuation) and its curves, by parameter, if it has them |
| `get_device` | `device` | What it is and where, whether it's bypassed or deactivated, each parameter's number, name, value, range, default and unit, and its ports, a Compressor's or a Gate's Sidechain or a plugin's side inputs, by the plugin's ids and names for them, with the wires into each |
| `get_modulator` | `modulator` | What it is and where, each parameter's number, name, value, range, default and unit, a Macro's knobs and buttons with their kinds, and its outputs, each with its range |
| `get_automation` | `param`, such as `track 3 volume`, `wire 1` or `modulator 6 param 2` | Its points, in beats and the parameter's units, and whether it's overridden |
| `get_editor` | nothing | What's selected, the selected track, which panel has the focus, the start marker, the grid, what's in sight, the piano roll's clip and selected notes, the Sample Editor's clip, selected warp markers and the parameter its lane shows, and the devices selected in the devices panel, of the selected track's, in the order they play |
| `get_history` | nothing | Each step's name, oldest first, how many are done, and what Undo and Redo would change |

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
- `file.export_audio` (`file`) renders the mix to the file, with the
  export window's last settings and the format the file's extension says,
  `wav`, `aiff` or `flac`, from what the window starts out with: the time
  selection, or else the song. The call waits for the export, and gives
  back the file; your export settings stay as they were.

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
  tools' calls it runs itself.

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
  saves and opens files, answers the question about unsaved changes, and
  exports.
- [`crates/app/tests/agents.rs`](../crates/app/tests/agents.rs) starts the
  real `musicable mcp`, as a client does, and speaks MCP with it, through
  a real pipe to a session, and runs the audio tools with nothing
  listening, on Windows, macOS and Linux in CI. It runs `musicable audio`
  too, in text and in JSON, and checks its exit codes.

## Not done yet

- `musicable --headless`, which would let agents in without a window.
- Agents hearing of your changes as they're made, rather than reading the
  project again.
- Extensions, which will run in WebAssembly, on the same commands.
- More audio tools: spectra, matching and comparing two files, and
  warping a file as a clip would, for A/B tests of warping; then pitch,
  pictures and test signals. And MP3 and AAC files, which io doesn't read
  yet.
