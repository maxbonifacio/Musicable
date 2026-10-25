# The command line

Started with nothing after it, `musicable` opens its window with an empty
project. These do more:

| Command | What it does |
|---|---|
| `musicable PROJECT` | Opens the window with the project file PROJECT. |
| `musicable export PROJECT FILE` | Renders the project file PROJECT to the audio file FILE, as **File > Export Audio** does, faster than real time. |
| `musicable keymap [FILE]` | Prints every command with its keys: the default keys, with the changes in the keymap file FILE if you give one, and any mistakes in that file. |
| `musicable null-test [PROJECT]` | Plays PROJECT, or the demo, with Musicable's two renderers side by side, and checks that they play exactly the same. It's a check for developers. |
| `musicable mcp` | Connects an agent, such as Claude Code, to the Musicable that's running. The agent's app starts it: see [Agents](#agents). |
| `musicable api` | Prints the commands agents can run, with their arguments, as JSON. |
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
