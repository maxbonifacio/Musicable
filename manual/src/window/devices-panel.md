# The devices panel

The devices panel shows the devices of the selected track, which a click on
its header or its mixer strip selects, in the order the signal goes through
them, left to right, with the track's name over them. Clicking the master
track's header, at the bottom of the arrangement, or the master's strip
shows the master's. **Add Device** opens the browser's devices, and **Add
Modulator** adds a modulator to the track.

## Adding devices

Double-click a device in the browser's Devices list to put it at the end of
the selected track's devices, or the master's while the master track is
selected. Or drag it into the devices panel, where a line shows where it
would go between the devices, onto a track or a mixer strip, or below the
tracks for a new track ([The browser](browser.md#devices)). Adding a device,
however it's added, brings the devices panel forward and scrolls it to the
device. The **×** at the right of a device's title bar deletes it.

Left of the **×**, while an audio device plays, is the share of a core the
device took over the last second: amber over 25% of a core, and red over
50%. Its tooltip adds whether it renders live or ahead of the audio
device, and while live, the device's longest block, as a share of the
buffer ([The Performance panel](performance.md)). A deactivated device
takes none.

## Selecting, moving and copying

Click a device's title bar, or its box where no control takes the click, to
select it, as in Live: it's outlined. **Ctrl-click** or **Shift-click**
adds a device to the selection, or takes it out, and a click beside the
devices selects none. Only the devices panel's track has devices selected,
so selecting another track forgets them.

- **Drag** a device by its title bar to move it, with the other selected
  devices if it's one of them. In the devices panel, a line shows where
  they'll go. Let go of them over the arrangement to move them onto a
  track, the master track, or below the tracks onto a new track named
  after the first, or over a mixer strip to put them at the end of its
  devices. Press **Escape** before you let go to leave them where they
  were.
- **Ctrl-drag** copies them instead.
- **Ctrl+Left** and **Ctrl+Right** move the selected devices one place
  along the chain.
- **0** bypasses the selected devices, or brings them back if they're all
  bypassed. Live has no key for this; its 0 deactivates clips and tracks.
- While the devices panel has the focus, which a click in it gives it, the
  edit keys work on the selected devices: **Ctrl+X** cuts them, **Ctrl+C**
  copies them, **Ctrl+V** puts copies in front of the first selected
  device, or at the end, **Ctrl+D** puts copies just after the last one,
  **Delete** deletes them, and **Ctrl+A** selects every device.
- A device's right-click menu has **Cut**, **Copy**, **Paste**,
  **Duplicate** and **Delete** too, with **Bypass** and **Deactivate**. It
  works on the selected devices if the device is one of them, or else on
  the device alone, which it selects.

A MIDI effect stays in front of its track's instrument: a move that would
take it past the instrument, or onto a track without notes, is refused, and
says why, and a copy goes just in front of the instrument, or isn't made
where there are no notes ([Plugins](../using/plugins.md#midi-effects)).

A moved device takes everything with it: its settings, its automation, and a
plugin's own window, open if it was. Moved onto the master, it leaves a
track that took the point after it with **No Input**, as deleting it does
([Routing](../using/routing.md#undoing-and-deleting)). While the song plays,
it's heard in its new place at once. A copy is a device of its own, with its
original's settings, a plugin's state as its window has it, and its
automation, in lanes of its own with the same points, as in Live. Turn off
**Copies of devices take their automation** on the Settings window's
Behaviour page for copies that start without any
([Settings](../customising/settings.md)). Pasted and duplicated devices are
copies too, and copies come selected, so Ctrl+D again makes more. Each move
or copy is one step in the undo history, such as "Move EQ" or "Copy 2
Devices". The Settings window's Mouse page changes what the clicks and drags
do ([The mouse](../customising/mouse.md)).

## Bypassing and deactivating

Each device has a **Bypass** button left of its name, a power button as in
Live, lit while the device plays.

- **Click** it to bypass the device: it stops, and what comes into it plays
  on in its place, as late as the device's latency would make it, so
  nothing else in the mix moves, and the device's CPU is free. It fades out
  over 10 ms, and its box dims, but its controls still work, so you can set
  it up before you bring it back. Click again to bring it back: it starts
  afresh, from silence, and fades in over 10 ms, once a device that looks
  ahead has caught up.
- **Alt-click** it to deactivate the device: it's out of the signal, its
  latency too, as if it were deleted, though it keeps its place and its
  settings, and a plugin stays loaded. Its box dims and says
  **Deactivated**. What goes through the track then plays sooner, by the
  device's latency, which moves the whole mix when the track is the latest
  one, so deactivating can't switch on the beat as bypassing does. It's for
  when you'd rather hear less delay, as while recording through a track
  with a look-ahead limiter or a linear-phase EQ. A click on a deactivated
  device's button activates it again, as Alt-click does.
- The device's right-click menu has **Bypass**, ticked while the device is
  bypassed, and **Deactivate**, or **Activate**. For the selected devices,
  Bypass is ticked while they're all bypassed, and Activate shows while
  any is deactivated.
- **0** bypasses the selected devices, or brings them back if every one is
  bypassed.

Each is a step in the undo history, such as "Bypass EQ" or "Deactivate
EQ". A bypassed instrument is silent: the notes it was playing end with the
fade, and notes that start while it's bypassed aren't heard, though the next
one after it comes back is. A bypassed MIDI effect passes the notes on as
they come, and a deactivated one is out of their way. A Bypass lane automates the button
([Automation](../using/automation.md#bypass)), and clicking it overrides the
lane, as moving any control does. The Settings window's Mouse page changes
what a click and an Alt-click do ([The mouse](../customising/mouse.md)).

## Modulators

A track's modulators have cards in its **modulator strip**, at the left of
its devices, with a line between them, and the strip shows even when the
track has no devices. Drag a card's handle onto a control, in the devices
panel or the mixer, to have the modulator move it, and the control shows how
far its modulators take it
([Modulation](../using/modulation.md#the-modulator-strip)).

## Built-in devices

A built-in device has its name and its controls, which [Using the
devices](../devices/index.md#their-controls) explains, and each device has a
page of its own under [Built-in devices](../devices/index.md).

## Plugins

A plugin device has its name, its kind and format ("Instrument · CLAP",
"MIDI Effect · VST3" or "Audio Effect · CLAP"), and:

- **Plugin Window**, which opens the plugin's own window, or closes it.
  Double-clicking the device's name opens it too, as in Live
  ([Plugins](../using/plugins.md#plugin-windows)).
- Why the plugin isn't playing, when it isn't: it isn't installed, it didn't
  load, or the scan that finds it hasn't finished. A device whose plugin
  isn't playing lets the signal through as it is, and keeps its settings for
  when the plugin is there.
- A row for each of its side inputs, such as a compressor's sidechain or a
  vocoder's carrier, under the plugin's name for it: where its signal comes
  from, with a gain for each source, and a **+** to add another
  ([Sidechains](../using/sidechains.md#plugins-side-inputs)).

## Presets

**Save as Preset...**, in a device's right-click menu, saves the device's
settings, a plugin's state and all, as a preset under the name you give it
([The browser](browser.md#presets)).
