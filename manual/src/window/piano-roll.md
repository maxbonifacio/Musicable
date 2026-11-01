# The piano roll

The piano roll shows the notes of a MIDI clip, as Live's MIDI note editor
does: rows of keys beside a keyboard, under a ruler, over a lane of the
notes' velocities or a controller's curve. It shows the MIDI clip selected
last in the arrangement, or opened with a double-click. Across, it shows
song positions, as the arrangement does, so the start marker, the playhead
and the grid line up with it, its lines dashed while nothing snaps to
them; its zoom and scrolling are its own.
Everything you do in it can be undone, and a whole drag is one step in the
undo history.

## What it shows

- **Notes** take their track's colour, fainter the softer they start, with
  their names on them when there's room. Selected notes are outlined. While
  a drag lasts, ghosts show where notes will go.
- **The rows** of the black keys are darker, with a line under each C, where
  an octave starts. The clip's notes before its start or after its end are
  shaded, as they don't play.
- **The keyboard** names the Cs: middle C, MIDI note 60, is C3, as in Live,
  unless the Settings window's Behaviour page says otherwise
  ([Settings](../customising/settings.md#behaviour)).
- **The velocities** are stems at the notes' starts, as tall as the notes
  start hard.
- **The lane's name**, under the keyboard, opens a menu of what the lane can
  show: the velocities; the controllers the clip moves; those keyboards and
  pedals have, such as the mod wheel, the sustain pedal and CC 74, MPE's
  slide; then the rest of MIDI's 120.
- **A controller's curve** shows how the clip moves it: each point's value
  held until the next, filled under, in the track's colour, from the first
  point, as the clip leaves the controller alone before it. A drawing drag
  shows the curve as it will be. The curve before the clip's start and after
  its end is shaded.
- **Draw Mode** (B) turns clicks and drags on the rows into drawing, as in
  Live, and its button is in the Notes menu and the rows' right-click menu.

## The mouse

| Where | Gesture | What it does |
|---|---|---|
| A note | Click; Ctrl- or Shift-click | Selects the note; adds it to the selection or takes it out |
| | Drag; Ctrl-drag; Alt-drag | Moves the selected notes in time and to other keys, on the grid; copies them; changes their velocity |
| | Double-click | Deletes the note |
| A note's start or end | Drag; Alt-drag | Trims the selected notes, on the grid; off it |
| A row | Click | Clears the selection and puts the start marker there |
| | Double-click | Adds a note, a step of the grid long, on the line before the pointer |
| | Drag | Selects the notes in the rectangle dragged over |
| A row, in Draw Mode | Click; drag | Adds a note; draws a note at each step of the grid dragged over, on the key under the pointer |
| A note, in Draw Mode | Click | Deletes the note |
| A key | Click; Ctrl- or Shift-click | Selects the notes on the key; adds them |
| | Wheel; Ctrl-wheel; Alt-wheel | Scrolls up and down; makes the rows taller or shorter around the pointer; the same |
| The velocities | Drag | Changes the velocity of the note dragged, or the selected notes |
| A controller's lane | Click; drag | Draws the curve, a step of the grid at a time, at the pointer's height: the steps a fast drag skips get the values on the way. In MIDI's 128 steps, and off or on for a switch such as the sustain pedal, on from halfway up |
| | Alt-click; Alt-drag | Erases the curve there, so what came before holds through it |
| The ruler | Click; drag | Puts the start marker and the playhead there; zooms |
| | Double-click | Zooms to the selected notes, or with none selected, to the whole clip |
| Anywhere else | Wheel; Shift-wheel; Ctrl-wheel | Scrolls up and down; sideways; zooms around the pointer |
| | Alt-wheel | Makes the rows taller or shorter around the pointer |

Alt held as a drag of notes starts changes their velocity, but pressed once
they're moving or being trimmed, it keeps them off the grid until it's let
go, as in Live. Ctrl pressed part of the way through a move copies them,
and letting go of it moves them again. With Snap to Grid off, drawing a
controller fills steps of three points. Escape cancels a drag. A drag held
at the rows' edge scrolls them, sideways, and up and down for moving,
selecting and drawing notes, and the notes keep their place under it.
W, X and Shift+X fit the clip and step back and forward through the
piano roll's own history of views, and Ctrl+Shift+F turns its own Follow
on or off ([Getting around](arrangement.md#getting-around)). The Settings window's Mouse page
changes what the gestures do ([The mouse](../customising/mouse.md)).

## Right-click menus

| Where | Commands |
|---|---|
| A note | Cut, Copy, Paste, Duplicate, Delete, Quantize, Zoom to Selection and the grid |
| The rows | Paste, Select All, Draw Mode, Quantize and the grid |
| A controller's lane | Clear Controller and the grid |
| The ruler | Locate and Follow |

## The keys

Clicking in the piano roll gives it the focus, and clicking in the
arrangement gives the arrangement the focus back. While the piano roll has
it, the edit commands (Ctrl+X, C, V, D, A and Delete), Zoom and the arrow
keys act on its notes:

| Keys | What they do |
|---|---|
| Up, Down | Transpose the selected notes a semitone |
| Shift+Up, Shift+Down | Transpose them an octave |
| Left, Right | Nudge them by the grid |
| Shift+Left, Shift+Right | Shorten and lengthen them |
| Ctrl+Up, Ctrl+Down | Change their velocities |
| Ctrl+U | Quantize them |

Hiding the piano roll gives the arrangement the focus.
[Default keys](../reference/keys.md#in-the-piano-roll-panel) lists every
key that works in the piano roll.
