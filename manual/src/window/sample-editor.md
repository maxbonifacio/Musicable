# The Sample Editor

The Sample Editor shows an audio clip, as Live's clip view does: its
waveform against the song's beats, with the clip's controls beside it, its
warp markers along it and one of its curves under it. It shows the audio clip
selected last in the arrangement, or opened with a double-click, in a tab
beside the devices panel and the piano roll. Across, it shows song
positions, as the arrangement does, so the start marker, the playhead and
the grid line up with it, its lines dashed while nothing snaps to them; its
zoom and scrolling are its own. Everything you do in it can be undone, and a whole drag is one step in the undo history.

[Warping](../using/warping.md) says what warping a clip does.

## What it shows

- **The clip's controls** are in the column on the left, under the clip's
  name:
  - **Warp** turns warping on or off: whether the clip follows the song's
    tempo.
  - **The mode**, the menu beside it, is how a warped clip's audio plays
    between its markers: Re-Pitch, Slice, Granular or Spectral
    ([Modes](../using/warping.md#modes)).
  - **Slice's settings**, under the mode while it's Slice: **Preserve**,
    where it cuts the audio, **Loop**, what a slice plays once its audio
    runs out, and **Envelope**, how much each slice fades
    ([Slice](../using/warping.md#slice)).
  - **Granular's settings**, under the mode while it's Granular: **Follow
    pitch**, which fits the grains to the audio's pitch, as Live's Tones
    does, **Grain Size**, from 5 to 500 ms, how long each grain is, and
    while Follow pitch is off, **Fluctuation**, from 0 to 100%, how far the
    grains stray ([Granular](../using/warping.md#granular)).
  - **Spectral's settings**, under the mode while it's Spectral: **High
    quality**, which renders the clip with Rubber Band, as Live's Complex
    Pro does, **Formants**, from 0 to 100%, how far a transposed voice
    keeps its character, and while High quality is on, **Envelope**, from 8
    to 512, how finely it follows the outline of the spectrum it keeps them
    by ([Spectral](../using/warping.md#spectral)).
  - **The tempo** of a warped clip's audio, in BPM. Drag it, or click it and
    type a tempo from 5 to 999, which applies on Enter. **÷2** and **×2**
    halve and double it, as Live's buttons do: doubling it plays the audio
    half as fast, over twice the beats. An unwarped clip shows the tempo
    **Warp** would warp it at, greyed out.
  - **Transpose** and **Detune**, in semitones (st) and cents (ct), move a
    warped clip's pitch in every mode but Re-Pitch, where they're greyed
    out ([Transpose and Detune](../using/warping.md#transpose-and-detune)).
  - **Gain**, from silence to +24 dB: the clip's gain, where its gain curve
    doesn't say otherwise. Drag it, or click it and type a gain in dB.
- **The ruler** over the waveform has the song's bars, the start marker and
  the playhead, as the arrangement's does.
- **The waveform** is in the clip's colour where the file is, over the grid,
  as tall as the clip's gain makes it, and shaded before the clip's start
  and after its end, where it doesn't play.
- **Warp markers** are lines down the waveform with handles along its top,
  brighter while they're selected. While a drag lasts, the waveform shows
  where the audio will play.
- **Transients**, where the sounds in a warped clip's file start, are ticks
  along the top of the waveform, under the markers' handles, once Musicable
  has analysed the file. Near one, the waveform's top half shows a faint
  pseudo marker ([Transients](../using/warping.md#transients)).
- **The time selected**, a stretch of the clip dragged across, is shaded.
- **The ruler of the file's time**, under the waveform, counts the file's
  minutes and seconds from its first frame, where they play: closer
  together where a warped clip plays its audio faster.
- **The lane** at the bottom shows one of the clip's curves, its gain's
  until you choose another: the line through the curve's points, level
  before the first and after the last, or a dashed line at the setting in
  the column while it has no curve. The curve stays with the audio: it
  follows a warped clip's beats, and an unwarped clip's seconds. Under the
  lane's name, beside it, is the value under the pointer.
- **The lane's name** chooses the curve the lane shows, while the clip is
  warped: click it for a menu of the clip's settings that can follow a
  curve, those its mode plays first ([Curves in the
  clip](../using/warping.md#curves-in-the-clip)). The lane of a setting the
  mode doesn't play is grey, with "Not in Re-Pitch", say, under its name:
  the clip keeps its curve for the modes that play it. Preserve's and
  Loop's curves are steps, each point holding its value until the next.

## The mouse

The mouse does what it does in Live, until the Settings window's Mouse page
changes it ([The mouse](../customising/mouse.md)):

| Where | Gesture | What it does |
|---|---|---|
| A warp marker | Click; Ctrl- or Shift-click | Selects the marker; adds it to the selection or takes it out |
| | Drag; Alt-drag | Moves the selected markers, on the grid; off it. The audio either side stretches to fill the beats up to the next markers |
| | Shift-drag | Moves the audio under the marker, which stays on its beat |
| | Double-click | Deletes the marker |
| A transient's pseudo marker | Drag; Alt-drag | Adds a warp marker at the transient and moves it, on the grid; off it |
| | Shift-drag | Moves the transient, between the transients either side |
| | Double-click | Adds a warp marker at the transient, pinning it where it plays |
| The waveform's top half | Double-click | Adds a warp marker on the line before the pointer, pinning the audio that plays there |
| The waveform | Click | Clears the selected markers and time, and puts the start marker there |
| | Drag | Selects the clip's time dragged across, on the grid, with the warp markers in it and its transients |
| | Right-click | Opens a menu of the warp commands, for what was clicked ([Warping again from a point](../using/warping.md#warping-again-from-a-point)) |
| The lane | Click; double-click | Puts the start marker there; adds a point there |
| | Drag; Alt-drag | Draws a value for each step of the grid crossed; draws freely, off the grid, as it does with Snap to Grid off |
| The lane's line | Click; drag | Adds a point on the line; moves that stretch of the line, with its points, up or down, or on a lane of steps, that step |
| A point on the lane | Click; drag; Alt-drag | Deletes the point; moves it, on the grid and between the points either side; moves it off the grid |
| The rulers | Click; drag | Puts the start marker and the playhead there; zooms |
| | Double-click | Zooms to the selected markers, or with none selected, to the whole clip |
| Anywhere | Wheel; Ctrl-wheel | Scrolls sideways; zooms around the pointer |

A marker can't pass the markers either side, nor squeeze or spread the
audio between them past 5 or 999 BPM. Alt keeps any drag that snaps to
the grid off it while it's held, from whenever it's pressed: moving
markers, selecting time, and the lane's points and drawing. Escape cancels
a drag. A drag along the waveform held at its left or right edge scrolls
it. W, X and Shift+X fit the clip and step back and forward through the
Sample Editor's own history of views ([Getting
around](arrangement.md#getting-around)).

## The keys

Clicking in the Sample Editor gives it the focus. While it has it, these
act on the clip's warp markers:

| Keys | What they do |
|---|---|
| Ctrl+I | Inserts a warp marker at each transient in the time selected, or else at the start marker |
| Left, Right | Nudge the selected markers by the grid |
| Delete, Backspace | Delete the selected markers |
| Ctrl+A | Selects all the markers, and all the clip's time |
| Ctrl+Shift+I | Inserts a transient at the start marker |
| Ctrl+Shift+Backspace | Deletes the transients in the time selected |
| Ctrl+U | Quantizes the transients in the time selected, or all those the clip plays, to the grid ([Quantize](../using/warping.md#quantize)) |

The Clip menu has Warp, Double and Halve Clip Tempo, Warp Selection as a
Loop, and the marker and transient commands, for the clip the Sample Editor
shows. Hiding the
Sample Editor gives the arrangement the focus.
[Default keys](../reference/keys.md#in-the-sample-editor-panel) lists every
key that works in the Sample Editor.
