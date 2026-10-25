# Troubleshooting

## Nothing plays

- The status bar's audio readout says "No audio device" when none opened:
  hover the CPU meter, at the right end of the transport bar, for why.
  **Options > Audio Settings...** picks another device
  ([Setting up audio](../getting-started/audio.md)).
- Another app may have the device to itself, in exclusive mode or through
  its ASIO driver. Close it, or play through the device in shared mode.
- Check the tracks' mute and solo buttons, their faders, and the master's.

## Clicks and gaps

Clicks, crackles and gaps are dropouts: the device ran out of audio. The
dropout counter at the right end of the transport bar counts them, and
[The CPU meter and the
dropouts](../getting-started/audio.md#the-cpu-meter-and-the-dropouts) says
what helps.

## Recordings land late or early

A take lands where you played it, by the latency the driver reports. When
your interface adds latency it doesn't report, set its recording offset
([Where it lands](../using/recording.md#where-it-lands)).

## A MIDI keyboard does nothing

- MIDI inputs open on Windows only so far.
- A MIDI track plays its input when it's armed, or monitoring In, and its
  strip's input menu says which input that is: a new MIDI track takes every
  input ([MIDI inputs](../using/recording.md#midi-inputs)).
- A keyboard plugged in while Musicable runs opens within two seconds, once
  a track takes it, but not while Musicable records.

## A plugin is missing, or doesn't play

- Musicable finds plugins in each format's usual folders, and in the
  folders `CLAP_PATH` and `VST3_PATH` list
  ([Plugins](../using/plugins.md#finding-them)). **Options > Rescan
  Plugins** looks again, and tries the plugins that failed before.
- A plugin that crashes, hangs or fails as it loads is left out, and the
  status bar says which.
- The devices panel says why a plugin on a track isn't playing ([The devices
  panel](../window/devices-panel.md#plugins)).

## Audio clips are silent

A clip whose file is missing, or isn't one Musicable reads, is silent, and
the status bar says so. Moving a project away from its files does that:
move the project's folder with its files in it instead ([Projects and
files](../using/projects.md#audio-files)).

## Keys don't do what they should

- While a text field is being typed in, such as the tempo field or the
  browser's search, every key goes to it.
- A panel's own keys win while it has the focus, so check which panel is
  outlined ([How keys reach
  commands](../customising/keys.md#how-keys-reach-commands)).
- The Keys page lists the mistakes in your keymap file
  ([Keys](../customising/keys.md#mistakes)).
- The Windows key doesn't reach Musicable, so keys with Cmd work only on
  macOS.

## The window doesn't draw, or Musicable crashes as it starts

On Windows, Musicable draws with Direct3D 12. To try another graphics API,
set the `WGPU_BACKEND` environment variable to `vulkan` or `gl` before
starting it, as this does in PowerShell, from the source code's folder:

```powershell
$env:WGPU_BACKEND = "vulkan"
target\release\musicable.exe
```

`dx12`, `metal` (on macOS), `vulkan` and `gl` are the APIs it can draw
with. When the window can't open at all, Musicable says why in a message.

## Starting afresh

To try Musicable with nothing of yours, its own keys, layout and settings,
start it with the `MUSICABLE_CONFIG_DIR` environment variable set to an
empty folder. Your own config folder stays as it is for next time
([Settings](../customising/settings.md#the-config-folder)).

## The log file

Musicable writes what goes wrong while it runs to `musicable.log` in its
config folder ([Settings](../customising/settings.md#the-config-folder)):
warnings and errors, the graphics card it draws with, the graphics device
being lost, and crashes, with what led to them, each line with the time in
UTC. Once the file grows past a megabyte, it moves to `musicable.old.log`,
so the two hold the latest lines. The console Musicable was started from,
if any, shows them too.

## Reporting a bug

[Open an issue](https://github.com/maxbonifacio/Musicable/issues) on
GitHub, with what you did, what happened and what you expected. Add the log
file, and say which commit you built (`git log -1` in the source code's
folder prints it), on which system.
