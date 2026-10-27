# Plugins

Musicable plays CLAP and VST3 plugins, instruments and effects, beside its
own [built-in devices](../devices/index.md).

## Finding them

Musicable finds plugins in each format's usual folders, and the folders in
them:

| System | CLAP | VST3 |
|---|---|---|
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |
| macOS | `/Library/Audio/Plug-Ins/CLAP`, `~/Library/Audio/Plug-Ins/CLAP` | `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Linux | `~/.clap`, `/usr/lib/clap` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |

`%COMMONPROGRAMFILES%` is usually `C:\Program Files\Common Files`. To look
in other folders too, list them in the `CLAP_PATH` and `VST3_PATH`
environment variables before starting Musicable.

Musicable scans in the background as it starts, with the plugins the last
scan found ready at once, and loads only the plugins that are new or
changed since. A plugin that crashes, hangs or fails as it loads is left
out, and the status bar says which; **Options > Rescan Plugins** tries
those again too.

The browser's Devices list has the plugins found under **Plugins**, after
Musicable's own devices. A plugin installed in both formats is listed in
each, with its format after its name: "Surge XT (CLAP)" and "Surge XT
(VST3)". The list's foot says how many plugins there are, and has
**Rescan**, or says "Scanning..." while a scan runs.

## Putting one on a track

Double-click a plugin in the browser to put it at the end of the selected
track's devices, or the master's while the master track is selected. Or drag
it into the devices panel, onto a track for the end of its devices, onto the
master track for the master's, or below the tracks for a new track named
after it: a MIDI track for an instrument and an audio track for an effect
([The browser](../window/browser.md#devices)).

Drag a plugin device by its title bar in the devices panel to move it along
the chain or to another track, and Ctrl-drag it to copy it ([The devices
panel](../window/devices-panel.md#selecting-moving-and-copying)). A moved
plugin plays on, with its window open if it was. A copy loads the plugin
again, with the state the original has in its window.

## Plugin windows

**Plugin Window**, on the plugin's device in the devices panel, opens the
plugin's own window, or closes it, and double-clicking the device's name
opens it too, as in Live. On Windows, the plugin's window goes inside a
window of Musicable's that opens over the window the devices panel is in, on
its monitor, stays in front of it and minimizes with it. Elsewhere it's a
window of the plugin's own. VST3 plugins' windows open only on Windows so
far.

## Bypassing

A plugin device's Bypass button stops the plugin, as it does any device
([The devices
panel](../window/devices-panel.md#bypassing-and-deactivating)). A plugin
with a bypass switch of its own shares it with its device: bypassing the
device switches the plugin's own on, once the device has faded out, and
clicking the plugin's own in its window bypasses the device, as a step in
the undo history. So they never disagree, and Musicable still does the
bypassing, which keeps the plugin's latency and frees its CPU. When a
project opens, the device's Bypass wins over what the plugin's state says.
A bypassed plugin still takes the changes made to it, from its window,
automation or the project, so it comes back as you left it.

## Undo, saving and timing

Adding a plugin, moving it, copying it, deleting it and every change made
in its window are steps in the undo history, named after the plugin, such
as "Change Surge XT"; a whole drag of a knob is one. What a plugin changes on its own, as it loads,
or with its window closed, goes into the project without a step. Saving the
project saves each plugin's state as it is then.

Delay compensation lines up every track with the latest one, so a plugin
that looks ahead doesn't put its track out of time.

A device whose plugin isn't playing, because it isn't installed, didn't
load, or the scan hasn't found it yet, lets the signal through as it is,
and keeps its settings for when the plugin is there. The devices panel
says why ([The devices panel](../window/devices-panel.md#plugins)).
