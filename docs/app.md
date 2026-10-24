# The app

`musicable` opens a window with the arrangement, the piano roll, the mixer,
the devices panel and the browser. It plays a project through the audio
device with the parallel scheduler, records audio and MIDI into takes,
plays CLAP and VST3 plugins and eight devices of its own, runs the commands
from keys, menus, the command palette and the mouse, and opens, saves and
exports projects. Every command's keys can be changed in the app. Undo and
redo cover every change to the project: the clips and their notes, the
takes, the locators, the loop, the tempo, the tracks, the devices, the
automation and the mix.

## Running it

```sh
cargo run --release -p musicable-app                      # an empty project
cargo run --release -p musicable-app -- path/to/song.musicable
```

On Windows, release builds open no console window, so double-clicking
`musicable.exe` starts the app without a command line. Debug builds keep the
console, and are much slower: the engine checks every allocation, and egui
runs unoptimised.

`musicable keymap [FILE]` still prints the keys, as
[commands.md](commands.md#trying-it-out) describes,
`musicable null-test [PROJECT]` checks the parallel scheduler against the
reference renderer (see [The null test](#the-null-test)), and
`musicable export PROJECT FILE` renders a project to audio files (see
[Exporting audio](#exporting-audio)).

## The Tier 0 demo

1. Start the app, and choose **File > New Demo Project**. Or open
   [`crates/io/tests/fixtures/v1/demo.musicable`](../crates/io/tests/fixtures/v1/demo.musicable),
   which is the same project saved: a dozen test tones through their gain
   stages, one of them muted, looping the first four bars at 124 BPM with the
   metronome on.
2. Press Space. It plays, loops and clicks.
3. **Options > Audio Settings** picks the device, shared or exclusive mode,
   and the buffer size. The corner of the menu bar shows the CPU load and
   counts dropouts.
4. **File > Save Project As**, then **File > Open Project**: the project comes
   back as it was. Saving the demo writes the same bytes as the fixture.
5. `musicable null-test` checks what you can't hear: that the parallel
   scheduler plays the demo exactly as the reference renderer does, before
   and after saving.

[`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs) runs
the same demo offline in every CI run, on Windows, macOS and Linux: it opens
the saved demo, plays it past the end of the loop with both renderers, and
saves it and opens it again.

## The Tier 1 demo

Record a MIDI part into a synth plugin and an audio take, comp the best
take, arrange the clips, mix with an EQ, a compressor and a reverb send,
automate a fader, undo and redo freely, save and reopen, and export a WAV
faster than real time:

1. **File > New Project** (Ctrl+N), then **File > Save Project As**
   (Ctrl+Shift+S) and **File > Open Project** (Ctrl+O) it again, so the
   recordings go in the project's folder: until it's opened again, a new
   project records into Unsaved Recordings ([Recording](#recording)).
2. **Track > New MIDI Track** (Ctrl+Shift+T). With it selected, double-click
   a synth in the browser's Devices (Ctrl+Alt+B shows the browser), such as
   "Surge XT (CLAP)". Then **Track > New Audio Track** (Ctrl+T): it records
   from the default input, and its strip's input menu picks another.
3. Arm both tracks, drag a loop of a bar or two on the ruler's strip, which
   turns looping on, and press **Record** (F9). Play the part on a MIDI
   keyboard or an MPE controller, sing or play the audio part two or three
   times round the loop, and press Space to stop. Each time round is a take.
4. **View > Show/Hide Take Lanes** (Ctrl+Alt+U), and drag across the best
   stretch of each take, to copy it to the track: that's the comp.
5. Arrange: drag clips to move them, Ctrl-drag to copy them, Ctrl+D
   duplicates, Ctrl+E splits at the start marker, and Delete deletes.
6. With the audio track selected, double-click **EQ** and then
   **Compressor** under Built-in in the browser's Devices, and set them in
   the devices panel (Ctrl+Alt+L). **Track > New Return Track**
   (Ctrl+Alt+T), give it the **Reverb** the same way, with its Mix at 100%,
   and drag each track's send to it in the mixer (Ctrl+Alt+M).
7. Press A for a track's volume lane, and draw in it: a dip, or a fade at
   the end.
8. Undo (Ctrl+Z) back to the empty project, recording and all, and redo
   (Ctrl+Y) all the way again: the song plays as it did.
9. **File > Save Project** (Ctrl+S), open another project, and **File > Open
   Project** (Ctrl+O) this one again. It plays as it did.
10. **File > Export Audio...** (Ctrl+Shift+R) writes the mix to a WAV file,
    and says how many times faster than real time it rendered.

[`crates/app/tests/tier1_demo.rs`](../crates/app/tests/tier1_demo.rs) runs
the same demo offline in every CI run, on Windows, macOS and Linux, through
the session and its commands, once with the test plugins' CLAP synth and
once with their VST3 one. Two times round a loop, it sings a phrase into
an input, with a cough in each take, and plays an arpeggio on a MIDI input,
then comps the takes around the coughs, arranges, mixes and automates,
undoes every step and redoes them, saves and reopens, and exports. It
checks what listening can't: that the takes hold what was played, on the
frame; that the comp plays the parts picked, to the sample; that undoing
everything leaves an empty project, and redoing it plays the song as a
fresh open of it does; that the project comes back from its file as it was
saved; that the parallel scheduler plays it as the reference renderer
does, sample for sample, with the CLAP synth's voices on the scheduler's
threads; and that the exported file is what the app plays. It prints how
fast the export rendered, which says little in a debug build.

## The null test

`musicable null-test` plays a project offline from the start of the song
with the reference renderer and with the parallel scheduler side by side,
and checks that they play it the same, sample for sample:

```sh
cargo run --release -p musicable-app -- null-test                  # the demo
cargo run --release -p musicable-app -- null-test song.musicable --seconds 60 --buffer 64
```

Each player opens the project in a session and presses Play, as the app
does, then renders one buffer at a time:

| Line | What plays |
|---|---|
| Reference renderer | The oracle: one node at a time, in order, on one thread |
| Parallel, as the app plays | The parallel scheduler with the app's options, which keep blocks with little work on the audio thread |
| Parallel, every block shared | The parallel scheduler sharing every block between all its workers |
| Saved and opened again | The project written as a project file and read back, played as the app plays it |

A line says `identical` or where the output first differed, how fast it
rendered, how many blocks the scheduler shared and with how many workers,
and how many threads ran nodes at once, on average, while blocks were
shared. The test fails if any output differed, or if the project came back
different after saving. `--seconds`, `--rate` and `--buffer` set how long
it plays, the sample rate and the buffer size: 20 seconds at 48 kHz in
128-frame buffers by default. A debug build checks the renders for
allocations too, as the tests do.

The null test doesn't load plugins: a plugin's device passes its signal
through, as a plugin that isn't installed does. Plugins may not render the
same every time they play the same, so only ones that do, such as the test
plugins, can be checked this way, as
[`crates/app/tests/tier1_demo.rs`](../crates/app/tests/tier1_demo.rs) checks
them ([The Tier 1 demo](#the-tier-1-demo)).

Audio clips play their files from the project file's folder, as in the app
(see [Audio files](#audio-files)). Clips that stream wait for the disk here
instead of missing what it hasn't read yet, so every run plays them the
same however fast it renders. Files that can't be read are listed before
the results, and their clips are silent in every run.

On a 4-core cloud VM:

```text
Null test of the demo: 12 tracks, 20 s at 48000 Hz in 128-frame buffers

Reference renderer             peak -2.7 dBFS, 0.23 s, 87x real time
Parallel, as the app plays     identical, 0.23 s, 89x real time; 14 of 7502 blocks shared, 3 workers, 0.8 threads busy
Parallel, every block shared   identical, 0.32 s, 63x real time; 7502 of 7502 blocks shared, 3 workers, 1.1 threads busy
Saved and opened again         identical, 0.24 s, 84x real time; 23 of 7502 blocks shared, 3 workers, 0.8 threads busy

Passed: the parallel scheduler played it exactly as the reference renderer did, before and after saving.
```

The demo's nodes are tiny, so the app's options render most blocks on the
audio thread alone, and sharing every block is slower than not sharing.
That's what `ParallelOptions::inline_below` is for
([engine.md](engine.md#parallel-rendering)).

## The window

- **The menu bar** lists the commands with their keys, under File, Edit,
  Track, Clip, Notes, Automation, Locator, Transport, Browser, View and
  Options. Undo and Redo name the change they would make, as in "Undo Set
  Tempo", and are greyed out when there's nothing to undo or redo, as is any
  command that doesn't apply now, such as Split with nothing selected.
  Toggles, such as Follow and Snap to Grid, are ticked while on. A command
  in an area with no menu yet gets a menu named after its area, before
  Browser, so every command can be reached; commands that need arguments
  are left out, as menus can't give them. On the right are the CPU meter and
  the dropout counter.
- **The transport bar** has go to start, play, stop, record, loop, punch in
  and punch out, the metronome, the tempo, the time signature, and the
  playhead in bars, beats and sixteenths and in minutes and seconds.
  Hovering a button shows its command and keys, hovering Loop shows the
  loop region, and Punch In and Punch Out where they punch
  ([Recording](#recording)).
- **The tempo field** sets the tempo where the playhead is, and moves any
  other tempos in the song by as much, as Tempo Up and Down do. Drag it, or
  click it and type a tempo, which applies on Enter. Each change is a "Set
  Tempo" transaction, and a whole drag is one step in the undo history.
- **The panels** fill the rest of the window, each with its tab, and can be
  moved about, sized and hidden ([below](#the-panels)).
- **The arrangement** is in the middle: the tracks over time, with their
  clips, under a ruler ([below](#the-arrangement)).
- **The mixer** is under the arrangement, with a strip for each track and
  the master's on the right (see [The mixer](#the-mixer)). **View > Mixer**
  (Ctrl+Alt+M) hides or shows it.
- **The devices panel** is under the mixer: the selected track's devices,
  or the master's, with Add Device to open the browser's devices (see
  [Built-in devices](#built-in-devices) and [Plugins](#plugins)). **View >
  Devices** (Ctrl+Alt+L) hides or shows it.
- **The piano roll** is in a tab beside the devices panel, as Live's clip
  view takes its device view's place. Double-clicking a MIDI clip, or
  **View > Piano Roll** (Ctrl+Alt+3), shows it, and F12 switches between
  the two ([below](#the-piano-roll)).
- **The browser** is a panel on the left with the samples, the devices and
  the presets ([The browser](#the-browser)), hidden until Add Device,
  **Browser > Search in Browser** (Ctrl+F) or **View > Browser**
  (Ctrl+Alt+B) shows it.
- **The status bar** shows errors and notices until they're dismissed, the
  audio device's sample rate, buffer, and latency as the device's clock
  measures it (click it for the audio settings), and the project's name,
  with "(not saved)" while it has changes. The window's title marks them
  with a `*`.
- **View > Command Palette** (Ctrl+Shift+P) finds any command by name or
  keys and runs it ([below](#the-command-palette)).
- **Options > Keyboard Shortcuts** (Ctrl+K) lists every command with its
  keys, and changes them ([below](#changing-keys)).
- **Options > Appearance** picks the theme Musicable draws with
  ([Themes](#themes)).

**Double-clicks** count when the second click comes soon enough after the
first: on Windows, within the double-click time in its mouse settings, as
in other Windows apps, which the app reads as it starts; elsewhere, within
egui's 0.3 seconds.

### The panels

The arrangement, the mixer, the devices panel, the piano roll and the
browser are docked panels, laid out in splits and tabs
([egui_tiles](https://docs.rs/egui_tiles)):

- **Drag a tab** onto another panel to split it, beside, over or under,
  where the blue preview shows, or onto a tab bar to join its tabs. A
  group of tabs moves together by its tab bar.
- **Drag the edge** between two panels to size them. A panel keeps its
  size as the window grows and shrinks, and when it moves, and the part
  with the arrangement takes what's left. A group of tabs is as big as the
  tab in front, until its edge is dragged, which sizes them all.
- **A tab's ×** hides its panel, as the View menu's command does, and the
  panel keeps its place: the View menu shows it there again, in front of
  the tabs beside it. The arrangement stays.
- **The devices and the piano roll follow the selection**: selecting the
  clip the piano roll shows brings it forward, selecting another track
  brings the devices forward, and so does adding a device. Neither comes
  back from hidden by itself.

Devices and presets dragged from the browser go in the devices panel's
chain wherever the panel is. The window opens laid out as it was left:
the app saves the layout to the default workspace's file as it changes
([Settings](#settings-and-your-keymap)).

## The arrangement

The arrangement shows the song's tracks over time, as Live's Arrangement
View does: the ruler along the top, a lane for each track with its clips,
and each track's header on the right. Everything it does runs a command
([commands.md](commands.md#clip)), so it can all be undone and given other
keys, and a drag runs one command when it ends, so it's one step in the
undo history and the engine rebuilds its graph once.

- **The ruler** has the bar numbers, and along its top, a strip with the
  loop brace and the locators. The start marker is a triangle at the
  ruler's foot, and the playhead a line through the lanes, drawn where the
  sound is: the engine's position less the latency of the plugins and the
  audio device ([engine.md](engine.md#delay-compensation)).
- **Clips** show their name in a title bar, and under it an audio clip's
  waveform, with its fades and their handles, or a MIDI clip's notes. A
  clip takes the colour of its track's place in the list. Selected clips
  are outlined. While a drag lasts, a ghost shows where the clips will go.
- **Track headers** have the track's name, M, S and the arm button, and a
  meter. A selected track's header and lane are lighter. The corner over
  the headers says what the grid is, such as "Grid 1/16".
- **Takes** show in lanes under their track, each named in its header,
  after **View > Show/Hide Take Lanes** (Ctrl+Alt+U) or the same in the
  track header's menu. A take's clips show their waveforms or their notes,
  grey where the track doesn't play them, and in the track's colour where
  it does: where its own clips play the same audio or the same notes from
  the same place, until one is moved or its notes change. Dragging across
  a take copies that stretch of it to the track, replacing what the track
  has there, and clicking one of its clips copies all of the clip: that's
  comping ([commands.md](commands.md#take)).
- **What tracks are recording** shows in red over their lanes, growing as
  they record, until each pass becomes a take ([Recording](#recording)).
- **Automation** shows in lanes under a track's lanes after the A button in
  its header, **Automation > Show/Hide Automation** (A), or the same in the
  header's menu: a lane for each parameter the track automates, or else
  one for its volume. The master's lanes go under the last track; the
  master strip's controls show them, in the mixer. A lane's header names
  the parameter, with its value at the playhead, or "Overridden" once
  someone moved its control by hand, and has three buttons: ▾ chooses the
  parameter the lane shows (the track's volume, pan and sends, then each
  device's parameters, a submenu for each device, the automated ones in
  red), + adds a lane for the next parameter, and × hides the lane. The
  line through a lane's points is red, or grey while its automation is
  overridden; a lane without automation shows the parameter's own value as
  a dashed line ([commands.md](commands.md#automation)).
- **The grid** follows the zoom, its lines at least 16 points apart, unless
  it's fixed (Ctrl+5), and Ctrl+1 and Ctrl+2 make it finer or coarser.
  Gestures snap to it while Snap to Grid is on (Ctrl+4).

The mouse does what it does in Live:

| Where | Gesture | What it does |
|---|---|---|
| A clip's title bar | Click; Ctrl- or Shift-click | Selects the clip; adds it to the selection or takes it out |
| | Drag; Ctrl-drag; Alt-drag | Moves the selected clips, also to other tracks of their kind; copies them; moves them off the grid |
| | Double-click | Renames the clip |
| A clip's body | Click | Selects the clip and puts the start marker there, so Ctrl+E splits it there |
| | Double-click | Opens a MIDI clip in the piano roll |
| | Drag | Selects time, as on an empty lane |
| A clip's start or end | Drag; Alt-drag | Trims the clip, on the grid; off it |
| A fade handle | Drag | Makes the fade longer or shorter |
| A lane | Click | Selects the track and puts the start marker there |
| | Drag | Selects the time dragged over, on the tracks dragged over |
| The ruler | Click | Puts the start marker and the playhead there |
| | Drag | Zooms up and down, and scrolls sideways |
| The ruler's strip | Drag | Draws a new loop region, and turns looping on |
| The loop brace | Click; drag; drag an end | Turns looping on or off; moves the loop; moves its start or end |
| A locator | Click; drag; double-click | Jumps there; moves it; renames it |
| A track's header | Click; Ctrl- or Shift-click; double-click | Selects the track; adds it or takes it out; renames it |
| A track's bottom edge | Drag | Makes the track taller or shorter |
| A take's lane | Drag; Alt-drag; click | Copies the stretch dragged over to the track, on the grid; off it; copies the clip clicked |
| A take's name, in its header | Double-click | Renames the take |
| An automation lane | Click; double-click | Selects the track and puts the start marker there; adds a point there |
| | Drag; Alt-drag | Draws a value for each step of the grid crossed, as Live's Draw Mode does; draws freely, off the grid, as it does with Snap to Grid off |
| An automation lane's line | Click; drag | Adds a point on the line, on the grid; moves that stretch of the line, with its points, up or down |
| An automation point | Click; drag; Alt-drag | Deletes the point; moves it, on the grid and between the points either side; moves it off the grid |
| An automation lane's header | Click | Selects the track |
| Solo, in a header or a strip | Click; Ctrl-click | Solos the track alone; along with the others |
| Anywhere | Wheel; Shift-wheel; Ctrl-wheel | Scrolls up and down; sideways; zooms around the pointer |

Escape cancels a drag. A right-click opens a context menu of commands from
the registry for what was clicked: Cut, Copy, Duplicate, Split and the grid
on a clip, Insert MIDI Clip and Add Locator on a lane, the new tracks on a
header, Locate and Toggle Loop on the ruler, Rename and Delete Locator on a
locator, Comp, Rename, Delete Take and Show/Hide Take Lanes on a take,
and Re-Enable Automation, Clear Automation and Hide Automation Lane on an
automation lane. Each shows its keys, and takes its arguments from where
the menu was opened: the clip, the track, the parameter, or the position.
Rename, from a menu, a key or a double-click, opens a box for the new
name.

Audio files dropped on the window become clips, one after another, where
they're dropped: on that track if it's an audio track, or else the
selected one or a new one ([Audio files](#audio-files)). While the
transport plays, **Follow** (Ctrl+Shift+F) turns the page when the playhead
leaves the view; scrolling away pauses it until the playhead is in sight
again.

**Themes and mouse maps.** The arrangement, the piano roll and the mixer
draw every colour and size from a `musicable_ui::Theme`, and look up what
each gesture does, from where it starts and the modifiers held, in a
`MouseMap`. `MouseMap::live()` is the table above and the piano roll's.
`Shell::set_theme` and `set_mouse_map` change them: the app sets the theme
from Options > Appearance and theme files ([Themes](#themes)), and the
mouse map is for the profiles of Tier 2. Each view is a panel with its own
id, such as `musicable_ui::MIXER` and `PIANO_ROLL`, which the shell's panel
registry (`crates/ui/src/panels.rs`) holds with its title, its place and
size in the default layout, and how it's drawn: the dock lays the panels
out by id, and the workspace file names them by it.

### How fast it draws

The arrangement is drawn with egui's painter: its shapes are tessellated
into triangles on the CPU each frame, and egui-wgpu draws them with wgpu.
Only what's in sight is laid out and drawn: the visible tracks, the clips in
view, a waveform column per point of width from peaks read at a few
resolutions (`musicable_ui::Peaks`), and the mixer strips in sight. The
`arrangement_frames` example measures a hundred tracks of 32 clips each,
audio with waveforms and MIDI with notes, in a 1920×1080 window:

```sh
cargo run --release -p musicable-ui --example arrangement_frames
```

On a 4-core cloud VM (Xeon at 2.8 GHz), 300 frames each:

| Frame | Built in | Tessellated in | Slowest frame | Triangles |
|---|---|---|---|---|
| Stopped, mixer shown | 2.46 ms | 1.18 ms | 11.23 ms | 63k |
| Stopped, mixer hidden | 2.30 ms | 1.17 ms | 9.80 ms | 61k |
| Playing, the playhead moving | 2.11 ms | 1.02 ms | 5.67 ms | 61k |
| Scrolling sideways every frame | 2.73 ms | 1.42 ms | 13.20 ms | 63k |
| Zoomed out to the whole song, tracks small | 1.52 ms | 2.29 ms | 5.24 ms | 101k |
| The same, scrolling down every frame | 1.55 ms | 2.23 ms | 10.39 ms | 104k |

So a frame takes 3 to 4 ms of a 16.7 ms frame at 60 Hz, a hundred tracks or
not; the slowest frames there are the shared VM's. Before the mixer drew
only its strips in sight, its hundred strips cost another 3 ms.

That settles the question the Tier 1 plan left open, whether the timeline
needs wgpu drawing of its own inside egui: it doesn't, for now. Drawing
with egui's painter keeps one way of drawing, which the tests can see
(`egui_kittest`) and themes can colour. If a later view needs far more
triangles, such as automation of hundreds of tracks zoomed out, the lanes'
drawing (`crates/ui/src/arrangement/paint.rs`) can move into an
`egui_wgpu::Callback` with a render pass of its own, with the gestures left
as they are.

## The piano roll

The piano roll shows the notes of a MIDI clip, as Live's MIDI note editor
does: rows of keys beside a keyboard, under a ruler, over a lane of the
notes' velocities or a controller's curve. It shows the MIDI clip selected
last in the arrangement,
or opened with a double-click. Across, it shows song positions, as the
arrangement does, so the start marker, the playhead and the grid line up
with it; its zoom and scrolling are its own. Everything it does runs a
command ([commands.md](commands.md#notes) and
[controllers](commands.md#controllers)), and a drag runs one when it ends,
so it's one step to undo.

- **Notes** take their track's colour, fainter the softer they start, with
  their names on them when there's room. Selected notes are outlined. While
  a drag lasts, ghosts show where notes will go.
- **The rows** of the black keys are darker, with a line under each C,
  where an octave starts. The clip's notes before its start or after its
  end are shaded, as they don't play.
- **The keyboard** names the Cs: middle C, MIDI note 60, is C3, as in
  Live, unless the `middle_c` setting says otherwise
  ([below](#settings-and-your-keymap)).
- **The velocities** are stems at the notes' starts, as tall as the notes
  start hard.
- **The lane's name**, under the keyboard, opens a menu of what the lane
  can show: the velocities; the controllers the clip moves; those
  keyboards and pedals have, such as the mod wheel, the sustain pedal and
  CC 74, MPE's slide; then the rest of MIDI's 120.
- **A controller's curve** shows how the clip moves it: each point's value
  held until the next, filled under, in the track's colour, from the first
  point, as the clip leaves the controller alone before it. A drawing
  drag shows the curve as it will be. The curve before the clip's start
  and after its end is shaded.
- **Draw Mode** (B) turns clicks and drags on the rows into drawing, as in
  Live, and its button is in the Notes menu and the rows' context menu.

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
| | Wheel; Ctrl-wheel | Scrolls up and down; makes the rows taller or shorter around the pointer |
| The velocities | Drag | Changes the velocity of the note dragged, or the selected notes |
| A controller's lane | Click; drag | Draws the curve, a step of the grid at a time, at the pointer's height: the steps a fast drag skips get the values on the way. In MIDI's 128 steps, and off or on for a switch such as the sustain pedal, on from halfway up |
| | Alt-click; Alt-drag | Erases the curve there, so what came before holds through it |
| The ruler | Click; drag | Puts the start marker and the playhead there; zooms |
| Anywhere else | Wheel; Shift-wheel; Ctrl-wheel | Scrolls up and down; sideways; zooms around the pointer |

Live turns off the grid with Alt while a drag goes, but Alt-dragging a note
changes its velocity there, so moving notes off the grid is Snap to Grid
(Ctrl+4) turned off. Escape cancels a drag. A right-click opens a context
menu of commands from the registry, as the arrangement's do: Cut, Copy,
Paste, Duplicate, Delete, Quantize and Zoom to Selection on a note; Paste,
Select All, Draw Mode and Quantize on the rows; the grid on both; Clear
Controller and the grid on a controller's lane; and Locate and Follow on
the ruler. With Snap to Grid off, drawing a controller fills steps of three
points.

**The focus.** Clicking in the piano roll gives it the focus, and clicking
in the arrangement gives the arrangement the focus back. While the piano
roll has it, the edit commands (Ctrl+X, C, V, D, A and Delete), Zoom and the
arrow keys act on its notes: Up and Down transpose, Shift with them by
octaves, Left and Right nudge by the grid, Shift+Left and Shift+Right
shorten and lengthen, Ctrl+Up and Ctrl+Down change velocities, and Ctrl+U
quantizes ([commands.md](commands.md#notes)). Hiding the piano roll gives
the arrangement the focus.

**For Tier 2's extensions and profiles.** What the piano roll shows, its
selection, its zoom and the focus are the `Editor`'s, beside the
arrangement's, where commands and agents read and change them; its context
menus list commands from the registry; its colours and sizes are in the
`Theme` (`theme.piano_roll` and the piano roll's colours); its gestures are
the `MouseMap`'s targets `Note`, `NoteEdge`, `NoteGrid`, `DrawNote`,
`DrawGrid`, `PianoKey`, `VelocityLane` and `ControllerLane`; what its lane
shows is the editor's too (`PianoRoll::lane`); its panel's id is
`musicable_ui::PIANO_ROLL`; and what middle C is called is a behaviour
setting.

## The command palette

The command palette lists every command as "Area: Title", with its keys.
Typing narrows the list:

- Every word typed has to match. A word matches best at the start of a word
  of the name ("tog", "loop"), then inside one, or as the initials of its
  words ("tl" is Toggle Loop), then in the command's id.
- Keys find their command: "ctrl+l" lists Toggle Loop first, and "ctrl+shift"
  lists every command with a Ctrl+Shift chord while you type the rest.
- The commands run from the palette lately come first among those that
  match as well.

Up and Down choose a command, and Enter or a click runs it and closes the
palette. Escape or a click outside closes it. While it's open, keys type
into it and run no commands.

## Changing keys

Options > Keyboard Shortcuts lists the commands by menu, each with its keys.
The search field at the top finds commands the way the command palette
does, by name or by keys.

- **Click a key** to change it, or **+** to add one. The window waits for
  the keys, and any keys can be pressed, Escape, Enter and Tab included;
  they run nothing while it waits. Then it says what the keys do now:
  - "isn't used yet": **Assign** (or Enter) gives them to the command.
  - "runs *another command* now": **Reassign** (or Enter) takes them from
    that command.
  - "runs *this command* already": there's nothing to do.

  **Try Other Keys** waits for keys again. **Cancel**, Escape, or a click
  anywhere else stops without changing anything.
- **×** next to a key removes it.
- **Reset** shows on a command whose keys aren't the default ones, and gives
  it its default keys back. If another command has one of them now, the
  tooltip says so, and Reset takes it.

Every change is saved to your keymap file at once
([below](#settings-and-your-keymap)), as the changes to the default keymap.
Under the list:

- **Mistakes in your keymap file**, as [commands.md](commands.md#mistakes)
  describes. A chord that two lines claim runs none of their commands, and
  gets a button for each to give it to. Keys for commands this version
  doesn't have can be removed.
- **Import...** uses the keys in another keymap file in place of yours. What
  it gets wrong is left out, and listed.
- **Export...** saves your keys to a keymap file, to share or keep.
- **Reset All** gives every command its default keys again.
- **Reload** reads your keymap file again after you edit it, and **Create
  It** creates it when there's none yet, to edit.
- Escape closes the window, once it isn't waiting for keys.

## The mixer

Each track has a channel strip, in the project's order, and the master's
strip is on the right. Tracks scroll sideways when they don't fit, and only
the strips in sight are drawn. From the top, a strip has:

- **The name.** Hovering it shows the track's kind and test tone.
  Right-clicking it has **Solo-Safe** and **Delete Track**.
- **The input**, on audio and MIDI tracks: what the track records from
  ([Recording](#recording)).
- **Monitoring**, on audio and MIDI tracks: **In**, **Auto** or **Off**.
- **The output**: the master, or a group track the track can feed. Return
  tracks always feed the master.
- **A send to each return track**, from silence to 0 dB, with **Pre** to
  take the signal before the fader instead of after it. Dragging a send that
  isn't there yet adds it, and right-clicking one has **Remove Send**.
- **The pan**, from 50L to 50R. It's a balance control: panning turns the
  other side down and never boosts.
- **The fader**, from silence to +6 dB, beside the meter and its scale.
- **The gain** in dB, to drag or type into, and **the highest peak** since
  the peaks were cleared, red once the channel has gone over full scale.
  Clicking a peak clears every channel's.
- **M, S and the arm button**: mute, solo, and arm for recording, which
  audio and MIDI tracks have. Clicking S solos that track alone, as in Live;
  Ctrl-click (Cmd-click on a Mac) solos it along with the others.
- **The clip stage**: Off, Hard or Soft.

Drag a fader, a send or the pan to move it, with Ctrl (Cmd on a Mac) held
to move it finely. Both gestures come from the mouse map, as the
arrangement's do. Double-click a fader for 0 dB, a send to silence it, or
the pan to centre it. A whole drag is one step in the undo history. The
faders, sends, pans, buttons, menus and meters are named for screen readers,
such as "Lead Volume", which is how the tests drive them.

**Automation.** A fader, a send or a pan with automation follows it, showing
its value at the playhead, and has a red dot at its top left; the dot is
grey once the control is moved by hand, which overrides the automation
until it's re-enabled, as in Live. Right-clicking one of them has **Show
Automation**, which shows its lane in the arrangement, **Re-Enable
Automation** and **Clear Automation**.

**Meters.** Each strip's meter shows its channel's output, after mute and
solo, left and right: the peak in a light shade, and the RMS level, averaged
over about 0.3 s, solid. They're green up to -6 dB, yellow to 0 dB and red
over. The peak falls back at 20 dB a second once the signal drops. The
meters read the engine's `Meters` every frame, and the window redraws at
60 frames a second while any of them shows a signal, so they fall all the
way back after you stop.

The **Track** menu adds tracks (see [commands.md](commands.md#track)). The
mixer's controls hand back `Action::Mixer` with a `mixer::Change`, which the
app runs as its command with `Session::change_mixer`
([commands.md](commands.md#the-mixers-controls)).

## Recording

Arm a track with its arm button, in its header or its strip, choose its
input in the strip's input menu, and press **Record** (F9) in the transport
bar. Playing starts from the start marker, as Space does, and every armed
track records its input while the transport plays: an audio track what its
audio input captures, a MIDI track the notes its MIDI input plays. Record
while playing turns recording on or off there. Stop ends the recording, and
each pass of the playhead over the song becomes a take on its track
([commands.md](commands.md#take)): one take for a recording straight
through, and one for each time round the loop while looping, the last of
them on the track. One undo takes the whole recording back.

**Punch In** and **Punch Out**, beside Loop in the transport bar, record
only from the loop region's start on, and only up to its end. With both on
and looping on, each time round the loop records the region and nothing
else.

**Inputs.** An audio track records from one of the inputs the audio
settings' device or another input device has, or on Windows from the
loopback of an output device: everything the system plays there, listed as
"Loopback:" and the output's name. A new audio track takes the default
input in stereo, "Default 1/2": the system's default recording device, or
with an ASIO driver, the driver's own first inputs. The input menu offers
each input's pairs of channels and each channel on its own, and lists the
inputs again when it opens, for a device plugged in since. An input that
isn't plugged in keeps its name, in red. The stream opens the inputs that
armed tracks and tracks monitoring In take, and keeps them open until the
device changes. Opening one restarts the stream, so it waits while
recording. An input that doesn't open, such as one another app has to
itself, says so in the status bar, and a track taking it records nothing.

**MIDI inputs.** A MIDI track records from one MIDI input or all of them,
and from one MIDI channel or all sixteen. A new MIDI track takes every
channel of every input, "All Inputs", as an MPE controller needs: its
channels read as MPE ([engine.md](engine.md#notes)), and a keyboard's
single channel plays as it is. The input menu lists All Inputs and then
each MIDI input plugged in, each with **All** and its channels 1 to 16,
and lists them again when it opens. The stream opens the MIDI inputs that
armed MIDI tracks and MIDI tracks monitoring In take, while they're plugged
in, and keeps them open. While tracks want any, the app looks at what's
plugged in every two seconds, except while recording: it opens an input
plugged in since that a track takes, or one plugged in again after it was
pulled out. MIDI inputs open on Windows only so far
([audio.md](audio.md#midi-inputs)).

**Monitoring.** In plays the track's input instead of its clips. Auto, a new
track's, plays it while the track is armed and the transport is stopped or
recording, so you hear yourself as you record and the takes as you play
them back. Off never does, for an interface that monitors itself. The input
plays through the track's devices, the stream's monitoring latency late
([audio.md](audio.md#inputs)). A MIDI track plays its input's notes on its
instrument, with Auto whenever it's armed, as in Live, the stream's MIDI
delay late ([audio.md](audio.md#midi-inputs)).

**Where it lands.** A track records what its input captured as each song
position was heard, so it lands where it was played: the audio backend
lines the inputs' buffers up with the output, frame for frame, and the
engine's timeline says which song position each frame played, and how late
the plugins' latency made it ([engine.md](engine.md#recording)). Latency
the driver doesn't report, such as an interface's converters', is made up
for by the device's **recording offset**: each audio take starts that many
frames later in what its input captured. **Options > Audio Settings** sets
it for the device playing, under Now playing, and the
[settings](#settings-and-your-keymap) keep one for each device, mode and
sample rate. The record round trip example below measures it. A MIDI
message lands the same way as audio, without the offset: the backend stamps
each with the frame heard as it came in.

**Notes.** A MIDI track records the notes played, with their velocities,
their release velocities and their expression: pitch bend, pressure and
slide, each note's own with MPE. Each pass with notes is a take with a MIDI
clip as long as the pass; a pass without notes makes no take. A note held
as recording punches in isn't recorded, and one held as it punches out or
stops ends there. Round a loop, a note held over the loop's end ends there
and starts again at the loop's start, in the next pass. It records how the
controllers moved too, such as the sustain pedal and the mod wheel, as the
clip's [controller curves](#the-piano-roll), each pass starting with them
where they are: a pedal held down as a pass starts holds its notes in the
take as it did while recording.

**Files.** Recordings are 32-bit float WAV files at the engine's sample
rate, in a `Recordings` folder in the project's folder, named after the
track and numbered, as in `Lead 1.wav`. A project that has never been saved
records into `Music/Musicable/Unsaved Recordings` in your home folder, and
its clips name those files by their whole paths. A file's header says how
long it is as of the last second, so a crash loses a second at most. A
loop recording is a file for each track, its passes one after another, and
a file that reaches the 4 GB a WAV file can hold goes on in another, in
the same take.

**The recorder** (`musicable-recorder`, a thread of its own) reads the
engine's timeline and the inputs' buffers every 10 ms, and writes what the
armed tracks record. The audio thread never waits for it. However late it
runs, what the session sent it before the transport changed, such as a
stream's inputs or the tracks just armed, takes effect before it follows
the change. The inputs' buffers hold 8 seconds at least: if the recorder falls further behind than
that, or an input stops capturing for two seconds, the recording has silence
there, and the status bar says how much. A MIDI track's recording waits for
its inputs to bring every message up to where it records, which the stream
says 5 ms behind what's heard, for two seconds at most. A MIDI input's
buffer holds 16,384 messages: if the recorder falls further behind, the
status bar says how many are missing. A recording that the engine's
replacement cuts short, for new audio settings, ends where it got to. One
that finishes after its project was closed doesn't go into the project open
then, and the status bar names its files.

`Session::set_inputs` hands the session a stream's input layout and
buffers, the MIDI inputs' too, `Session::wanted_inputs` and
`wanted_midi_inputs` say which inputs the tracks want open, and
`Session::recordings` what the tracks have recorded so far, for the
arrangement. `crates/app/tests/recording.rs` records a second of a ramp
through the session, checks it landed on the frame, and undoes it, and
records a note on a MIDI track the same way;
`crates/app/tests/mpe.rs` records an MPE performance, with the sustain pedal
and the mod wheel, into a synth that takes only MPE MIDI, and checks the
take plays back into it as it played, to the sample;
`crates/app/src/recorder/tests.rs` checks the recorder's passes, punching,
waiting for and giving up on inputs, and stopping, for audio and MIDI
tracks, the notes held over loops and punches, and where the controllers
start each pass.

The record round trip example records on real devices as the app does,
through its own `Output`, `Session` and recorder. One track plays clicks
out while an armed track records them back in, from an output's loopback or
through a cable from an output to an input. It says where each click
landed, to a fraction of a frame, then comps the take and undoes that and
the recording. With `--minutes`, it records for that long and checks that
nothing was lost. Through a loopback, a click lands on the frame it played;
through a cable, late by the converters' latency that the drivers don't
report, the same every time: that's the recording offset to set for the
device, in that mode and at that rate. It doesn't use the offsets in the
settings. In WASAPI's shared mode it can differ by a few frames each time
the device opens.

```sh
cargo run --release -p musicable-app --example record_round_trip -- --loopback
cargo run --release -p musicable-app --example record_round_trip -- --output RME --input RME
cargo run --release -p musicable-app --example record_round_trip -- --asio
cargo run --release -p musicable-app --example record_round_trip -- --loopback --minutes 60
```

Under Wine, its clicks wander by up to 10 ms, as the audio crate's round
trip's do there ([audio.md](audio.md#trying-it)). That says nothing of
Windows.

After a new project's first save, it records into the Unsaved Recordings
folder until it's opened again.

### Capture

**Capture** (Ctrl+Shift+C), beside Record in the transport bar and in the
Transport menu, records what you just played without recording, as Live's
Capture MIDI does, for audio tracks as well as MIDI tracks. The recorder
holds the last few minutes of what the inputs of armed tracks, and of
tracks monitoring In, bring. Capture makes what they played over the
transport's last run into takes, as if they had recorded it: the same
files, passes and takes, a pass for each time round the loop, in one step
of the undo history, named Capture.

The last run goes from where the transport last started playing to where
it stopped, or to now if it still plays. Stretches the transport recorded
are left out, since they're takes already, and so is what came in while
it was stopped, which has no place in the song. A run longer than the
inputs are held is captured from as far back as they go. A capture asked
for while playing waits for the inputs to bring what was heard up to then,
as a recording does, for two seconds at most. With nothing to capture, the
status bar says why.

**How long.** `capture_minutes` in the settings
([settings.toml](#settings-and-your-keymap)) is how long each input is
held, 5 minutes by default; 0 turns Capture off. An audio input is held as
32-bit floats at the engine's sample rate, rounded up to a power of two of
frames: at 48 kHz, 5 minutes of a stereo input take 128 MiB. A MIDI input
holds 2,000 messages for each second, 24 MiB for 5 minutes: one that sends
more than that on average, such as an MPE controller played throughout,
holds less than the whole length. Tracks on the same input channels share
what's held. The held inputs take 2 GiB at most together: an input that
would go past that isn't held, and the status bar names the tracks it's
for.

What's held is dropped when another project opens, when
`capture_minutes` changes, and when new audio settings replace the engine.
An input that no armed or monitoring-In track takes any more stops being
held. Holding starts again with what the inputs bring next.

Every 10 ms, the recorder copies each held audio input's channels from the
stream's 8-second buffer into a buffer of its own, at the same engine
frames, and each held MIDI input's messages, with the frames heard. It
keeps the engine's timeline as far back as that and a minute more, for the
plugins' latency. Capturing runs the recording's own code over what's
held, on the run's timeline as a recording would have had it: recording on
throughout, and recording wherever the transport didn't
(`crates/app/src/recorder/capture.rs`). `Session::capture` asks for a
capture, and `Session::set_capture_seconds` sets how long inputs are held.
`crates/app/tests/recording.rs` captures a second of a ramp on an armed
track and on one monitoring In, and a note on a MIDI track, through the
session. `crates/app/src/recorder/tests.rs` checks where captures land,
loops, leaving out what was recorded, the last run, waiting for inputs and
the timeline, how far back inputs are held, new streams, shared and
refused inputs, and why there's nothing to take, for audio and MIDI.

## Built-in devices

Musicable has devices of its own, which play without plugins: an EQ, a
compressor, a gate, a delay, a reverb, a utility, a sampler and a synth.
Each is a node in the engine ([engine.md](engine.md#built-in-devices)),
and the project saves its settings ([project-format.md](project-format.md)).
The sampler and the synth play their track's notes, from its clips or its
MIDI input, each note bent on its own, as MPE bends them; a note's
pressure and slide don't move them yet, and nor do controllers, such as
the sustain pedal.

The plugin browser lists them under **Built-in**, above the plugins, and
finds them by name; All, Instruments and Effects pick among them too. A
double-click puts one at the end of the selected track's devices, or the
master's, with every control at its default.

In the devices panel, a built-in device shows its controls in columns, a
column for each group of them, such as one of the EQ's bands, under the
group's name, and more columns, side by side, for a group of more than
five:

- **A bar** for each number, filled as far as the value is along its range
  (from zero, for a range either side of it, such as a gain in dB), with
  its name and value on it. Drag it sideways, or Ctrl-drag (Cmd on a Mac)
  to move it a tenth as far; the mouse map's `DeviceControl` target says
  which. A double-click gives the default, and the right-click menu takes a
  typed value: a number with or without its unit ("1.2k", "1.2 kHz",
  "-6 dB", "250 ms"), a note's name ("C#3") or a pan ("20L"), applied on
  Enter. Controls with whole steps, such as the synth's voices, move in
  whole steps however slowly they're dragged.
- **A button** for each switch, lit while it's on.
- **A menu** for each choice, such as a band's shape.

Every change is a step in the undo history named after the device, such
as "Change EQ", and a whole drag is one. The engine hears each as it's
made, and smooths it over 10 ms, or moves it its device's own way: the
delay fades in a new time over 50 ms, and the reverb glides to a new
size. Screen readers find each control by the device's name and the
control's, such as "EQ Band 3 Freq", and can set a bar to a number.

A sampler shows the name of its sample's file beside its own, and **Load
Sample...** asks for an audio file for it to play (WAV, AIFF or FLAC), in
the project's folder; right-click the name to remove it. As for an audio
clip, the project keeps the file's path relative to its folder if it's in
there. The sampler reads the whole file when the graph is next built, and
samplers playing the same file share it; if it can't, it plays nothing,
and the status bar says why. Loading and removing a sample are steps in
the undo history: "Load Sample" and "Remove Sample".

**Automation.** Every control of a built-in device can be automated. One
with automation follows it, showing its value at the playhead, with the
mixer's dot at its top left: red, or grey once the control is moved by
hand, which overrides the automation until it's re-enabled
([commands.md](commands.md#automation)). Right-clicking a button or a menu has **Show
Automation**, **Re-Enable Automation** and **Clear Automation**, and a
bar's right-click menu has them under the typed value. In the
arrangement, a lane's ▾ lists every parameter of a track's built-in
devices, and the lane spreads the values as the control does (a frequency
on a log scale, say) and names them as it does ("1.20 kHz").

## Plugins

The app finds CLAP and VST3 plugins in each format's usual places, and in
the folders in `CLAP_PATH` and `VST3_PATH`
([plugins.md](plugins.md#scanning)). It scans in the background
as it starts, with the plugins the last scan found ready at once, and
loads only the libraries that are new or changed since. A library that
crashes, hangs or fails as it loads is left out, and the status bar says
which; **Options > Rescan Plugins** tries those again too.

**The browser's Devices** list Musicable's own devices under **Built-in**,
then the plugins found by name under **Plugins**: all of them, or only the
instruments or the effects. Hovering one shows its vendor, its
kind and its format. A plugin installed in both formats is listed in each,
with its format after its name: "Surge XT (CLAP)" and "Surge XT (VST3)".
The search field finds plugins by name or vendor, the
best matches first. Double-click a device or a plugin to put it at the end
of the selected track's devices, or the master's while the devices panel
shows those. Or drag it: into the devices panel, onto a track for the end of
its devices, or below the tracks for a new track named after it, a MIDI
track for an instrument and an audio track for an effect. The list's foot
says how many plugins there are, and has Rescan, or says "Scanning..."
while a scan runs.

**The devices panel** shows the devices of the selected track, which a
click on its header selects, in the order the signal goes through them,
left to right. The buttons above them pick the selected track's or the
master's. A built-in device has its name and its controls
([above](#built-in-devices)). A plugin device has its name, its kind and
format ("Instrument · CLAP"), and:

- **Plugin Window**, which opens the plugin's own window, or closes it.
  Double-clicking the device's name opens it too, as in Live. On Windows
  the plugin's window goes inside a window of Musicable's that stays in
  front of the main window and minimizes with it; elsewhere it's a window
  of the plugin's own. VST3 plugins have no windows of their own, so
  theirs open only on Windows so far.
- **×**, which deletes the device.
- Why the plugin isn't playing, when it isn't: it isn't installed, it
  didn't load, or the scan that finds it hasn't finished. A device whose
  plugin isn't playing lets the signal through as it is, and keeps its
  settings for when the plugin is there.

Adding, deleting and every change made in a plugin's window are steps in
the undo history, named after the plugin, such as "Change Surge XT"; a
whole drag of a knob is one. What a plugin changes on its own, as it loads
or takes in what undo gave it, or with its window closed, goes into the
project without a step, so redo carries on past it. Saving the project saves each plugin's state
as it is then. Delay compensation lines up every track with the latest
one, so a plugin that looks ahead doesn't put its track out of time
([engine.md](engine.md#delay-compensation)). [plugins.md](plugins.md)
has how the app hosts plugins.

## The browser

The browser is the panel on the left, with three lists: Samples, Devices
and Presets. Ctrl+F shows it with the keys in the search field of the list
showing, and Up and Down there move through what it lists. What the browser
does is commands (`browser.*` and the device commands,
[commands.md](commands.md#browser)), so keys, the command palette and
agents reach it too, and a search an agent runs shows in the browser.

### Samples

**The library** is the folders of samples you add, with the + at the foot
of the list, or **Browser > Add Folder to Library...**. The app looks
through each folder and the folders in it for WAV, AIFF and FLAC files,
reading each one's header, and keeps what it finds in `library.json` in
the config folder ([Settings](#settings-and-your-keymap)), so the next
start has the library at once. Each start looks through the folders again
in the background, reading only the files whose size or time of change
differ, and **Browser > Rescan Library** does it when asked; a file that
moved within its folder keeps its tags. A folder's right-click menu has
Rescan and Remove from Library, which leaves the files where they are. A
folder can't be added twice, nor one inside a folder of the library.

**Browsing and searching.** With the search field empty, the list is the
folders of the library, which a click opens and closes: each folder's
folders first, then its files, each in order of name. Typing in the field
searches as you type: every word has to be in a file's name, its folders
or its tags, and the best matches come first, those whose names have the
words before those whose folders do, and a word that starts a word of the
name before one inside it, so "kick" finds "Kick_01" before "Sidekick".
The list shows the first thousand files found, with their folders, and the
foot says how many there are in all. Searches of a quarter of a million
files take a few milliseconds ([below](#how-fast-it-finds-things)).

**A sample's menu** has Preview, Insert, Find Similar, and its tags. Find
Similar lists the files most like it, "Like Kick 01" above them, until ×
goes back to the search: those whose names share the most words with its
name, then those in the same folder, then those nearest its length. The
names and lengths are all it goes on, until analysis
([below](#not-done-yet)) gives sounds an embedding to compare. The tags
are yours: type one in New tag and press Enter to give it, and click one
to take it away. Searches find them, and the library file keeps them. They
aren't in the project's undo history, as they aren't the project's.

**Previews.** Clicking a sample plays it, beside the song, while Auto, at
the foot, is on. The strip there shows its waveform with where it's
playing, which a click plays from the start again; Stop stops it, and the
slider sets how loud previews play, from -60 to +6 dB. Previews play after
the master, so they're never in an export, and a file at another sample
rate is resampled. A file up to about twelve seconds long at 44.1 kHz is
read whole to preview it, and a longer one streams. The browser remembers
Auto and the volume in `settings.toml`.

**Onto tracks.** Double-click a sample, or pick Insert, to put it on the
selected audio track at the start marker, or on a new audio track named
after it, as an imported file goes
([Audio files](#audio-files)). Or drag it onto a track, where it starts
at the grid line nearest where it's dropped, or below the tracks for a new
track there. While a sample, a device or a preset is dragged, the place it
would go is outlined, and "New track" shows below the tracks.

### Devices

The Devices list is Musicable's own devices and the plugins the scans
found, which go on tracks as the samples do ([Plugins](#plugins)).

### Presets

**Presets** are a device's settings in a file, a plugin's state and all.
**Save as Preset...** in a device's right-click menu in the devices panel
asks for a name, and saves the device as a Musicable preset: a
`.musicable-preset` file in a folder named after the device in the
Presets folder, `Music/Musicable/Presets` in your home folder, or the
`[browser]` settings' `presets_folder`. A name the device has a preset for
already is refused. The Presets list also finds the VST3 presets
(`.vstpreset`) in VST3's preset folders:

| System | Folders |
|---|---|
| Windows | `Documents\VST3 Presets`, `%PROGRAMDATA%\VST3 Presets` |
| macOS | `~/Library/Audio/Presets`, `/Library/Audio/Presets`, `/Network/Library/Audio/Presets` |
| Linux | `~/.vst3/presets`, `/usr/share/vst3/presets`, `/usr/local/share/vst3/presets` |

The list groups presets by device, finds them by name and device as the
samples are found, and leaves out those for plugins that aren't
installed, which its foot counts. Double-click a preset to add its device
to the chain the devices panel shows, set as it was saved, or drag it
there, onto a track, or below the tracks for a new track. Adding a preset
is a step in the undo history named after it. A sampler's preset names its
sample in full, so it plays in any project; added to a project whose folder
has the sample, the sampler names it from there, as clips name their files.

### Behind the browser

**One queue for file work.** Reading the library file, looking through
folders, finding presets, opening the files to preview and drawing their
waveforms all happen on one queue of the library's own
(`musicable_library::Queue`), never on the window's thread or the
engine's workers. It has a thread for every two cores, one to four of
them, called `musicable-files-1` and up, which run below normal priority.
A preview comes first, then a waveform, then scans. Scans run on every
thread while the transport is stopped and on one while it plays, and
while recording they rest as long as they work, so the disk and the cores
are left to the recorder and the engine. On Windows and macOS they also
run in the system's background mode, which puts their reads behind
others. Analysis will run on it too.

**The library file** has a record for each file, with room for what
analysis will add: tags with where each came from (the name, a
measurement, a model or you) and how sure it is, measured values, an
embedding for finding similar sounds, and the version of the analyser that
made them. The browser fills in only the names, the file's size and time
of change, and its header: format, sample rate, channels and length. It's
saved a couple of seconds after a change, so tagging a run of files saves
it once, and when the app closes. A library file that can't be read is
kept beside the new one as `library.json.broken`, and the status bar says
so.

### How fast it finds things

`cargo bench -p musicable-library --bench large` builds a made-up library
of a quarter of a million files, named as sample packs name them (pass
`-- 1000000` for a million), and prints how long each step takes. On a
four-core cloud machine:

| What | Time |
|---|---|
| Writing the index file (44.1 MB) | 179 ms |
| Reading the index file | 161 ms |
| Preparing the index for search | 157 ms |
| Preparing one folder of the library, of 62,500 files | 37 ms |
| Searching for "kick" (38,874 found) | 3.9 ms |
| Searching for "dark pad" (625 found) | 3.2 ms |
| Searching for "deep house kick 120" (146 found) | 5.7 ms |
| Searching for "ha" (64,375 found) | 4.2 ms |
| Searching for "nothing like it" (none found) | 3.4 ms |
| Finding files like one | 62 ms |
| Scanning 20,000 files the first time | 133 ms |
| Scanning them again, unchanged | 62 ms |

The scan is of 20,000 small WAV files the bench writes, which the system
has cached, so it measures Musicable's work, not the disk's. A search goes
through every file's name and folders, in lower case, one after the other
in a single string for each folder of the library, so there's no index of
words to keep up to date.

## Keys

Every key press goes to the keymap before egui sees it, through
`musicable_ui::KeyPresses`. A bound chord runs its command through
`Session::key_pressed` and goes no further, so Space plays even when a button
has keyboard focus. Unbound keys go on to egui, and so do keys whose command
doesn't apply now (`Session::takes_key`): with no notes selected in the
piano roll, the arrow keys move the keyboard focus between controls, as
egui does with them. Key presses never reach the engine any other way.

- While a text field has focus, such as the tempo field being typed into,
  every key goes to it instead. Ctrl+C, Ctrl+X and Ctrl+V reach the keymap
  like other chords, and copy, cut and paste only in text fields.
- Holding a key down runs its command once. The repeats the system sends are
  swallowed.
- While the app asks whether to save changes, keys go to that question.
- While the command palette is open, keys go to it, and run no commands.
- While the keyboard shortcuts window waits for the keys to give a command,
  every key press goes to it, Escape and Enter too, and nothing else sees
  it. `KeyPresses::capture` takes it there instead of `KeyPresses::take`.

### Keyboard layouts

Keymaps name keys after a US keyboard ([commands.md](commands.md#key-chords)).
A key press on another layout is named like this:

1. **The digit row by its place**: the key that types 1 on a US keyboard is
   `1`, even where the layout needs Shift for 1, as AZERTY does. So Ctrl+1
   is the same key on every layout.
2. **Other keys by what they type**, when keymaps have a name for it: Ctrl+Z
   is the key labelled Z, wherever the layout puts it (German, AZERTY), and a
   key that types `-` is `Minus`.
3. **Otherwise by its place on a US keyboard.** That covers letters on
   layouts without Latin letters (Ctrl+F on a Russian keyboard is the key
   where a US keyboard has F), characters a US keyboard has no key for (ö is
   `Semicolon`, ß is `Minus`), and keys pressed with Shift, since Shift
   changes what they type: Shift+1 is `Shift+1`, not `!`.

This is what most apps do with letters, and keeps shortcuts on the digit row
and punctuation where a keymap file puts them. The numeric keypad comes
through as the main keyboard's keys, so the keypad's 5 is `5`, except
NumpadAdd. The other keypad names in keymap files don't match any key press
yet. The Windows key isn't reported to the app, so `Cmd` chords only work on
macOS.

## Files

**File > Open Project**, **Save Project** and **Save Project As** use the
system's file dialogs, starting in the folder of the last project. Saving
adds `.musicable` to a name without it. Project files are read and written
by `musicable_io::project`, which writes through a temporary file, so a
failed save leaves the old file alone. Errors go to the status bar.

New, New Demo, Open and Quit lose the project's unsaved changes, so they ask
first: Save, Don't Save or Cancel. So does closing the window. Saving there
can still be cancelled in the file dialog, which cancels the rest.

The file commands are real commands (`file.new` and so on, see
[commands.md](commands.md#file)), so they can be remapped. A command can't
show a dialog, so it pushes a `Request` into its `Context`, and the app
carries it out after the command has run.

## Audio files

An audio clip plays a file by its path: relative to the folder the project
file is in, or absolute ([project-format.md](project-format.md#track)).
The app reads WAV, AIFF and FLAC files (`musicable_io::audio` lists the
sample formats), and resamples a file at another rate than the device's.

Each time the session builds a graph, on the compiler's thread, it finds
the files the project's audio clips play (`crates/app/src/media.rs`):

- A file up to 2²¹ frames long, about 44 seconds at 48 kHz, loads whole.
- A longer one streams: the disk thread reads it ahead of each clip that
  plays it, and from where the clip plays after the loop jumps back
  ([engine.md](engine.md#clips)).
- A graph rebuilt for an edit gets the files and streams the old one has,
  so nothing is read again and the clips that stream play on. Opening a
  project reads its files again, so it plays them as they are now.

A file that can't be read, because it's missing or isn't one the app
reads, leaves its clips silent, and the status bar says so, once for each
file until another project is opened. So does a file that fails while it
streams. A project that hasn't been saved has no folder, so relative paths
lead nowhere.

**Adding files.** Drop audio files on the arrangement, or pick them with
**File > Import Audio Files...**, and they become clips, one after another
(`clip.insert_audio`). A file in the project's folder, or a folder inside
it, is named relative to it, as `Samples/kick.wav`; any other is named by
its absolute path. The app reads a file's header before it adds the clips,
so they're as long as the file, and a file it can't read is left out, with
the reason in the status bar.

**Headers and waveforms.** A thread of the app's own
(`crates/app/src/audio_files.rs`) reads the header and the waveform of
every file the project's clips play: the headers first, as edits need them
(an audio clip can't be trimmed past its file's end, and positions in a
file count in its own sample rate), then the waveforms, a file at a time,
so a long one never holds up the window. It looks again whenever the
project changes, and starts over when another project opens. A waveform is
the lowest and highest sample of every 64 frames, and of every 4 of those,
and so on, about 5 MB for ten minutes of audio at 48 kHz; clips draw from
it at any zoom.

Save Project As doesn't copy the files or change the clips' paths, so a
project saved in another folder, away from its files, plays its clips
silent when it's opened again.

To hear clips now, the `clips_demo` example writes a project and the files
it plays, here into `target`, which Git ignores:

```sh
cargo run --release -p musicable-app --example clips_demo -- target/clips-demo
cargo run --release -p musicable-app -- "target/clips-demo/Clips demo.musicable"
```

It's sixteen bars at 100 BPM that loop: drums from a WAV file, a bass line
from an AIFF file at 44.1 kHz, a pad from a file long enough to stream,
fading in and out, and chords on the test instrument that swell with
pressure and bend at the end of each bar. The null test plays it too:
`musicable null-test "target/clips-demo/Clips demo.musicable"`.

## Exporting audio

**File > Export Audio...** (Ctrl+Shift+R, as in Live) opens the export
window, which renders the song to audio files faster than real time, on a
thread of its own, while the app plays on:

| Setting | What it does |
|---|---|
| Render | The song, from its start to the end of its last clip; the loop region; or the selection: the time selection, or else from the first selected clip to the end of the last. It starts on the selection when there is one. |
| Files | The mix, as you hear it, mute and solo too; and each track ticked on its own (stems): the track after its fader, pan and slots, as it feeds its bus, before mute and solo. A return track's file is its return; a group's, what its unmuted tracks feed it. Sends aren't in a track's file, and the master's devices aren't in any but the mix's. |
| Format | WAV, AIFF or FLAC, 16-bit or 24-bit, or 32-bit float in WAV and AIFF, which keeps what goes over full scale. FLAC files carry the samples' MD5, as libFLAC's do. |
| Sample rate | The engine's, or another: the song then renders at that rate, its plugins too, and audio files at other rates are converted as they play. |
| Dither | Adds triangular noise of one step as integer samples are rounded, so quiet sounds fade out smoothly. Exact silence stays silent, and every export of the same song is the same, bit for bit. |
| Normalise | Raises or lowers each file so its loudest sample is at 0 dBFS. |
| Render the tail | Renders on after the end until everything has been quiet, below -96 dBFS, for two seconds, for 30 seconds at most, and leaves the quiet end off, so reverbs and releases ring out. |

**Export...** asks where to save the mix, and the stems go next to it,
named after it and their track's number: `Song.wav`, `Song 01 Drums.wav`,
`Song 02 Bass.wav`. The window then shows how far along the export is and
how many times faster than real time it renders, and Cancel stops it.
Closing the window leaves it running, and the status bar says when it's
done. The window then lists each file with its peak, and warns about
samples over full scale, which clip in integer files. The window's
settings are kept in `settings.toml` for the next export.

An export renders exactly what the app plays. It plays a copy of the
project as it would be saved, with each plugin's state saved afresh, in a
session of its own (`Session::exporting`), as the null test's player does:
its own engine, whose parallel scheduler's workers don't take real-time
priority, and its own instances of the project's plugins, loaded on the
main thread, which serves what they ask of it while the export runs. The
copy plays without the metronome, looping or punching in, from the start of
the range, and the transport stops at its end, to the frame. The render
thread asks the engine for 4,096 frames at a time and skips the frames the
graph's latency delays the output by, so a file starts with the range,
lined up to the sample however late the plugins are. Stems are taps on the
graph ([engine.md](engine.md#delay-compensation)), lined up with the mix by
delay compensation, and render with solo cleared, so a group's stem has all
its unmuted tracks; when solo is on and the mix is wanted too, the mix
renders as it's heard, in a pass of its own.

Files are written by `musicable_io::audio::Writer`, which streams WAV, AIFF
and FLAC files with our own FLAC encoder, and named `NAME.part` until
they're whole, so a cancelled or failed export leaves no half-written files.
Normalising renders to a float file first, `NAME.float.part`, then writes
the file from it.

`musicable export` does the same from the command line, with the plugins
the app's last plugin scan found:

```sh
cargo run --release -p musicable-app -- export song.musicable out/Song.flac --stems
cargo run --release -p musicable-app -- export song.musicable Song.wav --bits 16 --rate 44100 --normalize
```

`--help` lists its options: the loop instead of the song, the format (the
file's extension by default), the bits, the rate, the block size, stems
with or without the mix, normalising, and no dither or no tail. On a 4-core
cloud VM, the clips demo below, 38.4 seconds and four tracks, exports at 54
times real time to a WAV file, 39 times to FLAC, and 22 times with the mix
and the four stems in FLAC. The engine sets the pace: the null test renders
it at 46 times real time.

[`crates/app/tests/export.rs`](../crates/app/tests/export.rs) is the done
test: an exported mixdown is what the app plays live, sample for sample,
with a latent plugin, an audio file at 44.1 kHz, a test instrument and a
muted track, the live render asked for in 441-frame buffers. Each stem is
its track soloed, the tail is the release the live render plays, and
normalised, 16-bit, AIFF, FLAC and 96 kHz files hold the same samples,
scaled or rounded. A cancelled or failed export leaves no files behind.

## Settings and your keymap

The app keeps its settings, your keymap, your themes and the panels'
layout in its config folder:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\Musicable` |
| macOS | `~/Library/Application Support/Musicable` |
| Linux | `$XDG_CONFIG_HOME/musicable`, or `~/.config/musicable` |

The `MUSICABLE_CONFIG_DIR` environment variable picks another folder, which
is handy for testing.

`plugins.json` is what the plugin scans found, so the next start has the
plugins at once and scans only what changed. Deleting it makes the next
scan load every library again.

`library.json` is the browser's library: the folders of samples you
added, what's in them, and your tags ([The browser](#the-browser)).
Deleting it empties the library, tags and all.

`keymap.toml` is your keymap file, as [commands.md](commands.md#the-keymap-file)
describes. The app reads it at startup and when you click Reload in
Options > Keyboard Shortcuts, which shows any mistakes in it. A file that
can't be used leaves the default keymap, and says why. The app saves every
change of keys made in the window to the file at once. Create It writes a
file with no changes in it yet, just the comment that explains it.

Before writing over your keymap file, the app copies it to
`keymap.backup.toml` in the same folder when the write would lose something:

- When the file has mistakes, or can't be read at all, the first change made
  in the window writes a new file, and the old one becomes the backup. The
  window says so.
- Reset All and Import replace all your changes at once, so the file with
  your keys from before becomes the backup. Importing the backup brings them
  back.

Single changes to a file without mistakes make no backup: there's nothing to
lose. Without a config folder, changes last until the app closes. If the
backup or the file can't be written, the window says so, and the keys you
set still work until the app closes.

`workspaces/Default.toml` is how the window's panels are laid out
([The panels](#the-panels)), written whenever you change the layout, once
you let go of the tab or the edge you're dragging, and read at startup:

```toml
format = "musicable-workspace"
version = 1

[[windows]]
hidden = ["browser", "piano_roll"]
layout = { across = [
    { panel = "browser" },
    { down = [
        { panel = "arrangement" },
        { panel = "mixer", size = 372 },
        { tabs = ["devices", "piano_roll"], front = "devices" },
    ] },
] }
```

A part's `size` is its width in a row of parts (`across`) or its height in
a column (`down`), in points; parts without one take their panels' default
sizes, and the part with the arrangement takes what's left. `hidden` lists
the panels hidden, which keep their place. Panels the file doesn't have go
where they go at first, hidden, and a file that can't be used leaves the
first layout, and says why. Deleting the file brings back the first
layout.

`settings.toml` holds the audio settings, written whenever you apply new
ones, how editing behaves, the theme, the export window's and the
browser's settings as they were last used, how long Capture holds the
inputs, and the devices' recording offsets:

```toml
format = "musicable-settings"
version = 1

[audio]
device = "{0.0.0.00000000}.{a1b2c3d4-...}"
exclusive = false
buffer_size = 128

[behaviour]
play_from = "start_marker"
middle_c = "c3"

[appearance]
theme = "Studio.toml"

[export]
format = "wav"
bits = 24
dither = true
normalize = false
tail = true
mixdown = true
stems = false

[browser]
auto_preview = true
preview_volume = -6

[recording]
capture_minutes = 5

[[recording.offsets]]
device = "{0.0.0.00000000}.{a1b2c3d4-...}"
exclusive = true
sample_rate = 44100
frames = 878
```

Every setting is optional: no device is the default output, no
`sample_rate` is the rate the device runs at, and no `buffer_size` is the
smallest the device supports, or an ASIO driver's preferred size. `play_from`
is where Space plays from: the start marker (`start_marker`), as in Live, or
wherever the playhead stopped (`playhead`). `middle_c` is what the piano
roll calls MIDI note 60: `c3`, as in Live, Cubase, Logic and Bitwig, `c4`,
as in scientific pitch notation and Reaper, or `c5`, as in FL Studio.
`capture_minutes`, under `[recording]`, is how long [Capture](#capture)
holds the inputs, 5 minutes if it's left out; 0 turns Capture off. The app
reads them at startup; there's no window for them yet. `theme`, under
`[appearance]`, is the theme Musicable draws with: `dark`, `light`, or the
name of a file in the `themes` folder ([Themes](#themes)), which Options >
Appearance writes. Each of
`recording.offsets` is the [recording offset](#recording), in `frames`, of
a `device`, by its id, in exclusive mode or not (`exclusive`, false if it's
left out), at a `sample_rate`. Audio Settings writes them; an offset of 0
isn't kept. `[export]` is the export window's settings as it was last used:
`format` is `wav`, `aiff` or `flac`, `bits` is 16, 24, or 32 for floats,
and a `sample_rate` there renders at that rate instead of the engine's.
`[browser]` has Auto, which plays a sample when it's picked, and the
previews' volume in dB, from -60 to 6; a `presets_folder` there saves
presets somewhere other than `Music/Musicable/Presets` in your home folder.
Fields this version doesn't know are ignored.
An ASIO driver's id starts with `asio:`; a build without ASIO plays through
the default output instead, and says why.

## Themes

Musicable draws with a theme: the dark one, a light one, or a theme file of
your own. **Options > Appearance** picks one, and the window changes at
once. A theme colours Musicable's views and egui's own widgets too, the
menus, buttons, fields and windows, and the window's title bar is dark or
light as the theme is, whichever the system is in.

Theme files live in the `themes` folder in the [config
folder](#settings-and-your-keymap), and hold only what they change in a
built-in theme, the one their `base` names:

```toml
base = "light"
playhead = "#ff8800"

[mixer]
strip = 96

[widgets.resting]
background = "#d0d4e0"

[fonts]
text = "Inter.ttf"
```

Each entry has the name of one of the theme's colours or sizes, in the
section that holds it. Colours are `"#rrggbb"`, or `"#rrggbbaa"` with an
opacity, sizes are in points, and pairs of sizes, across and down, are
`[8, 3]`. A file without a `base` changes the dark theme. **Export Theme...**
in the Appearance window writes the theme in use as a file with every entry
in it, with those that keep the base's value commented out and each group
explained, to start a theme of your own from; written into the themes
folder, it becomes the theme in use. Musicable looks at the theme file in
use twice a second, and draws with what you change in it as soon as you
save it. Reload reads it again, and its fonts.

Most entries colour or size Musicable's views: the arrangement's, the
transport bar's, and those under `[mixer]`, `[devices]`, `[piano_roll]` and
`[browser]`. `[dock]` sizes the panels' tab bars (`tab_bar`), the gaps
between panels (`gap`) and the least a panel's edge can be dragged to
(`min_panel`). `[widgets]` is egui's: the fill of panels and of windows and
menus, their outline and shadow, text fields (`field`), the selection,
links, warnings, corners and spacing. Under it, `[widgets.passive]` is what
can't be clicked, such as labels and separators, and `resting`, `hovered`,
`pressed` and `open` are buttons, checkboxes and sliders at rest, with the
mouse over them, while they're pressed, and with their menu open, each with
a `background`, a `fill` for checkboxes and the like, an `outline`, `text`
and a `corner`. `[fonts]` names the font of text and of monospaced text: a
font file in the themes folder, or one of egui's, `Ubuntu-Light` (egui's
text) and `Hack` (egui's monospace); text a font can't write falls back to
egui's fonts. `[fonts.sizes]` holds egui's text sizes; Musicable's views
have sizes of their own, such as `text`.

A mistake doesn't stop the rest of the file from applying: an entry no
theme has, or one whose value isn't what it should be, keeps the base's
value, and a font file that can't be read keeps egui's font. A file that
can't be used at all, such as one that isn't valid TOML, leaves its base in
its place, or the dark theme when even that can't be told, and so does a
file that's gone. The status bar says so until the file is mended, and the
Appearance window says what was wrong.

## Audio

`musicable_app::Output` keeps the audio device playing the engine:

- At startup it opens the stream the settings ask for. If the device in the
  settings isn't plugged in, it plays through the default output instead, and
  says so; the settings keep the device for next time.
- The engine is built for the stream, with its sample rate and its period as
  the block size, and always renders with the parallel scheduler
  (`musicable_app::engine`).
- When the stream ends by itself, because the device was unplugged or its
  format changed, the output tries it again at once and then every two
  seconds. An ASIO driver that asks to be reset, after a change in its own
  settings, opens again at once with them, and the app says what it plays
  with now. While nothing plays, the engine waits, and the commands sent to it
  wait in its queue and the session's.
- **Apply** in the audio settings stops the stream and opens the new one. If
  it needs another sample rate or block size, a new engine takes over:
  `Session::replace_engine` sets it up with the project's graph, tempo map
  and loop, at the same place in bars and beats, playing if the old one was.
  If the new stream doesn't open, the old one comes back, and the error shows
  in the window.

The audio settings window lists the devices (Refresh lists them again after
plugging one in), offers exclusive mode where the device does, the device's
rates in exclusive mode, and buffer sizes from the smallest the device
supports. Drivers round the buffer size, and some ignore it, so "Now playing"
says what the stream got. Below that, **Recording offset** is the device's
[recording offset](#recording) in its mode and at its rate, in frames and
milliseconds: drag it, or type it and press Enter, and the next recording
uses it, as does Capture. The foot of the window has the notice the GPL asks
for.

On Windows, the window has a driver type above the device, WASAPI or ASIO
([audio.md](audio.md#asio)). For ASIO it lists the installed drivers, the
rates each can switch to and the buffer sizes it takes, by default the one
it prefers, and Driver Settings opens the playing driver's own settings.

### Meters

- **CPU**: the share of the time the audio thread spent in its wake-ups,
  rendering included, over the last quarter of a second. Near 100%, wake-ups
  start running late and dropouts follow. It comes from
  `StreamStats::busy`. The tooltip adds the longest wake-up, and how many
  threads the parallel scheduler kept busy at once while it shared blocks.
- **Dropouts**: `StreamStats::underruns` since the stream started. Click to
  count from zero again. The tooltip has the audio lost, how many of the
  dropouts were in the system's audio engine or driver, and the late
  wake-ups.

## How it's built

- `crates/ui` draws the shell with egui. It depends on the model, the
  commands and the library's types only. The app hands it a `View` of what
  to show each frame, and carries out the `Action`s it hands back. Its
  tests drive the shell with `egui_kittest`.
- `crates/library` is the browser's library: the index and its file,
  scanning folders, searching, presets, and the queue for file work, with
  no app or UI in it. `crates/app/src/library.rs` puts it to work for the
  app: it runs the library's file work on the queue, hands the engine the
  previews, and reaches the commands through `LibraryLink`.
- `crates/app` has the eframe app (`App`), which holds the `Session`, the
  `Output`, the audio settings window and the user's keymap file
  (`UserKeymap`), and the binary. Debug builds install the engine's
  allocation checker, as the tests do.
- [`crates/app/src/app/remapping.rs`](../crates/app/src/app/remapping.rs)
  drives the whole window with `egui_kittest`, handing it key presses as
  eframe does, without an audio device. It gives every command new keys in
  the keyboard shortcuts window, finds every command in the command palette
  with them, and starts the app again to check they were saved.
- [`crates/app/src/app/arranging.rs`](../crates/app/src/app/arranging.rs)
  drives the window the same way through the Tier 1 demo's arranging
  steps, with the mouse and the keys: it drops an audio file on a track,
  inserts a MIDI clip, moves, duplicates, splits, deletes, trims and fades
  clips, draws and toggles the loop, adds a locator from the ruler's menu,
  zooms, undoes every step and redoes it, then saves and opens the project
  again.
  [`crates/app/src/app/plugins_on_tracks.rs`](../crates/app/src/app/plugins_on_tracks.rs)
  puts the test plugins on a track from the browser, opens and
  closes a plugin's window from the devices panel, turns a knob in it,
  undoes, deletes a device, saves, and opens the project again. It also
  puts a VST3 plugin on a track and opens its window.
  [`crates/app/src/app/browsing.rs`](../crates/app/src/app/browsing.rs)
  adds a folder of samples to the library, searches it with Ctrl+F,
  previews a sample through the engine at the settings' volume, puts it on
  a new track with a double-click, tags it, and starts the app again to
  find the tag and the settings kept.
  `crates/app/src/app/testing.rs` has what they share.
- `crates/ui/src/arrangement/tests.rs` drives the arrangement's gestures,
  context menus, wheel and dropped files on a real document, and checks that
  only what's in sight of a hundred tracks is laid out.
  `crates/ui/src/piano_roll/tests.rs` does the same for the piano roll's
  gestures, Draw Mode, keyboard, wheel and context menus.
- [`crates/app/tests/piano_roll.rs`](../crates/app/tests/piano_roll.rs)
  edits notes with the piano roll's commands and keys, and checks that the
  test instrument plays them as they are after each change and undo.
- [`crates/app/tests/app_shell.rs`](../crates/app/tests/app_shell.rs) checks
  what the shell asks of the session: file requests, opening projects, the
  tempo field and undoing a drag of it, and a new engine taking over. The
  demo project there must equal the fixture.
  [`crates/app/tests/mixer.rs`](../crates/app/tests/mixer.rs) checks that the
  mixer's changes reach the engine and the meters move, and that a mix with
  groups, returns and sends plays the same with both renderers.
- Graphs compile on their own thread
  ([commands.md](commands.md#running-commands-in-the-app)). While one
  compiles, the window looks for it every 5 ms, to send it to the engine
  soon after it's ready.
  [`crates/app/tests/slow_graphs.rs`](../crates/app/tests/slow_graphs.rs)
  checks that a node that's slow to prepare doesn't hold up the
  session, and that what you do meanwhile reaches the engine with the
  graph.
- `musicable_app::null_test` plays projects offline for the null test and
  for [`crates/app/tests/tier0_demo.rs`](../crates/app/tests/tier0_demo.rs)
  and [`tier1_demo.rs`](../crates/app/tests/tier1_demo.rs), which load the
  test plugins into its players (`Player::with_plugins`).
- `musicable_app::export` renders exports, and
  [`crates/app/tests/export.rs`](../crates/app/tests/export.rs) checks them
  against what the app plays live (see [Exporting audio](#exporting-audio)).
  [`crates/app/src/app/exporting.rs`](../crates/app/src/app/exporting.rs)
  opens the export window with Ctrl+Shift+R and exports from it in the
  background.
- [`crates/app/tests/clips.rs`](../crates/app/tests/clips.rs) saves a
  project whose audio clips play WAV, AIFF and FLAC files from its folder,
  one of them streaming, and whose MIDI clip plays the test instrument. It
  checks that every renderer plays it the same, and that a missing file is
  reported.
- [`crates/app/tests/plugins.rs`](../crates/app/tests/plugins.rs) plays the
  test plugins on tracks through the session, as CLAP and as VST3: saved
  and opened again, undoing changes made in their windows, missing, and
  lined up with a dry track by delay compensation. It also runs the app's
  binary as the plugin scanner, on a CLAP library and a VST3 bundle.

To see the window without Windows, a Linux build runs under Xvfb, and the
Windows build runs under Wine with `WGPU_BACKEND=vulkan` (Wine's Direct3D 12
can't compile wgpu's shaders). Wine's WASAPI plays into PulseAudio.

## Not done yet

- Automation: selecting points to move or delete several at once, curves
  between points, automation moving and copying with its clips, and a
  "Back to Arrangement" button in the transport bar, as Live has (for now
  **Automation > Re-Enable Automation** re-enables every overridden lane).
  Lanes of switches and choices drawn and edited as steps: for now their
  points take whole values, but the value passes through the ones between
  two points that differ, so switching at a place takes a step there.
- Reordering tracks, grouping the selected ones (Live's Ctrl+G), and
  devices in the mixer.
- Moving devices along a chain, and a device's parameters in the devices
  panel.
- In the browser: analysis, which will tag sounds by how they sound and
  find similar ones by an embedding; previews in time with the song;
  dragging a sample onto a sampler, as onto Live's Simpler (Load Sample...
  gives it one for now); watching the library's folders, which are looked
  through at each start and on Rescan; CLAP presets, which need the
  plugins' preset discovery; renaming and deleting presets, and saving
  VST3 presets; and tags in the undo history. Taking a folder out of the
  library forgets its files' tags.
- VST3 plugins' windows on macOS and Linux.
- Nudging clips with the arrow keys, consolidating them, and crossfades
  drawn between clips; a window for the behaviour settings.
- Editing a theme in the Appearance window, with colour pickers: theme
  files are edited in a text editor for now. Plugins' own windows keep
  their own look.
- In the piano roll: the notes' MPE expression, shown and edited (Tier 2),
  drawing velocities across the lane, folding to the keys in use, scales,
  and repeating the arrow keys while they're held.
- Keymaps in the style of other DAWs (Logic, Cubase, Pro Tools). Each would
  be a keymap file to import.
- A macOS default keymap with Cmd.
- Following the default output when it changes.
- An icon, and opening `.musicable` files from the file manager.
- Exporting to MP3 or other lossy formats, stems with the returns' and the
  master's effects (Live's "Include Return and Master Effects"), and
  remembering which tracks were ticked for stems.
