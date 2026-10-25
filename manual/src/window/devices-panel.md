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
