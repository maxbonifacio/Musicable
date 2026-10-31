# The command line

Started with nothing after it, `musicable` opens its window with an empty
project. These do more:

| Command | What it does |
|---|---|
| `musicable PROJECT` | Opens the window with the project file PROJECT. |
| `musicable export PROJECT FILE` | Renders the project file PROJECT to the audio file FILE, as **File > Export Audio** does, faster than real time. |
| `musicable keymap [FILE]` | Prints every command with its keys: the default keys, with the changes in the keymap file FILE if you give one, and any mistakes in that file. |
| `musicable null-test [PROJECT]` | Plays PROJECT, or the demo, with Musicable's two renderers side by side, and checks that they play exactly the same. It's a check for developers. |
| `musicable audio <command> FILE...` | Cuts, joins, converts or measures audio files, without opening the window: see [Audio commands](audio-commands.md). |
| `musicable analyse FILE...` | Finds the tempo, beats and downbeats of audio files, as warping will, and scores them against beats you've marked, and against Live's: see [Analysing files](#analysing-files). |
| `musicable mcp` | Connects an agent, such as Claude Code, to the Musicable that's running, and gives it the audio commands as tools. The agent's app starts it: see [Agents](#agents). |
| `musicable api` | Prints the commands agents can run, with their arguments, and the audio commands, as JSON. |
| `musicable --help` | Lists these. |

On Windows, the program opens no console window of its own, so run these
from a console, such as PowerShell, to see what they print.

## Exporting

`musicable export PROJECT FILE` renders the song, from its start to the end
of its last clip, with the plugins Musicable's last plugin scan found. It
takes these options after the file:

| Option | What it does |
|---|---|
| `--loop` | Renders the loop instead of the song. |
| `--format <format>` | `wav`, `aiff` or `flac`. Without it, the file's extension says which, or WAV. |
| `--bits <bits>` | 16, 24, or 32 for floating point. 24 without it. |
| `--rate <hz>` | The sample rate it renders at. 48000 without it. |
| `--block <frames>` | The most frames plugins get at once. 512 without it. |
| `--stems` | Renders each track on its own too, in files next to FILE. |
| `--no-mix` | Renders only the stems, without the mix. |
| `--normalize` | Raises or lowers each file so its peak is at full scale. |
| `--no-dither` | Writes 16- and 24-bit files without dither. |
| `--no-tail` | Stops at the end, without the tail of echoes and reverb. |

`musicable export --help` lists them too.

## Analysing files

`musicable analyse FILE...` finds what warping needs to know about each
audio file, and prints it: its tempo and whether it's steady, whether it
has a beat at all, whether it's a loop of whole bars, and how many beats,
bars and transients it has. It finds the beats with a neural network, Beat
This!, and checks them with a classic beat tracker.

To see how well it does, mark a file's beats yourself. It then scores what
it found against them: a number from 0 to 1 for the beats, and another for
the downbeats, where 1 means it found every beat within 70 ms and nothing
else. There are two ways to mark them:

- **In a `.beats` file** beside the audio, with the same name, such as
  `Drums.beats` beside `Drums.wav`. Each line is a beat: its time in
  seconds, then its place in the bar, where 1 starts a bar, such as
  `12.345 1`. Sonic Visualiser exports time instants this way.
- **In Live**: warp the files by hand in a Live Set, save it, and give it
  with `--labels`. The beats between two warp markers are spread evenly, so
  put a marker wherever the playing moves off the grid.

With `--live`, it scores Live's own auto-warp of the same files too: drop
them into a new Live Set with auto-warp on, save it, and give it with
`--live`. At the end it prints the mean of each, so you can compare them.

| Option | What it does |
|---|---|
| `--fast` | Leaves the network out, as Fast analysis will. |
| `--labels <set>` | Takes the beats you marked from the warp markers in the Live Set, instead of from `.beats` files. |
| `--live <set>` | Scores Live's warping of the files in the Live Set too. |
| `--json` | Prints everything it found in each file, as a line of JSON. |

In a Live Set, bars count as four beats from 1.1.1.

The network needs its two model files in a `models` folder beside the
program, or in a folder above it. In the source code,
`scripts/fetch-models.sh`, or `scripts\fetch-models.ps1` in PowerShell,
fetches them into the folder a build finds them in. The environment
variable `MUSICABLE_MODELS` can name another folder. Without them, give
`--fast`.

## Agents

Agents, such as Claude Code, can run Musicable's commands and read the
project while you watch. Every change one makes is a step in the undo
history, as yours are. Nothing listens for agents until you let them in:
**Options > Agents...** opens the Settings window's Agents page, where
**Let agents control Musicable** switches them on, and back off. The page
also has the command that adds Musicable to Claude Code, with Musicable's
own path in it, and a **Copy** button for it.

Other agents' apps start `musicable mcp` the same way. It passes their
calls on to the Musicable that's running, and only your account on your
computer can connect to it.

Agents can read what each device costs the CPU too, as the
[Performance panel](../window/performance.md) lists it, to say which plugin
to freeze or take out, and how far ahead of the audio device Musicable
renders ([Render Ahead](../getting-started/audio.md#render-ahead)).

They can read what plays and what's under way: whether the transport plays
or records, where the playhead is, the takes being recorded, a question the
window asks you, and work that finishes later, such as analysing audio files
for warping, rendering, an export or a scan. They can read the messages in
the status bar, and clear them, as **Dismiss** (×) does. And they can read
the [log file](troubleshooting.md#the-log-file), which `musicable mcp` reads
itself, so they can see why Musicable stopped after it has.

Agents can read your settings and the audio devices there are, and change
the settings the Settings window has, such as the audio device, its buffer
size and [Render Ahead](../getting-started/audio.md#render-ahead), with the
commands Change Setting, Set Audio Device and Set Render Ahead. Changing a
setting isn't a step in the undo history, so the status bar says when an
agent did, and the agent hears what the setting was, to put it back when
it's done. Agents can't change the theme, your workspaces or profiles, or
whether they're let in.

Agents also get the [audio commands](audio-commands.md) as tools, which
cut, convert and measure audio files. `musicable mcp` runs those itself,
so they work while Musicable is closed or doesn't let agents in, and they
never change the project you have open.
