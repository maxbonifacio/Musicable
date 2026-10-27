# Automation

Automation moves a control over the song: a track's volume, pan and sends,
and every control of its devices, built-in or plugin.

## Showing the lanes

A track's automation shows in lanes under its lanes in the arrangement,
after the **A** button in its header, **Automation > Show/Hide Automation**
(A), or the same in the header's menu: a lane for each parameter the track
automates, or else one for its volume. The master's lanes go under its row,
after its A button, or from its controls' menus in the mixer's master strip.

A lane's header names the parameter, with its value at the playhead, or
"Overridden" once someone moved its control by hand, and has three buttons:

- **▾** chooses the parameter the lane shows: the track's volume, pan and
  sends, then each device's parameters, in a submenu for each device, the
  automated ones in red. A Compressor's or a Gate's submenu ends with the
  gains of its sidechain's sources ([Sidechains](sidechains.md)).
- **+** adds a lane for the next parameter.
- **×** hides the lane.

The line through a lane's points is red, or grey while its automation is
overridden. A lane without automation shows the parameter's own value as a
dashed line. A built-in device's lane spreads the values as its control
does, a frequency on a log scale say, and names them as it does ("1.20
kHz").

## Bypass

Every device has a **Bypass** lane, first in its submenu, which bypasses
the device and brings it back as its Bypass button does ([The devices
panel](../window/devices-panel.md#bypassing-and-deactivating)): **On** is
bypassed, and **Off** plays, as in a plugin's own bypass. It switches on the
very sample it's drawn at, with the 10 ms fade, in playback and in an
export alike, and keeps the device's latency, so the mix keeps its timing.
Clicking the Bypass button overrides the lane. A plugin's own bypass, if
it has one, follows the device's, so it has no lane of its own
([Plugins](plugins.md#bypassing)).

## Drawing

- **Drag** across a lane to draw a value for each step of the grid crossed,
  as Live's Draw Mode does, or **Alt-drag** to draw freely, off the grid, as
  it does with Snap to Grid off.
- **Double-click** a lane to add a point there, or **click** its line to add
  one on the line, on the grid.
- **Drag** a point to move it, on the grid and between the points either
  side, or Alt-drag it to move it off the grid. **Click** a point to delete
  it.
- **Drag the line** between two points to move that stretch of it, with its
  points, up or down.

A lane's right-click menu has **Re-Enable Automation**, **Clear Automation**
and **Hide Automation Lane**. [The
arrangement](../window/arrangement.md#the-mouse) lists every gesture, and
the Settings window's Mouse page changes them ([The
mouse](../customising/mouse.md)).

## Overriding and re-enabling

A control with automation follows it, showing its value at the playhead,
with a red dot at its top left: a fader, a send or a pan in the mixer, or a
control of a built-in device. Moving the control by hand overrides the
automation, as in Live: the dot turns grey, and the control stays where you
put it until you re-enable the automation. So does moving a plugin's
control in the plugin's own window. One undo puts back both the value and
the automation. An overridden lane keeps its points, which you can still
edit.

Right-click the control for **Show Automation**, which shows its lane in the
arrangement, **Re-Enable Automation** and **Clear Automation**.
**Automation > Re-Enable Automation** re-enables every overridden lane at
once.

## Moving and copying devices

A device's automation goes with it when it moves, along its chain or to
another track. A copy of a device, dragged, duplicated or pasted, gets lanes
of its own with the original's points, as in Live, and they show on its
track. Switch off **Copies of devices take their automation** on the
Settings window's Behaviour page for copies that start without automation
([Settings](../customising/settings.md#behaviour)).

## Not yet

Points can't be selected to move or delete several at once yet, the lines
between points are straight, and automation doesn't move or copy with its
clips. Switches and choices take whole values at their points, and pass
through the values between two points that differ, so switching at a place
takes a step there.
