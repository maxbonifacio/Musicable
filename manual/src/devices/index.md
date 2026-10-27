# Using the devices

Musicable has eight devices of its own, which play without plugins:

- Effects: the [EQ](eq.md), the [Compressor](compressor.md), the
  [Gate](gate.md), the [Delay](delay.md), the [Reverb](reverb.md) and the
  [Utility](utility.md).
- Instruments: the [Sampler](sampler.md) and the [Synth](synth.md). They
  play their track's notes, from its clips or its MIDI input, and bend each
  note on its own, as MPE bends them. The sustain pedal holds on the notes
  let go while it's down. A note's pressure and slide don't move them yet,
  and nor do other controllers.

## Adding one

The browser's Devices list has them under **Built-in**, above the plugins,
and its search field finds them by name. Double-click one to put it at the
end of the selected track's devices, or the master's while the master track
is selected, with every control at its default. Or drag it into the devices
panel, or onto a track.

## Their controls

The devices panel shows a device's controls in columns, a column for each
group of them, such as one of the EQ's bands:

- **A bar** for each number, filled as far as its value goes. Drag it
  sideways, or Ctrl-drag to move it a tenth as far. Double-click it for its
  default. Right-click it to type a value, then press Enter: a number with
  or without its unit ("1.2k", "1.2 kHz", "-6 dB", "250 ms"), a note's name
  ("C#3") or a pan ("20L").
- **A button** for each switch, lit while it's on.
- **A menu** for each choice, such as a band's shape.

Every change is a step in the undo history, named after the device, such as
"Change EQ", and a whole drag is one step. Changes are smoothed, so they
don't click.

The **Bypass** button left of a device's name bypasses it, and Alt-click
deactivates it ([The devices
panel](../window/devices-panel.md#bypassing-and-deactivating)). A bypassed
device's controls still work, and it comes back afresh: a delay's or a
reverb's tail, a compressor's envelope and an instrument's voices start
from silence.

Every control can be automated. A control with automation follows it, with
a dot at its top left: red while it follows, grey once you move the control
by hand, which overrides the automation until you re-enable it. Right-click
a control for **Show Automation**, **Re-Enable Automation** and **Clear
Automation**.

## The tables on these pages

Each device's page lists its controls group by group, as the devices panel
shows them, with their ranges and defaults. Groups with the same controls,
such as the EQ's bands, share their tables: one gives the controls' ranges,
and the next each group's defaults. The tables come from Musicable's code,
so they match the version this manual describes.
