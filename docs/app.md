# The app

`musicable` opens a window with the arrangement, the piano roll, the Sample
Editor, the mixer, the devices panel and the browser. It plays a project through the audio
device with the parallel scheduler, records audio and MIDI into takes,
plays CLAP and VST3 plugins and eight devices of its own, runs the commands
from keys, menus, the command palette and the mouse, and opens, saves and
exports projects. Every command's keys can be changed in the app. Undo and
redo cover every change to the project: the clips and their notes, the
takes, the locators, the loop, the tempo, the tracks, the devices, the
automation and the mix.

[The manual](../manual/src/introduction.md) says how to use it. Each section
here links the manual's pages for what it covers, and has what they leave
out: how the app is built, the commands and types behind it, and how it's
tested.

## Running it

```sh
cargo run --release -p musicable-app                      # an empty project
cargo run --release -p musicable-app -- path/to/song.musicable
```

On Windows, release builds open no console window, so double-clicking
`musicable.exe` starts the app without a command line. Debug builds keep the
console, and are much slower: the engine checks every allocation, and egui
runs unoptimised.

On Windows, the app draws with Direct3D 12. With Vulkan, which wgpu would
pick, an NVIDIA card lost the graphics device as the displays woke up, and
egui-wgpu can't carry on without it, so the app crashed. The `WGPU_BACKEND`
environment variable picks another of wgpu's backends, as
`$env:WGPU_BACKEND = "vulkan"` does in PowerShell.

What goes wrong while the app runs goes to the log file, `musicable.log` in
the config folder ([Settings and your keymap](#settings-and-your-keymap)):
warnings and errors from the crates the app is built on, such as wgpu, the
graphics adapter the app draws with, the graphics device being lost, and
panics, with the calls that led to them, each line with the time in UTC.
The console the app was started from, if any, shows them too.

[The command line](../manual/src/reference/command-line.md) in the manual
lists what else `musicable` runs: `musicable keymap [FILE]` prints the keys
([commands.md](commands.md#trying-it-out)), `musicable null-test [PROJECT]`
checks the parallel scheduler against the reference renderer ([The null
test](#the-null-test)), and `musicable export PROJECT FILE` renders a
project to audio files ([Exporting audio](#exporting-audio)). `musicable
mcp` lets the agent whose client starts it, such as Claude Code, control the
Musicable that's running, and `musicable api` prints the commands agents can
run, as JSON ([agents.md](agents.md)). `musicable scan-plugin LIBRARY` is
the plugin scan's own process for each library
([plugins.md](plugins.md#scanning)).

## The Tier 0 demo

1. Start the app, and choose **File > New Demo Project**. Or open
   [`crates/io/tests/fixtures/v1/demo.musicable`](../crates/io/tests/fixtures/v1/demo.musicable),
   which is the same project saved: a dozen test tones through their gain
   stages, one of them muted, looping the first four bars at 124 BPM with the
   metronome on.
2. Press Space. It plays, loops and clicks.
3. **Options > Audio Settings** opens the [Settings
   window](#the-settings-window) at its Audio page, which picks the device,
   shared or exclusive mode, and the buffer size. The right end of the
   transport bar shows the CPU load and counts dropouts.
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

[A first song](../manual/src/getting-started/first-song.md) in the manual is
the same song with Musicable's own synth, for people trying Musicable.

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
same however fast it renders, clips in Spectral wait for their
segments to render, as an export's do ([The render
cache](#the-render-cache)), and clips in Granular that follow the pitch
find their files' pitch marks ([Audio files](#audio-files)). Files that can't be read are listed before the
results, and their clips are silent in every run.

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

[The window](../manual/src/window/index.md) in the manual has the menu bar,
the transport bar, the panels, windows of their own and the status bar, and
[Workspaces](../manual/src/customising/workspaces.md) has the layouts and
the views' parts. Behind them:

- **The menus** list the registry's commands (`crates/ui/src/menus.rs`). A
  command in an area with no menu yet gets a menu named after its area,
  before Browser, so every command can be reached; commands that need
  arguments are left out, as menus can't give them.
- **The tempo field** runs Set Tempo for each change, and a whole drag is
  one step in the undo history.
- **The status bar** has the audio device's latency as its clock measures
  it ([Audio](#audio)), and an agent's last change
  ([agents.md](agents.md)).
- **Double-clicks** count within the double-click time in Windows' mouse
  settings, which the app reads as it starts
  (`follow_system_double_clicks`), and within egui's 0.3 seconds elsewhere.

### The panels

The arrangement, the mixer, the devices panel, the piano roll, the Sample
Editor, the browser, the History panel, the Routing panel and the
Performance panel are docked panels, laid out in splits and tabs
([egui_tiles](https://docs.rs/egui_tiles), `crates/ui/src/dock.rs`). The
shell's panel registry (`crates/ui/src/panels.rs`) holds each with its id,
such as `musicable_ui::MIXER` and `PIANO_ROLL`, its title, its place and
size in the default layout, and how it's drawn: the dock lays the panels
out by id, and the workspace files name them by it.

The panel with the focus, outlined in the theme's `focus` colour, is the
editor's ([commands.md](commands.md#the-editors-state)), so the edit
commands act on its selection, and its own keys work while it has it.

### Windows of their own

`view.move_to_new_window` moves the panel with the focus to a window of its
own, as its tab's menu does, and `view.move_to_main_window` sends it back
([The window](../manual/src/window/index.md#windows-of-their-own)). Each
window is an egui viewport the shell shows immediately, with its own tree of
panels and its own place on the desktop, which the workspace keeps
(`crates/ui/src/windows.rs`), so it draws in the same frame as the main
window ([How fast it draws](#how-fast-it-draws)). A panel out of the main
window keeps its place there, hidden, for Move to Main Window. A tab dragged
out past the window's edge has its ghost follow the pointer in a little
window of its own (`crates/ui/src/ghost.rs`).

### Workspaces

[Workspaces](../manual/src/customising/workspaces.md) in the manual has the
Workspaces menu and the workspace files. The menu's commands are
`workspace.*` ([commands.md](commands.md#workspace)): `workspace.switch_1`
to `workspace.switch_9` switch to the workspace in each slot, Alt+1 being
Default. `musicable_ui::Workspace` is a workspace's file, with the windows'
layouts and the views' parts (`crates/ui/src/workspace.rs`), and the app
keeps the files in the profile in use's `workspaces` folder
(`crates/app/src/workspaces.rs`, [Settings and your
keymap](#settings-and-your-keymap)). A project remembers the workspace it
was saved with ([project-format.md](project-format.md#the-file)).

### Configurable views

[Hiding and moving
parts](../manual/src/customising/workspaces.md#hiding-and-moving-parts) in
the manual lists the transport bar's, the track headers' and the mixer
strips' parts, with the ids the workspace files call them by.
`crates/ui/src/parts.rs` keeps them ([How it's built](#how-its-built)), and
the app keeps what's changed about the views in the workspace in use, with
the panels' layout, so each workspace can have its own
([Workspaces](#workspaces)).

## The arrangement

[The arrangement](../manual/src/window/arrangement.md) in the manual says
what it shows, and what the mouse and the right-click menus do in it.

Everything it does runs a command ([commands.md](commands.md#clip)), so it
can all be undone and given other keys, and a drag runs one command when it
ends, so it's one step in the undo history and the engine rebuilds its
graph once. Comping runs the take commands
([commands.md](commands.md#take)), and drawing in a lane the automation
commands ([commands.md](commands.md#automation)). Its right-click menus
list commands from the registry, which take their arguments from where the
menu was opened: the clip, the track, the parameter, or the position. The
playhead is drawn where the sound is: the engine's position less the
latency of the plugins and the audio device
([engine.md](engine.md#delay-compensation)).

**Themes and mouse maps.** The arrangement, the piano roll, the Sample
Editor and the mixer draw every colour and size from a `musicable_ui::Theme`, and look up what
each gesture does, from where it starts and the modifiers held, in a
`MouseMap`. `MouseMap::live()` is what they do at first, as the manual's
tables give it. `Shell::set_theme` and `set_mouse_map` change them: the app
sets the theme from the Settings window's Appearance page and theme files
([Themes](#themes)), and the mouse map from the Mouse page and the user's
mouse map file, which holds the changes to `MouseMap::live()`
(`musicable_ui::MouseMapFile`). What each place can do with each gesture,
which the page offers, is `Target::actions`: a new action goes in that
table, as well as in the views that carry it out.

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

Windows of their own ([above](#windows-of-their-own)) draw in the same
frame as the main window, so every window draws whenever one does. To see
how long frames take in the app itself, start it with the
`MUSICABLE_FRAME_TIMES` environment variable set, as in
`$env:MUSICABLE_FRAME_TIMES = 1` in PowerShell. The window then draws all
the time, as fast as the monitor takes frames, and every five seconds the
console it was started from shows how far apart frames were, and how long
they took on the CPU with every window drawn, as eframe measures it. A
debug build drawing in software (Mesa's lavapipe) under Xvfb, with three
windows open, printed:

```text
225 frames, 3 windows: 22.29 ms apart, at most 56.03; 17.54 ms on the CPU, at most 47.69
```

and about 7 ms on the CPU with the main window alone. Software drawing is
far slower than a GPU's, so a release build on a real monitor is the one to
go by: at 144 Hz, a frame has 6.9 ms.

## The piano roll

[The piano roll](../manual/src/window/piano-roll.md) in the manual says what
it shows, and what the mouse, the right-click menus and the keys do in it.
Everything it does runs a command ([commands.md](commands.md#notes) and
[controllers](commands.md#controllers)), and a drag runs one when it ends,
so it's one step to undo. Its keys are the piano roll panel's own in the
default keymap, so the arrow keys move notes only while it has the focus
([The panels](#the-panels)), and hiding it gives the arrangement the focus.

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

## The Sample Editor

[The Sample Editor](../manual/src/window/sample-editor.md) in the manual says
what it shows, and what the mouse and the keys do in it, and
[Warping](../manual/src/using/warping.md) what warping does. Everything it
does runs a command ([commands.md](commands.md#warping),
[warp modes](commands.md#warp-modes) for its controls and
[automation](commands.md#automation) for its lane), and a drag runs one
when it ends, so it's one step to undo: dragging a transient's pseudo
marker runs Insert Warp Marker and Move Warp Markers as one batch
(`Action::RunBatch`). While a drag lasts, it draws the waveform as the drag
would leave it, from a copy of the clip's warp that the drag changes
(`crates/ui/src/sample_editor.rs`), and the rulers, the markers, the
transients and the waveform are drawn in `sample_editor/paint.rs`; its
context menu is `sample_editor/menu.rs`. It finds the transients in sight
by their frames, which rise across with the warp's markers, so it only
looks at those near the pointer or on screen. A warped clip's transients
come from analysis: the app gives each warped clip that has none yet those
of its file as analyses arrive and the project changes
(`warp::take_transients`), as a change the project follows rather than a
step to undo. As analyses arrive, the app also warps the files dropped
before they were analysed (`Session::auto_warp`,
[commands.md](commands.md#auto-warp)), once none of a drop's files is
waiting (`Analyses::waits_for`). Its context menu has the warp commands
that act at a point, which it gives the frame of the marker, the transient
or the audio where it was opened ([commands.md](commands.md#warping)), and
Quantize. Its waveform comes from the same peaks as the arrangement's,
read along the clip's warp, so the two agree. Its keys are the Sample
Editor panel's own in the default keymap, as the piano roll's are, and
hiding it gives the arrangement the focus. Clip Editor and Switch Devices
and Clip Editor show the piano roll or the Sample Editor, whichever fits
the clip selected or opened last (`Editor::clip_editor`).

Its column of controls (`controls`) shows the mode's own settings under
the mode, as Live's does, and scrolls where the panel is too short for
them all. Its lane shows the curve of the parameter `Scene::lane` gives:
the one the editor's `SampleEditor::lane` names, or Gain while the clip is
unwarped. A click on the lane's name opens the menu of them
(`menu::lanes`), those the clip's mode plays first, and the lane of one
the mode doesn't play is greyed. A parameter in steps, such as Preserve,
is drawn as holds, its hold lines are what a drag moves, and its values
are rounded to steps as they're drawn. Under the lane's name is the value
under the pointer, or the one a drag moves a point to or draws
(`Scene::readout`).

**For Tier 2's extensions and profiles.** What the Sample Editor shows, its
selected markers and time, and its zoom are the `Editor`'s
(`editor::SampleEditor`); its colours and sizes are in the `Theme`
(`theme.sample_editor`, and the `warp_marker`, `warp_marker_selected`,
`transient`, `pseudo_marker`, `time_selection` and `outside_clip`
colours); its gestures are the `MouseMap`'s targets `WarpMarker`,
`Transient`, `WaveformTop` and `Waveform`, and its lane's are the
automation lanes' targets; and its panel's id is
`musicable_ui::SAMPLE_EDITOR`.

## The History panel

[The History panel](../manual/src/window/history.md) in the manual says what
it lists. Clicking a row runs `history.go_to`
([commands.md](commands.md#history)), which undoes or redoes the steps in
between, and the list has a row for each of the 1000 steps the history keeps
at most ([commands.md](commands.md#undo-and-redo)). Up and Down are the
panel's own keys for Undo and Redo ([Changing keys](#changing-keys)), so the
arrow keys still move the notes in the piano roll. Its panel's id is
`musicable_ui::HISTORY`, and it docks, moves to a window of its own and goes
in workspaces as the other panels do.

## The command palette

[The command palette](../manual/src/window/command-palette.md) in the manual
says how it finds commands. `crates/ui/src/search.rs` scores what's typed
against each command's area and title, its id and its keys, for the palette
and the Keys page's search alike, and the palette lists the five commands
run from it last first, among those that match as well.

## The Settings window

[Settings](../manual/src/customising/settings.md) in the manual has its
pages. `crates/app/src/settings_window.rs` lists them in order, each with
the id the app opens it at, such as `audio` for **Options > Audio
Settings...**, draws the small ones, and hands what changed to the app to
carry out and keep ([How it's built](#how-its-built)). Changes apply at
once, and are kept: in [settings.toml](#settings-and-your-keymap), in the
profile in use's keymap, mouse map, workspaces and settings
([Profiles](#profiles)), or in the theme's own file. Every setting in
settings.toml has a control here but the export window's, which that window
keeps as it was last used. The window isn't a panel, so no workspace has it.
The Behaviour page has auto-warp's switches, which go with the profile, and
Fast analysis, which doesn't: it's `Settings::analysis`, the computer's,
and the app hands it to the analysis jobs (`Analyses::set_fast`), so the
next file analysed uses it. The Files page has the render cache's limits,
`Settings::render_cache`, the computer's too, which the app hands to the
cache ([The render cache](#the-render-cache)).

### Profiles

[Profiles](../manual/src/customising/profiles.md) in the manual says what a
profile keeps, and where. `crates/app/src/profiles.rs` lists the user's
profiles and keeps their folders, and the app reads the profile in use's
files at once when it switches, with no restart. A profile's `settings.toml`
holds `[behaviour]`, `[appearance]` and `[workspaces]` alone
(`ProfileSettings`), and only what differs from Musicable's own
(`ProfileSettings::to_toml`), so a later version's new defaults still
arrive. Default's are the config folder's own files ([Settings and your
keymap](#settings-and-your-keymap)).

## Changing keys

[Keys](../manual/src/customising/keys.md) in the manual has the Keys page
and the keymap file. The shell draws the page, as it holds the keymap's
commands, and [commands.md](commands.md#changing-keys-in-the-app) has the
keymap's calls it's built on.

## The mixer

[The mixer](../manual/src/window/mixer.md) in the manual has a strip's
parts, and what the mouse does with them. Only the strips in sight are drawn
([How fast it draws](#how-fast-it-draws)). The mixer's controls hand back
`Action::Mixer` with a `mixer::Change`, which the app runs as its command
with `Session::change_mixer`
([commands.md](commands.md#the-mixers-controls)), and their drags come from
the mouse map, as the arrangement's do. The faders, sends, pans, buttons,
menus and meters are named for screen readers, such as "Lead Volume", which
is how the tests drive them. The meters read the engine's `Meters` every
frame, as of the frame the device plays (`Stats::time`, with
`Meter::level_at`), so while the engine renders ahead they move with what's
heard, not with what was rendered last ([engine.md](engine.md#built-in-devices)).
The window redraws at 60 frames a second while any of them shows a signal,
so they fall all the way back after you stop. The CPU part
(`cpu`) shows the channel's share of a core from the view's `Performance`
([CPU figures](#cpu-figures)), as a meter named such as "Lead CPU", and
"--" while there are no figures. The **Track** menu adds tracks
([commands.md](commands.md#track)).

An audio track's input menu lists the other tracks after the audio inputs,
each opening to its taps as a sidechain picker's tracks do
([The devices panel](#the-devices-panel)), then Resampling, the master's
Post Fader, as the manual's [Routing](../manual/src/using/routing.md) page
shows. A tap that would close a loop is listed but greyed out, and its
hover text says why (`Project::check_input`); a MIDI track's Pre FX, which
is notes, isn't listed. A MIDI track's input menu lists the other MIDI
tracks after the MIDI inputs the same way, each opening to its Pre FX, its
notes, and nothing that carries sound. The output menu has the buses
`Project::buses_for` gives the track, Sends Only last. They run
`mixer.set_input` and `mixer.set_output`
([commands.md](commands.md#the-mixers-controls)).

## The Routing panel

[The Routing panel](../manual/src/window/routing.md) in the manual says what
it lists. It's a child of the mixer's module
(`crates/ui/src/mixer/routing.rs`), so a row's controls are the strips' own:
the output menu's buses (`Mixer::bus_items`), the input menu's tracks and
Resampling (`Mixer::tap_items`), a send's level with its Pre switch
(`Mixer::send_in`), and a track's fader as a bar across, up to +6 dB
(`Mixer::volume_bar`). A key's source menu and gain are the Sidechain
column's (`native::sidechain::source_menu` and `gain`). So they hand back
the same `mixer::Change`s and `wire.*` commands, and they're named as the
strips' are, such as "Kick Output", with a key's named after its track, its
device and the side input, such as "Bass Compressor Sidechain Source". The
rows are listed from the project each frame: outputs, sends and inputs in
the tracks' order, then keys in the order of the devices they go into. Only
the rows in sight are drawn, but every row's names are measured, so that
each end's column is as wide as the widest name in it, a strip's width at
least (`Columns::across`), and the rows read across however wide the panel
is. Clicking an end runs `edit.select_tracks`, `edit.select_master` or
`device.select`, and the shell brings a device clicked into sight in the
devices panel (`Shell::show_device`). Its panel's id is
`musicable_ui::ROUTING`, in a tab beside the mixer, and
`view.toggle_routing` shows or hides it.

## The Performance panel

[The Performance panel](../manual/src/window/performance.md) in the manual
says what it lists. It draws the view's `musicable_ui::Performance`, the
figures the app worked out last ([CPU figures](#cpu-figures)): a `Cpu`,
the average, the longest block and whether it renders ahead, for each
`Row`, which is a device, a channel's clips, modulators or mixer, or the
metronome (`crates/ui/src/performance.rs`). The Renders column says Ahead
or Live, and the Longest column is empty for a row that renders ahead
(`Cpu::live_longest`), since its blocks have only to be ready before the
lead runs out. Each frame it lists the rows busiest
first (`Performance::busiest`), leaving out any whose device or track has
gone since, but while the pointer is over the panel the rows keep the
order they had, with any new ones at the foot, so a click lands on the row
it was aimed at as the figures change. Only the rows in sight are drawn,
over a Total line that stays in sight. The columns' headings say what the
figures mean in their tooltips, and each row is a list item named after its
channel and its part, such as "Bass, Drive", with its average and how it
renders as its value, such as "30%, live", which is how the tests read
it. Clicking a row runs `device.select` for a
device, and `edit.select_tracks` or `edit.select_master` for the rest of a
channel, and the shell brings a device clicked into sight in the devices
panel (`Shell::show_device`), as the Routing panel's clicks do. Its panel's
id is `musicable_ui::PERFORMANCE`, in a tab beside the mixer, and
`view.toggle_performance` shows or hides it.

## Recording

[Recording and comping](../manual/src/using/recording.md) in the manual says
how to record, monitor and comp, and where recordings go. Each pass of the
playhead over the song becomes a take on its track
([commands.md](commands.md#take)), and one undo takes the whole recording
back.

**Inputs.** The stream opens the inputs that armed tracks and tracks
monitoring In take, and keeps them open until the device changes. Opening
one restarts the stream, so it waits while recording. Applying new audio
settings opens those the tracks want with the new stream (`Output::apply`),
so a take that's recording carries on with them, even with the same
engine, as a new buffer size under 1024 frames keeps. A MIDI input's
channels read as MPE ([engine.md](engine.md#notes)), and MIDI inputs open
on Windows only so far ([audio.md](audio.md#midi-inputs)). A track's input
plays through its devices the stream's monitoring latency late
([audio.md](audio.md#inputs)), and a MIDI track's the stream's MIDI delay
late.

**Where it lands.** A track records what its input captured as each song
position was heard, so it lands where it was played: the audio backend
lines the inputs' buffers up with the output, frame for frame, and the
engine's timeline says which song position each frame played, and how late
the plugins' latency made it ([engine.md](engine.md#recording)). Latency
the driver doesn't report is made up for by the device's recording offset,
which the [settings](#settings-and-your-keymap) keep for each device, mode
and sample rate. A MIDI message lands the same way as audio, without
the offset: the backend stamps each with the frame heard as it came in.

**From another track.** A track that takes its input from another track's
tap, or resamples the master, records what the engine renders there
([engine.md](engine.md#inputs-from-taps)). The session has the engine
write each tap that armed tracks take, and with Capture on, each that
tracks monitoring In take, into a buffer like an input's
(`recorder::TapInput`), lined up with the output as an input's buffer is,
so the recorder reads it the same way, without the recording offset. A
MIDI track taking another's notes records what the engine played of them
at that track's Pre FX, which it writes into a buffer like a MIDI input's
(`recorder::TapNotes`): each note and controller move, packed in two
words, with the engine frame it played at, 16,384 of them. They go where
the song was as they played, without the latency, so the other track's
clips' notes land on them, and the recorder reads them as it reads a MIDI
input's, for recording and Capture. It asks for `MAX_WRITERS` at most: a
track taking a tap past those records nothing, and the status bar says so.
A new project or a new engine starts the writers over, since its taps are
other tracks'.

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
tracks, the notes held over loops and punches, where the controllers
start each pass, and recording and capturing from taps, of sound and of
notes; `crates/app/tests/routing.rs` records another track's Post Fader,
with a plugin's latency on a third, and checks it against that track's
stem in an export, bit for bit, captures it the same way, resamples the
master and checks it against an export of the mix without the resampling
track, and records and captures a MIDI track's notes on another, checking
they're its clip's notes and mod wheel exactly.

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

### Capture

[Capture](../manual/src/using/recording.md#capture) in the manual says what
it makes into takes, and how long it holds the inputs for (`capture_minutes`
in [settings.toml](#settings-and-your-keymap)). A capture asked for while
playing waits for the inputs to bring what was heard up to then, as a
recording does, for two seconds at most. An audio input is held as 32-bit
floats at the engine's sample rate, rounded up to a power of two of frames,
and a MIDI input's 2,000 messages for each second take 24 MiB for 5 minutes.
What's held is dropped when another project opens, when `capture_minutes`
changes, and when new audio settings replace the engine.

Every 10 ms, the recorder copies each held audio input's channels from the
stream's 8-second buffer into a buffer of its own, at the same engine
frames, each held tap's from the buffer the engine writes it into, and each
held MIDI input's messages, with the frames heard. It
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

## The devices panel

[The devices panel](../manual/src/window/devices-panel.md) in the manual
has what a device's box shows. Each box's title bar starts with its Bypass
button, drawn as Live's power button (`widgets::Face::Power`), lit in the
theme's `switch` colour while the device plays. What a click on it does
is the mouse map's `BypassButton` target: in Live's map, a click bypasses
the device or brings it back (`device.set_bypass`), and Alt-click
deactivates it or activates it (`device.set_active`), as in Cubase; a
click on a deactivated device's button activates it. A bypassed or
deactivated device's controls are drawn at the theme's `devices.dimmed`
opacity, and still work, and a deactivated one says so beside its name.
The name's menu has Bypass, ticked while the device is bypassed, and
Deactivate or Activate. Screen readers hear the button as "EQ Bypass",
on while the device is bypassed, and the name with the device's state,
such as "Device EQ, bypassed, selected". After its delete button, an
active device's title bar has its share of a core from the view's
`Performance` ([CPU figures](#cpu-figures)), amber over
`performance::BUSY`, a quarter of a core, and red over `VERY_BUSY`, half
of one, as a meter named such as "EQ CPU", with its longest block in its
tooltip. A deactivated device doesn't render, so it has none.

A click on a device's title bar, or on its box where no control takes it,
selects the device (`device.select`), as the mouse map's `DeviceTitle`
target says: in Live's map, Ctrl-click and Shift-click add it to the
selection or take it out, and a click beside the boxes selects none. The
selection is the editor's (`Editor::devices`, [commands.md](commands.md#the-editors-state)),
of the chain the panel shows, and selecting gives the panel the focus, so
the edit commands act on the selected devices and the panel's keys
(`[panels.devices]` in the keymap) do too: 0 bypasses them, or brings them
all back (`device.toggle_bypass`), and Ctrl+Left and Ctrl+Right move them
one place along the chain (`device.move_left`, `device.move_right`). They're
outlined in the theme's `clip_selected` colour. A device's menu works on
the selection if the device is in it, and otherwise selects the device
alone, as in Live, and so does a drag.

Dragging a title bar moves the devices (`device.move`), and Ctrl-dragging
copies them (`device.copy`), as the `DeviceTitle` target's drag says; Escape
lets go of them. Over the panel, a line in the `start_marker` colour shows
where they'd go between the devices, which `DevicesPanel::drop` works out
from the boxes it drew last, and devices and presets from the browser go in
at the line too. Over the arrangement they go on a track's row or the
master's, or under the tracks on a new track, as the browser's devices do
([The browser](#the-browser)), and onto a mixer strip, at the end of its
track's devices. A moved device keeps its id, so its node and its plugin go
with it: a plugin's window stays open, and the graph swaps at once, without
a crossfade, even while the song plays.

Every device's Bypass is a parameter automation can move
([commands.md](commands.md#automation)): the arrangement's lane chooser
lists it first among each device's parameters, and the lane's header says
"On" while the device is bypassed and "Off" while it plays. A plugin's own
bypass parameter isn't listed, as the device's Bypass covers it
([plugins.md](plugins.md#plugins-on-tracks)).

### The modulator strip

[Modulation](../manual/src/using/modulation.md) in the manual has what the
strip does. `modulators::strip` draws a card for each of the chain's
modulators at the left of the devices panel, before a separator, and the
panel shows it whenever the chain has modulators, devices or not. **Add
Modulator**, in the panel's header, runs `modulation.add`. A Macro's card
draws its knobs and buttons as `native::macro_control`s, and an LFO's or an
Envelope's draws its parameters in the built-in devices' columns
(`native::modulator_controls`), so their bars, menus and automation marks
are the devices' own. A momentary button sends 1 when it's pressed and 0
when it's let go, inside one gesture (`Action::BeginGesture`), so the two
edits cancel out and the history keeps no step.

Each output has a handle, filled to its reading. Dragging one carries a
`browser::Dragged::Output` in egui's drag and drop, which the browser's
drop targets ignore. Every control that modulators move checks for it as
it's drawn (`modulators::Shown::on`): it's outlined while the output could
move it, as `modulation.add_routing` would take it (in reach, a parameter
that can be modulated, and not one the output moves already), and being let
go over adds the routing at 25%. The devices' bars, the mixer's faders, pans
and sends, and the sidechains' gains all go through `Shown`, which also
draws the line from as low to as high as the routings can take the control
(`modulators::reach`, adding up each routing's span after its curve and
scale, times its amount) and the dot where the reading has it, in the
theme's `modulation` colour. Each control's menu lists its routings
(`modulators::routing_items`), each opening to its settings, and the
handle's menu lists the output's, with **Route To**. Dragging a routing's
Amount or Lag is a gesture, so the history keeps one step for it.

The readings come from the engine's `Meters`, which the app follows with
the project, as it does the meters ([engine.md](engine.md#built-in-devices)),
and hands the window each frame in `mixer::Levels::outputs` and `moved`, as
of the frame the device plays (`Reading::get_at`), as the meters are. It
redraws at 60 frames a second while they change, and clears them while no
audio plays, so a stopped engine's last readings don't linger: a Macro's
handles then show its controls' own values.

## Built-in devices

[Built-in devices](../manual/src/devices/index.md) in the manual has a page
for each, with its controls in tables made from the code
([CONTRIBUTING.md](../CONTRIBUTING.md#the-manual)). Each is a node in the
engine ([engine.md](engine.md#built-in-devices)), and the project saves its
settings ([project-format.md](project-format.md)).

In the devices panel, a built-in device shows its controls in columns, a
column for each group of them, and more columns, side by side, for a group
of more than five. A bar fills from zero for a range either side of it,
such as a gain in dB, and a control with whole steps, such as the synth's
voices, moves in whole steps however slowly it's dragged. The mouse map's
`DeviceControl` target says which drag moves a bar finely. The engine hears
each change as it's made, and smooths it over 10 ms, or moves it its
device's own way: the delay fades in a new time over 50 ms, and the reverb
glides to a new size. Screen readers find each control by the device's
name and the control's, such as "EQ Band 3 Freq", and can set a bar to a
number.

The Compressor's and the Gate's Sidechain column (`native::sidechain`)
starts with a picker for each wire into the device's side input, with the
wire's gain under it, or one picker that says No Input. A picker's menu has
No Input, then each other track, opening to its taps, then This track,
opening to the points before the device on its own chain, as the manual's
[Sidechains](../manual/src/using/sidechains.md) page shows. A tap the side
input takes already, or one that would close a loop, is listed but greyed
out, and its hover text says why (`Project::check_wire`). Picking runs
`wire.connect`, `wire.set_source` or `wire.disconnect`
([commands.md](commands.md#wires)), and the + by the column's name adds
another source. A wire's gain is a bar like the device's own
(`native::Setting::WireGain`), which runs `wire.set_gain`, and its lane,
named after its source, such as "Sidechain from Kick Post FX", is among
the device's parameters in the arrangement's lane chooser. Screen readers
hear the pickers as "Compressor Sidechain Source", then "Source 2" and on,
the gains as "Compressor Sidechain Gain", and the + as "Add a Source to
Compressor Sidechain".

A plugin's box has the same, a row for each of its side inputs, as its
device lists them (`PluginDevice::inputs`), under the plugin's name for
each, or "Side Input" if it has none (`Project::input_name`), and the
pickers as wide as the box. Picking runs `wire.connect` with
the input's id ([commands.md](commands.md#wires)), and screen readers hear
"Pro-C 2 Sidechain Source" and "Pro-C 2 Sidechain Gain", with the plugin's
name for the input. With No Input, a plugin's side input is off
([plugins.md](plugins.md#side-inputs)), so the picker's hover text says
so, where the Compressor's says it keys from its own input.

A sampler reads the whole of its file when the graph is next built, and
samplers playing the same file share it. Loading and removing a sample are
steps in the undo history: "Load Sample" and "Remove Sample". Every control
of a built-in device can be automated
([commands.md](commands.md#automation)), and an automation lane spreads
its values as the control does.

## Plugins

[Plugins](../manual/src/using/plugins.md) in the manual says where the app
finds plugins, how they go on tracks and what their windows do, and [The
devices panel](../manual/src/window/devices-panel.md#plugins) what a plugin
device shows. [plugins.md](plugins.md) has how the app hosts them: the scan,
in a process of its own for each library ([Scanning](plugins.md#scanning)),
and the devices, their windows, their state and undo ([Plugins on
tracks](plugins.md#plugins-on-tracks)). What a plugin changes on its own, as
it loads or takes in what undo gave it, or with its window closed, goes into
the project without a step, so redo carries on past it. Delay compensation
lines up every track with the latest one, so a plugin that looks ahead
doesn't put its track out of time
([engine.md](engine.md#delay-compensation)).

## The browser

[The browser](../manual/src/window/browser.md) in the manual says how its
lists work: the samples in the library, the devices and the presets. What
the browser does is commands (`browser.*` and the device commands,
[commands.md](commands.md#browser)), so keys, the command palette and agents
reach it too, and a search an agent runs shows in the browser.
`crates/library` keeps the library, and `crates/app/src/library.rs` puts it
to work for the app ([How it's built](#how-its-built)).

- The library is kept in `library.json` in the config folder ([Settings and
  your keymap](#settings-and-your-keymap)). A tagged file that moved within
  its folder keeps its tags, and with the same time of change, what its
  header said.
- Find Similar goes on the names and the lengths alone, until analysis
  ([Not done yet](#not-done-yet)) gives sounds an embedding to compare.
- A file up to about twelve seconds long at 44.1 kHz is read whole to
  preview it, and a longer one streams ([engine.md](engine.md#previews)).
  The browser keeps Auto and the previews' volume in `settings.toml`.
- Musicable's own presets are `.musicable-preset` files, in a folder for
  each device in the Presets folder, or in the `[browser]` settings'
  `presets_folder`.
- The Devices list puts each plugin in its kind's folder, as the user
  marked it (`browser.set_plugin_kind`) or the scan found it
  (`Session::plugin_kind`, [plugins.md](plugins.md#plugins-on-tracks)).
  The app keeps the marks in `settings.toml`'s `[plugins.kinds]`, saves
  them as one changes, and lists the plugins again.

### Behind the browser

**One queue for file work.** Reading the library file, looking through
folders, reading the headers of the files found, finding presets, opening
the files to preview and drawing their waveforms all happen on one queue
of the library's own (`musicable_library::Queue`), never on the window's
thread or the engine's workers. It has a thread for every two cores, one
to four of them, called `musicable-files-1` and up, which run below normal
priority. A preview comes first, then a waveform, then scans. Scans and
headers run on every thread but one while the transport is stopped, so a
preview never waits for a thread, and on one while it plays, and while
recording they rest as long as they work, so the disk and the cores are
left to the recorder and the engine. Headers are read 64 files to a job,
each job adding the next as it ends, so the jobs added meanwhile go
first. On Windows and macOS they also run in the system's background
mode, which puts their reads behind others. The analysis of the project's
audio files runs on it too, one file at a time
([analysis.md](analysis.md#in-the-app)).

**Why headers come after.** Opening a file takes far longer than finding
it. On a Windows PC with Defender's real-time protection on, opening each
file took about 7 ms, so a library of 355,764 files on a SATA SSD took 43
minutes when the browser listed nothing until every header was read,
while listing its folders, each file's size and time of change and all,
took 1.7 s. Asking a file for its size and time opens it on Windows too,
which took 24 s for that library, so they come from the folder listing,
and only links are asked. Opening files on several threads at once went
several times faster there, which is why headers are read on several.

**The library file** has a record for each file, with room for what
analysis will add: tags with where each came from (the name, a
measurement, a model or you) and how sure it is, measured values, an
embedding for finding similar sounds, and the version of the analyser that
made them. The browser fills in only the names, the file's size and time
of change, and its header: format, sample rate, channels and length. A
file whose header is still to be read says `"unread": true`. It's saved a
couple of seconds after a change, so tagging a run of files saves it
once, and when the app closes. Headers read in the background save it
only once a folder's are all read, as a large library's file is tens of
megabytes. A library file that can't be read is kept beside the new one
as `library.json.broken`, and the status bar says so.

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
| Scanning 20,000 files the first time | 61 ms |
| Reading their headers, on one thread | 158 ms |
| Scanning them again, unchanged | 72 ms |

The scan is of 20,000 small WAV files the bench writes, which the system
has cached, so it measures Musicable's work, not the disk's or an
antivirus's ([above](#behind-the-browser)). A search goes through every
file's name and folders, in lower case, one after the other in a single
string for each folder of the library, so there's no index of words to
keep up to date.

## Keys

[How keys reach
commands](../manual/src/customising/keys.md#how-keys-reach-commands) in the
manual says where key presses go. Every key press goes to the keymap before
egui sees it, through `musicable_ui::KeyPresses`. A bound chord runs its
command through `Session::key_pressed` and goes no further. The panel with
the focus has its own keys first, then the ones that work everywhere ([The
panels](#the-panels)). Unbound keys go on to egui, and so do keys whose
command doesn't apply now (`Session::takes_key`), so egui moves the keyboard
focus with the arrow keys where no command takes them. Key presses never
reach the engine any other way.

- While a text field has focus, every key goes to it instead. Ctrl+C,
  Ctrl+X and Ctrl+V reach the keymap like other chords, since eframe's
  clipboard shortcuts are off, and copy, cut and paste only in text fields.
- Holding a key down runs its command once: the repeats the system sends
  are swallowed.
- While the Settings window's Keys page waits for the keys to give a
  command, `KeyPresses::capture` takes every key press there instead of
  `KeyPresses::take`, Escape and Enter too, and nothing else sees it.

### Keyboard layouts

Keymaps name keys after a US keyboard
([commands.md](commands.md#key-chords)). `chord`, in
`crates/ui/src/keys.rs`, names a key press on another layout from what egui
reports, the key as the layout has it and the key in the same place on a US
keyboard, as [Keyboard
layouts](../manual/src/customising/keys.md#keyboard-layouts) in the manual
describes. egui reports the numeric keypad's keys as the main keyboard's,
but for NumpadAdd, and the Windows key isn't reported to the app, so `Cmd`
chords only work on macOS.

## Files

[Projects and files](../manual/src/using/projects.md) in the manual says how
projects open and save. Project files are read and written by
`musicable_io::project` ([project-format.md](project-format.md)), which
writes through a temporary file, so a failed save leaves the old file alone.

The file commands are real commands (`file.new` and so on, see
[commands.md](commands.md#file)), so they can be remapped. A command can't
show a dialog, so it pushes a `Request` into its `Context`, and the app
carries it out after the command has run. Given a file, as agents give
them, Open, Save As, Import Audio and Export Audio skip the dialog
([agents.md](agents.md#what-agents-can-run)).

## Audio files

[Audio files](../manual/src/using/projects.md#audio-files) in the manual
says how clips name their files, and what happens when one is missing. An
audio clip's path is relative to the folder the project file is in, or
absolute ([project-format.md](project-format.md#track)). The app reads WAV,
AIFF and FLAC files (`musicable_io::audio` lists the sample formats), and
resamples a file at another rate than the device's. Adding files runs
`clip.insert_audio`, which reads each file's header first, so the clips are
as long as their files, and auto-warps them by what analysis found in them
([commands.md](commands.md#auto-warp)).

Each time the session builds a graph, on the compiler's thread, it finds
the files the project's audio clips play (`crates/app/src/media.rs`):

- A file up to 2²¹ frames long, about 44 seconds at 48 kHz, loads whole.
- A longer one streams: the disk thread reads it ahead of each clip that
  plays it, and from where the clip plays after the loop jumps back
  ([engine.md](engine.md#clips)).
- A graph rebuilt for an edit gets the files and streams the old one has,
  so nothing is read again and the clips that stream play on. Opening a
  project reads its files again, so it plays them as they are now.
- A clip in Granular streams with room for as far apart as its grains read
  (`granular::stream_spread`, [engine.md](engine.md#clips)). One whose
  grains come to read further apart than its stream holds, as they get
  longer or are transposed further, gets a new stream with the graph
  rebuilt for the edit.
- A clip in Granular that follows the pitch gets the pitch marks analysis
  found in its file ([analysis.md](analysis.md#pitch-marks)). As analyses
  land, the session gives the pool their marks (`Session::take_pitch_marks`,
  `MediaPool::keep_marks`), by the file's name as the clips give it, and
  builds a new graph where such a clip plays a file whose marks are new;
  until then, its grains keep to the lattice. An offline session finds the
  marks itself as its graph builds, so an export follows the pitch whether
  or not the file was analysed. The marks aren't saved in the project: the
  analysis cache keeps them.

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

## The render cache

[Spectral](../manual/src/using/warping.md#spectral) in the manual says what
it does for a clip in Spectral, which plays segments of its file rendered
ahead ([engine.md](engine.md#clips)). `crates/app/src/renders.rs` renders
them and keeps them:

- As the session builds a graph, `MediaPool::media` plans each clip in
  Spectral at the song's tempo (`spectral::Plan::new`) and asks the render
  cache for its segments (`RenderCache::plan`). One that a graph has
  already, rendered or still to be, is shared, by its key: the segment's
  own (`SegmentPlan::key`) hashed with its file's, from the file's path,
  size and modification time when it was read. The cache keeps each as a
  `Weak`, so what no graph holds any more goes, and `prune` forgets it.
- The others render on the library's queue at `Priority::Soon`, ahead of
  scans and analysis, the segments the playhead plays next first
  (`renders::order`), and the status bar counts them down
  (`RenderCache::pending`). A job looks its segment up as it runs, and does
  nothing if no graph wants it any more or it's rendered already. It
  renders from the file in memory, or else from the file, opened afresh, as
  long as its key still matches: a file changed since it was read isn't
  rendered from.
- An offline session, such as an export's or the null test's, waits for
  its segments: `media` renders them there and then, on every core
  (`RenderCache::render_now`), before the graph is built, and so it does
  those its graph shares that are still waiting in the background.
  Whichever renders a segment first fills it. A session that goes stops the
  renders it waits for between segments (`MediaPool::stop`).
- The cache is the app's, the same for every session
  (`RenderCache::global`), so an export finds what playback rendered.

**The folder.** Rendered segments are kept in `renders` in the cache
folder ([The cache
folder](../manual/src/customising/settings.md#the-cache-folder)), a file for
each, named by its key, `KEY.render`: a 24-byte header, `MSRN`, the format
(1), the channels, the rate and the frames, then each channel's frames in
turn, as little-endian 32-bit floats. A file is written to a part file
beside it and renamed, so a reader never sees half of one, and one whose
header doesn't match the segment asked for counts as missing. A job looks
in the folder before it renders, and reading a file sets its modification
time, which says when it was used. Before writing one, the cache makes
room: while the folder would hold more than `render_cache.max_size_gb` or
leave less of its disk free than `render_cache.min_free_gb`, the files used
longest ago go, until a tenth of the size is to spare, up to 1 GB, so the
next writes needn't look again. Lowering a limit on the Settings window's
Files page lets go at once (`RenderCache::set_limits`).

## Exporting audio

[Exporting audio](../manual/src/using/exporting.md) in the manual has the
export window's settings and the files it writes, and [The command
line](../manual/src/reference/command-line.md#exporting) the options of
`musicable export`. The export renders on a thread of its own, while the app
plays on, and the export window keeps its settings in `settings.toml`.

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

On a 4-core cloud VM, the clips demo ([Audio files](#audio-files)), 38.4
seconds and four tracks, exports at 54 times real time to a WAV file, 39
times to FLAC, and 22 times with the mix and the four stems in FLAC. The
engine sets the pace: the null test renders it at 46 times real time.

[`crates/app/tests/export.rs`](../crates/app/tests/export.rs) is the done
test: an exported mixdown is what the app plays live, sample for sample,
with a latent plugin, an audio file at 44.1 kHz, a clip in Spectral, a
test instrument and a muted track, the live render asked for in 441-frame
buffers. Each stem is
its track soloed, the tail is the release the live render plays, and
normalised, 16-bit, AIFF, FLAC and 96 kHz files hold the same samples,
scaled or rounded. A cancelled or failed export leaves no files behind.

## Settings and your keymap

The app keeps its settings, the user's keymap, mouse map, themes, workspaces
and profiles in its config folder, which `musicable_app::config_dir` finds,
or the `MUSICABLE_CONFIG_DIR` environment variable picks, which is handy for
testing. [The config
folder](../manual/src/customising/settings.md#the-config-folder) in the
manual lists what's in it, and [The settings
file](../manual/src/customising/settings.md#the-settings-file) every
setting. Without a config folder, changes last until the app closes.

- `settings.toml` is `musicable_app::Settings`, every field of which can be
  left out, and fields this version doesn't know are ignored, so a file from
  a newer version still opens. The Settings window writes it as soon as a
  setting changes, and the app reads it at startup.
- `keymap.toml` is the user's changes to the default keymap (`UserKeymap`,
  `crates/app/src/keymap_file.rs`, [commands.md](commands.md#the-keymap-file)),
  and `mouse.toml` theirs to what the mouse does
  (`crates/app/src/mouse_file.rs`). Each is written at once on every change,
  with a backup first when the write would lose something.
- `workspaces` holds a file for each workspace ([Workspaces](#workspaces)),
  and `profiles` a folder for each profile but Default
  ([Profiles](#profiles)). The keymap file, the mouse map file and the
  workspaces in the config folder itself are Default's.
- `plugins.json` is what the plugin scans found
  (`crates/app/src/plugin_scan.rs`, [plugins.md](plugins.md#scanning)),
  `library.json` the browser's library ([Behind the
  browser](#behind-the-browser)), and `musicable.log` the log file
  ([Running it](#running-it)).

## Themes

[Themes](../manual/src/customising/themes.md) in the manual has the theme
files and what their entries change. `crates/app/src/theme_file.rs` keeps
the theme in use, a built-in one or a theme file, which it looks at twice a
second and reads again whenever it changes, and the views draw from the
`musicable_ui::Theme` it makes ([The arrangement](#the-arrangement)). A
theme colours egui's own widgets too: `[widgets]` is egui's visuals, and
`[fonts.sizes]` its text sizes. The built-in fonts, `Ubuntu-Light` and
`Hack`, are egui's, and text a theme's font can't write falls back to them.

## Audio

`musicable_app::Output` keeps the audio device playing the engine:

- At startup it opens the stream the settings ask for. If the device in the
  settings isn't plugged in, it plays through the default output instead, and
  says so; the settings keep the device for next time.
- The engine is built for the stream, with its sample rate, and blocks as
  long as its period or as the engine renders ahead in (`AHEAD_BLOCK`, 1024
  frames), whichever is longer (`output::engine_config`), so plugins are
  activated for both, and a change of buffer size under 1024 frames, or of
  Render Ahead, doesn't build another. It always renders with the parallel
  scheduler (`musicable_app::engine`). Exports render in blocks as long.
- When the stream ends by itself, because the device was unplugged or its
  format changed, the output tries it again at once and then every two
  seconds. An ASIO driver that asks to be reset, after a change in its own
  settings, opens again at once with them, and the app says what it plays
  with now. While nothing plays, the engine waits, and the commands sent to it
  wait in its queue and the session's.
- **Apply** on the Audio page stops the stream and opens the new one. If
  it needs another sample rate or block size, a new engine takes over:
  `Session::replace_engine` sets it up with the project's graph, tempo map
  and loop, at the same place in bars and beats, playing if the old one was.
  If the new stream doesn't open, the old one comes back, and the error shows
  on the page.

[Setting up audio](../manual/src/getting-started/audio.md) in the manual has
the Audio page. Its devices, modes, rates and buffer sizes come from
`musicable_audio::output_devices` and `capabilities`
([audio.md](audio.md#opening-a-stream)), and "Now playing" from the stream's
`StreamInfo`: drivers round the buffer size, and some ignore it. On Windows
the page has a driver type above the device, WASAPI or ASIO
([audio.md](audio.md#asio)), and for ASIO, Driver Settings opens the playing
driver's own settings. The foot of the page has the notice the GPL asks for.
The Recording page's offset is the playing device's [recording
offset](#recording), in its mode and at its rate, and the next recording
uses it, as does Capture.

**Render Ahead**, below the device's settings, is the engine's lead
(`EngineHandle::set_lead`, [engine.md](engine.md#rendering-ahead)): Off,
Short, Normal or Long, `audio.render_ahead` in the settings file, Normal
when it's left out (`settings::RenderAhead`). It applies at once, with no
Apply, and the session keeps it (`Session::set_lead`) to set on every
engine that takes over.

### Meters

- **CPU**: the share of the time the audio thread spent in its wake-ups,
  rendering included, over the last quarter of a second. Near 100%, wake-ups
  start running late and dropouts follow. It comes from
  `StreamStats::busy`. The tooltip adds the longest wake-up, how many
  threads the parallel scheduler kept busy at once while it shared blocks,
  and, while there are CPU figures, the project's share of a core, which
  is the Performance panel's total ([CPU figures](#cpu-figures)), and the
  share of what rendered ahead (`Performance::ahead`).
- **The lead gauge**, under the CPU meter while Render Ahead is on: the
  engine's lead as a share of the lead it aims for (`Stats::lead` over
  `lead_target`), over the last quarter of a second, from `LeadMeter` in
  `output.rs`, which the view carries as a `musicable_ui::LeadGauge`. It's
  amber under half while the engine renders ahead, and red over a quarter
  of a second in which `Stats::short_blocks` rose: the audio thread had to
  render blocks itself. While a track monitors an input, nothing renders
  ahead and it's empty, whatever rendered ahead earlier in the quarter of a
  second, and another setting starts the quarter again, so the gauge and
  the tooltip show nothing from the setting before. The CPU meter's
  tooltip, which the gauge shares, says how far ahead the engine was, of
  how far it aims, and the short blocks since the stream started
  (`Output::short_blocks`).
- **Dropouts**: `StreamStats::underruns` since the stream started. Click to
  count from zero again. The tooltip has the audio lost, how many of the
  dropouts were in the system's audio engine or driver, and the late
  wake-ups.

### CPU figures

The engine times every node it renders
([engine.md](engine.md#node-timing)), and `Figures`
(`crates/app/src/performance.rs`) works out from those times the CPU
figures the views show: each device's, each channel's and the project's
share of a core, and each one's longest block. The app hands it the
running graph's loads (`Session::loads`) each frame, after the levels:

- **Four times a second** it reads every node's load: the time the node
  has taken, and its longest block since the reading before, with the
  frames the engine has rendered, played and ahead (`Stats::rendered`), so
  while the lead fills or empties, the figures are still over the audio
  rendered. They're as of then, as the loads are, not as of the last
  callback, as `Stats::time` and `Stats::lead` are, since the engine's
  threads render ahead between callbacks. It keeps the readings that span at
  least the last second of audio rendered, and works the figures out from
  the oldest of them and the newest.
- **Each node goes in a row**, by what its key belongs to
  (`compile::keys::owner`): a device's node in the device's row, a track's
  clips, input and clip type in its Clips row, and a channel's modulation
  in its Modulators row. Everything else of a channel's goes in its Mixer
  row: its fader, pan and slots, its sends, its bus, the wires into its
  devices' side inputs, and the delays that delay compensation added after
  its nodes. The project says which channel a device, a send or a wire is
  on (`Places`).
- **A row's average** is the time its nodes took between the oldest
  reading and the newest, over the audio the engine rendered meanwhile. The
  difference is each node's, so a node new since the oldest reading counts
  all the time it took, and one gone since counts nothing, and a row
  doesn't drop to nothing for a second when one of its nodes goes, as
  when a send is deleted. Each delay is a node of its own here, by where
  it is (`LoadKey::Delay`), so the same goes for one of the delays after a
  node. A channel's figure adds up its rows, and the total adds up them
  all.
- **A row's longest block** is the longest any of its nodes took in the
  readings after the oldest, over the stream's period: the device's, not
  the engine's `max_block`, which is longer. Only blocks the device waited
  for count (`Load::add`), so the long blocks rendered ahead just before a
  track armed don't make Longest read up to 16 times too high for the
  second after.
- **A row renders ahead** if the engine rendered ahead at the newest
  reading (`Stats::lead_target` above zero). In this milestone the whole
  graph renders ahead or live, so every row says the same, and
  `Performance::ahead` adds up the rows that render ahead.
- While no stream plays there are no figures, so the views show none, and
  a graph the engine hasn't installed yet waits until it has
  (`NodeLoads::installed`): until then its loads read nothing, or only
  some of what they carry on from the graph before, and a reading of a
  node's load that hadn't carried on yet would count all its time for a
  second, once that reading was the oldest. Another engine, or one with
  another sample rate or period, starts the readings again.

The `View` carries the figures as a `musicable_ui::Performance`, which the
devices' title bars ([The devices panel](#the-devices-panel)), the mixer's
CPU part ([The mixer](#the-mixer)), the CPU meter's tooltip and the
Performance panel ([The Performance panel](#the-performance-panel)) show,
and agents read them with `get_performance`
([agents.md](agents.md#the-tools)). The window redraws at least four
times a second, so they keep up.

## How it's built

- `crates/ui` draws the shell with egui. It depends on the model, the
  commands and the library's types only. The app hands it a `View` of what
  to show each frame, and carries out the `Action`s it hands back. Its
  tests drive the shell with `egui_kittest`.
- `crates/ui/src/parts.rs` keeps the views made of parts. Each kind of part
  registers its id, title, size, the views it fits and what draws it, in
  `transport_bar.rs`, `arrangement/header.rs` and `mixer.rs`, so a feature
  adds a part in one place. The views ask it which parts to show, in what
  order and at what sizes, and `Views` is what a workspace keeps of them.
- `crates/library` is the browser's library: the index and its file,
  scanning folders, searching, presets, and the queue for file work, with
  no app or UI in it. `crates/app/src/library.rs` puts it to work for the
  app: it runs the library's file work on the queue, hands the engine the
  previews, and reaches the commands through `LibraryLink`.
- `crates/app` has the eframe app (`App`), which holds the `Session`, the
  `Output`, the Settings window, the user's keymap file (`UserKeymap`) and
  the agents' server (`app/agents.rs`, [agents.md](agents.md)), and the
  binary. Debug builds install the engine's
  allocation checker, as the tests do. The Settings window
  (`settings_window.rs`) lists its pages, draws the small ones, and hands
  what changed to the app to carry out and keep; the Audio, Mouse,
  Appearance and Profiles pages have files of their own (`audio_page.rs`,
  `mouse_page.rs`, `appearance_page.rs`, `profiles_page.rs`), and the
  shell draws the Keys page, as it holds the keymap's commands.
  `profiles.rs` lists the user's profiles and keeps their folders, and the
  app reads the profile in use's files at once when it switches.
  `log_file.rs` keeps the log file, and logs panics and the graphics device
  being lost.
- [`crates/app/src/app/remapping.rs`](../crates/app/src/app/remapping.rs)
  drives the whole window with `egui_kittest`, handing it key presses as
  eframe does, without an audio device. It gives every command new keys on
  the Settings window's Keys page, finds every command in the command
  palette with them, and starts the app again to check they were saved.
  [`crates/app/src/app/configuring.rs`](../crates/app/src/app/configuring.rs)
  opens the Settings window at each page, turns from page to page, changes
  every setting it has a control for and reads the settings file back, and
  checks what Escape and clicks do while the Keys page waits for keys.
  [`crates/app/src/app/switching_profiles.rs`](../crates/app/src/app/switching_profiles.rs)
  switches to a profile with its own theme, keys, mouse, behaviour and
  workspace, from the Profiles page and with Switch to Next Profile, and
  makes, copies, renames and deletes profiles on the page.
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
  [`crates/app/src/app/undoing.rs`](../crates/app/src/app/undoing.rs)
  makes changes, goes back to the first by clicking it in the History
  panel, steps through the rest with the panel's arrow keys, and moves the
  panel to a window of its own, which comes back with the app.
  [`crates/app/src/app/controlled_by_agents.rs`](../crates/app/src/app/controlled_by_agents.rs)
  lets an agent in on the Agents page and has it run, undo and batch
  commands, save, open and export ([agents.md](agents.md#testing)).
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
  and opened again, undoing changes made in their windows, moved to
  another track with a window open and copied, missing, and lined up with
  a dry track by delay compensation. It also runs the app's
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
- A plugin's parameters in the devices panel; crossfading the swap when a
  device moves while the song plays, which switches at once for now; and
  grouping the selected devices, which Ctrl+G is kept free for.
- For modulators: Macro colours and snapshots, and routing to a VST3
  plugin's parameters, which Route To leaves out until the engine moves
  them. A plugin's parameters have no controls to drag a handle onto yet,
  so Route To is the way to them.
- In the browser: analysis, which will tag sounds by how they sound and
  find similar ones by an embedding; previews in time with the song;
  watching the library's folders, which are looked through at each start
  and on Rescan; CLAP presets, which need the
  plugins' preset discovery; renaming and deleting presets, and saving
  VST3 presets; and tags in the undo history. Taking a folder out of the
  library forgets its files' tags.
- Dropping an audio file from the file manager onto a sampler, as one
  from the browser goes: files dropped on the window become clips, wherever
  they're let go.
- VST3 plugins' windows on macOS and Linux.
- Nudging clips with the arrow keys, consolidating them, and crossfades
  drawn between clips.
- Editing a theme on the Appearance page, with colour pickers: theme
  files are edited in a text editor for now. Plugins' own windows keep
  their own look.
- In the piano roll: the notes' MPE expression, shown and edited (Tier 2),
  drawing velocities across the lane, folding to the keys in use, scales,
  and repeating the arrow keys while they're held.
- Keymaps in the style of other DAWs (Logic, Cubase, Pro Tools). Each would
  be a keymap file to import.
- A macOS default keymap with Cmd.
- Following the default output when it changes.
- Carrying on when the graphics device is lost, which eframe can't do: the
  app crashes, losing unsaved changes. A copy of the project to restore
  after a crash, as Live offers, would keep them.
- An icon, and opening `.musicable` files from the file manager.
- Exporting to MP3 or other lossy formats, stems with the returns' and the
  master's effects (Live's "Include Return and Master Effects"), and
  remembering which tracks were ticked for stems.
