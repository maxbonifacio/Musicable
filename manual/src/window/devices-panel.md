# The devices panel

The devices panel shows the devices of the selected track, which a click on
its header or its mixer strip selects, in the order the signal goes through
them, left to right, with the track's name over them. Clicking the master
track's header, at the bottom of the arrangement, or the master's strip
shows the master's. **Add Device** opens the browser's devices.

## Adding devices

Double-click a device in the browser's Devices list to put it at the end of
the selected track's devices, or the master's while the master track is
selected. Or drag it into the devices panel, onto a track, or below the
tracks for a new track ([The browser](browser.md#devices)). Adding a device,
however it's added, brings the devices panel forward and scrolls it to the
device.

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
  bypassed, and **Deactivate**, or **Activate**.

Each is a step in the undo history, such as "Bypass EQ" or "Deactivate
EQ". A bypassed instrument is silent: the notes it was playing end with the
fade, and notes that start while it's bypassed aren't heard, though the next
one after it comes back is. A Bypass lane automates the button
([Automation](../using/automation.md#bypass)), and clicking it overrides the
lane, as moving any control does. The Settings window's Mouse page changes
what a click and an Alt-click do ([The mouse](../customising/mouse.md)).

## Built-in devices

A built-in device has its name and its controls, which [Using the
devices](../devices/index.md#their-controls) explains, and each device has a
page of its own under [Built-in devices](../devices/index.md).

## Plugins

A plugin device has its name, its kind and format ("Instrument · CLAP"),
and:

- **Plugin Window**, which opens the plugin's own window, or closes it.
  Double-clicking the device's name opens it too, as in Live
  ([Plugins](../using/plugins.md#plugin-windows)).
- **×**, which deletes the device.
- Why the plugin isn't playing, when it isn't: it isn't installed, it didn't
  load, or the scan that finds it hasn't finished. A device whose plugin
  isn't playing lets the signal through as it is, and keeps its settings for
  when the plugin is there.

## Presets

**Save as Preset...**, in a device's right-click menu, saves the device's
settings, a plugin's state and all, as a preset under the name you give it
([The browser](browser.md#presets)).
