# The arrangement

The arrangement shows the song's tracks over time, as Live's Arrangement
View does: the ruler along the top, a lane for each track with its clips,
and each track's header on the right. Everything you do in it can be undone,
and a whole drag is one step in the undo history.

## What it shows

- **The ruler** has the bar numbers, and along its top, a strip with the
  loop brace and the locators. The start marker is a triangle at the ruler's
  foot, and the playhead a line through the lanes, drawn where the sound is:
  where the song plays, less the latency of the plugins and the audio
  device.
- **Clips** show their name in a title bar, and under it an audio clip's
  waveform, with its fades and their handles, or a MIDI clip's notes. A clip
  takes the colour of its track's place in the list. Selected clips are
  outlined. While a drag lasts, a ghost shows where the clips will go. A
  warped audio clip's waveform is drawn where its audio plays, stretched
  between its warp markers ([Warping](../using/warping.md)).
- **Track headers** have the track's colour, its name, A, M, S and the arm
  button, and a meter, which can be hidden, moved and some resized
  ([Workspaces](../customising/workspaces.md#hiding-and-moving-parts)). A
  selected track's header and lane are lighter. The corner over the headers
  says what the grid is, such as "Grid 1/16". While Render Ahead renders the
  rest of the project ahead, a track that renders live has a **LIVE** mark
  at the end of its name, as does the master, and hovering it says why
  ([Render Ahead](../getting-started/audio.md#render-ahead)).
- **The master track** is pinned along the bottom, as Live's Main track is,
  and the tracks scroll over it, with room under the last one for a new
  track. Its header has its name, A and its meter, the same parts as the
  tracks' but for mute, solo and arm. Clicking its header, its lane or its
  strip in the mixer selects it, alone, so the devices panel shows the
  master's devices, and **Edit > Select Master Track** does the same.
  Devices dragged onto it go on the master. Dragging its top edge makes its
  row taller or shorter, and a drag on its lane selects time on it, as on a
  track's: from a track down into it, on the tracks from there down and on
  the master. The master has no clips for Cut, Copy or Delete to act on, but
  Toggle Loop, Zoom to Selection and exporting the selection take its time.
  With its automation lanes it takes at most half the arrangement's height,
  and the wheel over it scrolls them when they need more; in a short
  arrangement the tracks keep a track's height first.
- **Takes** show in lanes under their track, each named in its header, after
  **View > Show/Hide Take Lanes** (Ctrl+Alt+U) or the same in the track
  header's menu. Dragging across a take copies that stretch of it to the
  track: that's comping ([Recording and
  comping](../using/recording.md#comping)).
- **What tracks are recording** shows in red over their lanes, growing as
  they record, until each pass becomes a take.
- **Automation** shows in lanes under a track's lanes after the A button in
  its header, **Automation > Show/Hide Automation** (A), or the same in the
  header's menu ([Automation](../using/automation.md)).
- **The grid** follows the zoom, its lines at least 16 points apart, unless
  it's fixed (Ctrl+5), and Ctrl+1 and Ctrl+2 make it finer or coarser.
  Gestures snap to it while Snap to Grid is on (Ctrl+4).
- **Groups of bars** alternate between two shades across the lanes, so
  that 2, 4, 8 or 16 bars can be counted at a glance. A group is 1, 2, 4,
  8 or more bars, as many as are at least 96 points wide at the zoom, so
  zooming out makes the groups longer and zooming in shorter. Groups count
  from bar 1 and start at bar numbers on the ruler, and when the time
  signature changes in sight they keep the number of bars they have at the
  left edge. A theme's `bar_shade` and `bar_shade_width` change their shade
  and that width ([Themes](../customising/themes.md)).

## The mouse

The mouse does what it does in Live, until the Settings window's Mouse page
changes it ([The mouse](../customising/mouse.md)):

| Where | Gesture | What it does |
|---|---|---|
| A clip's title bar | Click; Ctrl- or Shift-click | Selects the clip; adds it to the selection or takes it out |
| | Drag; Ctrl-drag; Alt-drag | Moves the selected clips, also to other tracks of their kind; copies them; moves them off the grid |
| | Double-click | Renames the clip |
| A clip's body | Click | Selects the clip and puts the start marker there, so Ctrl+E splits it there |
| | Double-click | Opens a MIDI clip in the piano roll, and an audio clip in the Sample Editor |
| | Drag | Selects time, as on an empty lane |
| A clip's start or end | Drag; Alt-drag | Trims the clip, on the grid; off it |
| | Shift-drag | Stretches a warped clip, on the grid, changing its tempo so its audio fills it from the other edge; trims any other clip |
| A fade handle | Drag | Makes the fade longer or shorter |
| A lane | Click | Selects the track, or the master, and puts the start marker there |
| | Drag | Selects the time dragged over, on the tracks dragged over, and on the master's lane too if it's dragged over |
| The ruler | Click | Puts the start marker and the playhead there |
| | Drag | Zooms up and down, and scrolls sideways |
| The ruler's strip | Drag | Draws a new loop region, and turns looping on |
| The loop brace | Click; drag; drag an end | Turns looping on or off; moves the loop; moves its start or end |
| A locator | Click; drag; double-click | Jumps there; moves it; renames it |
| A track's header or strip, where the mixer strip has no control | Click; Ctrl- or Shift-click; double-click | Selects the track; adds it or takes it out; renames it. The master's header and strip select the master |
| A track's edge: its header's bottom edge, or the master's top edge | Drag | Makes the track taller or shorter |
| A take's lane | Drag; Alt-drag; click | Copies the stretch dragged over to the track, on the grid; off it; copies the clip clicked |
| A take's name, in its header | Double-click | Renames the take |
| An automation lane | Click; double-click | Selects the track, or the master, and puts the start marker there; adds a point there |
| | Drag; Alt-drag | Draws a value for each step of the grid crossed, as Live's Draw Mode does; draws freely, off the grid, as it does with Snap to Grid off |
| An automation lane's line | Click; drag | Adds a point on the line, on the grid; moves that stretch of the line, with its points, up or down |
| An automation point | Click; drag; Alt-drag | Deletes the point; moves it, on the grid and between the points either side; moves it off the grid |
| An automation lane's header | Click | Selects the track, or the master |
| Solo, in a header or a strip | Click; Ctrl-click | Solos the track alone; along with the others |
| Anywhere | Wheel; Shift-wheel; Ctrl-wheel | Scrolls up and down; sideways; zooms around the pointer |

Escape cancels a drag.

## Right-click menus

A right-click opens a menu of commands for what was clicked:

| Where | Commands |
|---|---|
| A clip | Cut, Copy, Duplicate, Split, Quantize and the grid |
| A lane | Insert MIDI Clip and Add Locator |
| A track's header | Rename, Delete Tracks, Show/Hide Take Lanes, Show/Hide Automation, Always Live and the new tracks |
| The master track's lane | Add Locator and Show/Hide Automation |
| The master track's header | Show/Hide Automation |
| The ruler | Locate and Toggle Loop |
| A locator | Rename and Delete Locator |
| A take | Comp, Rename, Delete Take and Show/Hide Take Lanes |
| An automation lane | Re-Enable Automation, Clear Automation and Hide Automation Lane |

Each shows its keys, and works on what the menu was opened on: the clip, the
track, the parameter, or the place. Rename, from a menu, a key or a
double-click, opens a box for the new name.

## Audio files and following the playhead

Audio files dropped on the window become clips, one after another, where
they're dropped: on that track if it's an audio track, or else on the
selected one or a new one ([Projects and
files](../using/projects.md#audio-files)).

While the transport plays, **Follow** (Ctrl+Shift+F) turns the page when the
playhead leaves the view. Scrolling away pauses it until the playhead is in
sight again.
