# Commands and keymaps

Every user action in Musicable is a named command, and key presses reach
commands only through a keymap. Menus, the command palette and the mouse
gestures of the arrangement, the piano roll and the Sample Editor run the
same commands, and so do agents ([agents.md](agents.md)), and scripts will, so remapping a
shortcut is just editing the keymap. This page covers
the commands so far, the editor's state they read and change, the
transactions they change the project through, undo and redo, how the app
runs them, key chords and the keymap file. How the app's window hands key
presses to the keymap is in [app.md](app.md#keys), and how people change
their keys in the manual's [Keys](../manual/src/customising/keys.md) page.

## Commands

A command has a dotted id, `area.action` in lower case, and a title in title
case for menus and the palette. Keymap files name commands by id, so a
command keeps its id once it has shipped. Each area registers its commands
with the `Registry`, as `transport::register` does, and
`musicable_commands::register_all` registers every area's. A new command
also needs an example for the undo tests
([below](#testing-every-command)). A command that only shows a window or
asks the user something, which an agent can't see or answer, is marked
`not_for_agents` (`Command::agents`), and one that can't be part of a
batch, as those that step through the history or open and save files,
`alone`. A running command
gets a `Context`: the open project's `Document`, with the editor's state
(below), the engine's `Transport`, the browser's `Library`, and a list of
`Request`s for what only the app can do, such as showing a file dialog.

### Arguments and results

A command can take arguments, and give back a result, so the arrangement's
gestures, scripts and agents can say exactly what to change. Its `params`
list them: each `Param` has a name, a `Type` (a bool, an integer, a number,
text, a position in beats, a track, a clip, a note, a locator, a device, a
take, a modulator, a routing, a parameter, or a list of one of those),
whether it's optional, and a line on what it's for.

- `Registry::run_with(id, &args, ctx)` runs a command with `Args`, built
  with `Args::new().with("clips", ids).with("by", Beats::new(4))`. It checks
  them against the params first: a missing required argument, one the
  command doesn't take, or one of the wrong type is an error, and nothing
  runs. It returns the command's result, a `Value`, such as the ids of the
  clips Move Clips copied.
- A command that names something the project doesn't have, or asks for what
  can't be done, such as an audio clip on a MIDI track, returns
  `CommandError::Invalid` with the reason, and changes nothing.
- `Registry::run` runs one without arguments, as keys, menus and the
  command palette do.
- `Command::needs_arguments` says whether it has a required param. Keys,
  menus and the palette can't give arguments, so they leave those commands
  out: the Keys page and the palette don't list them. The
  context menus of the arrangement and the piano roll fill them in from
  where they were opened ([app.md](app.md#the-arrangement)).
- A param made with `Param::asked` is one the app asks the user for when
  it's left out, such as Open's file. Scripts and agents, which can't
  answer, must give it.
- `Registry::run_batch(name, &commands, ctx)` runs commands in order as one
  step in the undo history called `name`, all or none: if one fails, the
  project, the editor's state and the history go back to how they were,
  redo included, nothing they asked the app for is asked for, and the
  `BatchError` says which failed and why. Commands that run `alone` can't
  be part of one.
- The `json` module reads a command's arguments from JSON by its params
  (`args_from_json`, and `agent_args`, which refuses commands not for
  agents), writes results as JSON (`to_json`), and describes the params as
  a JSON Schema (`schema`), for agents and `musicable api`
  ([agents.md](agents.md)). Positions are numbers of beats, and ids are
  whole numbers.

### When a command applies

A command's `when` says when it applies without arguments: always, or only
with clips selected, with something to paste, and so on (`When`). Menus,
context menus and the palette grey out a command that doesn't apply, and a
key bound to one goes on to the window instead
([app.md](app.md#keys)): `Registry::run` returns without running it, and
`run_with` returns `CommandError::NotNow`. A command given arguments runs
whatever is selected, as they say what it works on. What's selected depends
on the focus ([below](#the-editors-state)): the edit commands' conditions
mean the piano roll's notes while it has the focus, the Sample Editor's
warp markers while it has it, and nothing while the devices panel or the
browser has it.

### Transport

The default keymap follows Ableton Live's shortcuts where Live has one.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `transport.play_stop` | Play/Stop | Plays from the start marker, or stops if playing | Space | Space |
| `transport.play` | Play | Plays from the playhead: where playing stopped | Shift+Space | Shift+Space (continue from the stop point) |
| `transport.stop` | Stop | Stops, leaving the playhead where it is | Ctrl+Period | none |
| `transport.record` | Record | Records the armed tracks, playing from the start marker as Play/Stop does; while playing, turns recording on or off | F9 | F9 (Arrangement Record) |
| `transport.capture` | Capture | Makes what the armed tracks, and those monitoring In, played over the transport's last run into takes, as if they had recorded it ([app.md](app.md#capture)) | Ctrl+Shift+C | Ctrl+Shift+C (Capture MIDI) |
| `transport.go_to_start` | Go to Start | Moves the start marker and the playhead to the start of the song | Home | Home |
| `transport.toggle_loop` | Toggle Loop | Loops the selection, or else turns looping over the loop region on or off | Ctrl+L | Ctrl+L |
| `transport.toggle_metronome` | Toggle Metronome | Turns the metronome on or off | O | O |
| `transport.tempo_up` | Tempo Up | Raises the tempo by 1 BPM | Ctrl+Shift+Up | none |
| `transport.tempo_down` | Tempo Down | Lowers the tempo by 1 BPM | Ctrl+Shift+Down | none |

These take arguments, for the ruler's gestures and the tempo field:

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `transport.locate` | Locate | `at` | Moves the start marker and the playhead there; playing goes on from there |
| `transport.set_start_marker` | Set Start Marker | `at` | Moves the start marker, and the playhead with it while stopped |
| `transport.set_loop` | Set Loop | `start`, `end`, `looping`? | Sets the loop region, and turns looping on or off |
| `transport.set_tempo` | Set Tempo | `bpm`, `at`? | Sets the tempo at `at`, or the playhead, moving every other tempo by as much |

Space plays from the start marker, as in Live, so playing again starts
where the last play did. The `play_from` setting
([Settings](../manual/src/customising/settings.md#behaviour) in the manual)
has it play from wherever the playhead stopped instead. Live has no keys
for stop and tempo; the ones chosen do nothing in Live outside its MIDI
note editor.

Stopping stops recording too. `transport.toggle_punch_in` and
`transport.toggle_punch_out` (Toggle Punch In, Toggle Punch Out) have
recording start at the loop region's start and stop at its end, as Live's
punch-in and punch-out switches do. They change the project, so undo
covers them and they're saved with it, and they have no keys, as in Live.

### File

A file command can't show a dialog or close the window itself, so it pushes
a `Request` into its `Context`, and the app carries it out once the command
has run ([app.md](app.md#files)).

| Command | Title | What it does | Default key |
|---|---|---|---|
| `file.new` | New Project | Starts a new, empty project | Ctrl+N |
| `file.new_demo` | New Demo Project | Starts the Tier 0 demo project | none |
| `file.open` | Open Project... | Asks for a project file and opens it; `file`? opens that one | Ctrl+O |
| `file.save` | Save Project | Saves the project, asking where if it has no file yet | Ctrl+S |
| `file.save_as` | Save Project As... | Asks where to save the project, and saves it there; `file`? saves it to that one | Ctrl+Shift+S |
| `file.import_audio` | Import Audio Files... | Asks for audio files, and puts them on the selected track; `files`?, `track`? and `at`? say which and where | none |
| `file.export_audio` | Export Audio... | Shows the export window, which renders the song to audio files ([app.md](app.md#exporting-audio)); `file`? renders the mix to it instead, with the window's last settings and the format its extension says | Ctrl+Shift+R |
| `file.quit` | Quit | Closes Musicable | Ctrl+Q |

New, New Demo, Open and Quit ask about unsaved changes first. Each file
command runs alone, never in a batch. Their files are `Param::asked`, so
agents give them, and the app skips the dialog
([agents.md](agents.md#what-agents-can-run)).

### Edit

The edit commands work on the selection where the focus is
([below](#the-editors-state)). While the arrangement has it, that's the
selected clips, or else the time selection, or else the selected tracks.
While the piano roll has it, Cut, Copy, Paste, Duplicate, Delete and Select
All act on its notes instead, as they do in Live's MIDI note editor: Paste
puts the notes at the start marker, in the clip the piano roll shows, and
Duplicate puts a copy just after the selected notes. While the Sample
Editor has it, Delete and Select All act on its warp markers, and Cut,
Copy, Paste and Duplicate don't apply. While the devices panel has it,
Cut, Copy, Paste, Duplicate, Delete and Select All act on its selected
devices ([Device](#device)). Split always acts on clips, and applies only
while the arrangement or the mixer has the focus. The mixer shows the
arrangement's tracks, so while it has the focus they act on the
arrangement's selection, as Live's mixer does in its Arrangement View. The
browser, the History panel, the Routing panel and the Performance panel
have nothing they take, so there they don't apply. Quantize moves the piano
roll's notes while it has the focus, the Sample Editor's transients while
it has it ([Transients](#transients)), and else those of the selected
clips, MIDI and warped audio, at the zoom of the panel they're in.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `edit.undo` | Undo | Takes back the last change to the project | Ctrl+Z | Ctrl+Z |
| `edit.redo` | Redo | Makes the last change undo took back again | Ctrl+Y, Ctrl+Shift+Z | Ctrl+Y |
| `edit.cut` | Cut | Copies the selection, then deletes it | Ctrl+X | Ctrl+X |
| `edit.copy` | Copy | Copies the selected clips, or the parts of clips in the time selection | Ctrl+C | Ctrl+C |
| `edit.paste` | Paste | Puts what was copied at the start marker, from the selected track down | Ctrl+V | Ctrl+V |
| `edit.duplicate` | Duplicate | Puts a copy of the selection just after it, and selects the copy | Ctrl+D | Ctrl+D |
| `edit.delete` | Delete | Deletes the selected clips, or what the time selection covers, or the selected tracks | Delete, Backspace | Delete |
| `edit.split` | Split | Splits the clips at the time selection's edges, or the selected clips at the start marker | Ctrl+E | Ctrl+E |
| `edit.select_all` | Select All | Selects every clip, or every note of the piano roll's clip, or every warp marker of the Sample Editor's, or every device the devices panel shows | Ctrl+A | Ctrl+A |
| `edit.select_master` | Select Master Track | Makes the master track the selected track, with nothing else selected, so the devices panel shows the master's devices, as clicking its header or its mixer strip does | none | Clicking the Main track |
| `edit.rename` | Rename | Asks for a new name for the selected clip, or the selected track; `clip`, `track`, `locator`, `take` or `modulator` names something else, and `control` with `modulator` one of a Macro's knobs or buttons | Ctrl+R | Ctrl+R |
| `edit.quantize` | Quantize | Moves the selected notes' starts to the nearest line of the grid, or all the clip's notes if none are selected; the transients in the Sample Editor's time selection, or all those its clip plays, giving each a warp marker; or the notes and transients of the selected clips. `notes`, `frames` and `clip` name what it moves, `amount` moves them part of the way, and `ends` moves notes' ends too. It was `notes.quantize`, which keymap files can still name | Ctrl+U | Ctrl+U |

And for the arrangement's clicks, scripts and agents:

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `edit.select_clips` | Select Clips | `clips`, `toggle`? | Selects the clips, or adds them to the selection or takes them away |
| `edit.select_time` | Select Time | `start`, `end`, `tracks`, `master`? | Selects a stretch of time on some tracks, and on the master too with `master`; `tracks` can then be empty |
| `edit.select_tracks` | Select Tracks | `tracks`, `toggle`? | Selects tracks |

These give the arrangement the focus, unless the mixer has it, which edits
the same selection. Selecting clips also shows the last
MIDI clip selected in the piano roll, and the last audio clip in the Sample
Editor, without giving them the focus, as selecting a clip in Live shows it
in the clip view. The clip editor keys then show the editor that fits the
last clip selected ([below](#options-and-view)).

The Edit menu names the change, as in "Undo Tempo Up". Ctrl+Shift+Z is
the other common key for redo. Clips that land where others are, by
pasting, moving or duplicating, overwrite what's there, as in Live: it's
trimmed back, split around them, or removed. Cut, Copy and Paste keep the
clips in the editor's clipboard, not the system's.

### History

The History panel ([app.md](app.md#the-history-panel)) goes to the step
clicked through this command, and scripts and agents can too:

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `history.go_to` | Go to Step | `step` | Undoes or redoes up to a step of the undo history: `step` counts from 1, the oldest step the history keeps, and it and those before it are left done and those after it undone; 0 undoes every step |

Going to a step undoes or redoes the steps in between one at a time, as
that many presses of Undo or Redo would, so the engine follows each. A
step the history doesn't have is an error that changes nothing.

### Clip

These change the clips their arguments name, for the arrangement's
gestures, scripts and agents. Splitting and trimming keep a clip's content
where it plays in the song, so an audio clip split in two plays exactly as
before.

| Command | Title | Arguments | What it does | Default key |
|---|---|---|---|---|
| `clip.move` | Move Clips | `clips`, `by`?, `tracks`?, `copy`? | Moves the clips, or copies them, some beats later or earlier and some tracks down or up; gives back the ids of the clips moved or the copies | none |
| `clip.trim` | Trim Clip | `clip`, `start`?, `end`? | Moves a clip's start or end. An audio clip doesn't go past its file's start or end | none |
| `clip.set_fades` | Set Fades | `clip`, `fade_in`?, `fade_out`? | Sets an audio clip's fades, which fit inside it | none |
| `clip.rename` | Rename Clip | `clip`, `name` | Names a clip | none |
| `clip.split` | Split Clips | `clips`, `at` | Splits the clips at a position | none |
| `clip.delete` | Delete Clips | `clips` | Deletes clips | none |
| `clip.insert_midi_clip` | Insert MIDI Clip | `track`?, `start`?, `end`? | Adds an empty MIDI clip: over the time selection on its MIDI tracks, or else a bar from the start marker on the selected track | Ctrl+Shift+M |
| `clip.insert_audio` | Insert Audio | `files`, `track`?, `at`? | Adds clips that play whole audio files, one after another, on the selected audio track or a new one, from the time selection's start or the start marker | none |

When the clips can't all land on tracks that hold them, such as an audio
clip on a MIDI track, they keep their tracks and only move in time. Moving
clips before the start of the song stops at the start. `arrange::trimmed`
says where a trim would leave a clip, which the arrangement's ghost shows
while a drag lasts. Insert Audio needs each file's header read into
the editor first ([below](#the-editors-state)): the app reads it when a file
is dropped or imported. A warped audio clip's offset is in beats of its own
time ([project-format.md](project-format.md#track)), so trimming, splitting
and moving it keep its audio on the beats its warp markers put it on, at
any tempo; an unwarped clip's offset is a frame of its file.

### Notes

These edit the notes of a MIDI clip, for the piano roll's gestures,
scripts and agents. Each takes the clip as `clip`, or edits the one the
piano roll shows without it, and names notes by their ids in the clip.
Positions are in the clip's own time, as the project file has them
([project-format.md](project-format.md#track)): a note at the clip's offset
plays at the clip's start. Each change is one transaction, so a drag is one
step to undo, and each selects the notes it leaves, as in Live.

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `notes.select` | Select Notes | `notes`, `clip`?, `toggle`? | Selects the notes, or adds them to the selection or takes them away, and gives the piano roll the focus |
| `notes.add` | Add Note | `start`, `key`, `length`?, `velocity`?, `clip`? | Adds a note, a step of the piano roll's grid long and at velocity 100 without them; gives back its id |
| `notes.draw` | Draw Notes | `starts`, `keys`, `length`?, `velocity`?, `clip`? | Adds a note at each start, on its key, as Draw Mode's drags do; gives back their ids |
| `notes.delete` | Delete Notes | `notes`, `clip`? | Deletes notes |
| `notes.move` | Move Notes | `notes`, `by`?, `keys`?, `copy`?, `clip`? | Moves notes, or copies them, some beats later or earlier and some semitones up or down; gives back the ids of the notes moved or the copies |
| `notes.trim` | Trim Notes | `notes`, `start_by`?, `end_by`?, `clip`? | Moves notes' starts, keeping their ends, or their ends |
| `notes.set_velocity` | Set Velocity | `notes`, `velocity`?, `by`?, `clip`? | Sets how hard notes start, from 0 to 1, or changes it by `by` |
| `notes.open` | Open in Piano Roll | `clip` | Shows a MIDI clip in the piano roll, gives it the focus, and shows the piano roll |

Notes keep their places relative to each other: a move stops at the song's
start and at the lowest and the highest key, and a note never gets shorter
than a tick. Two notes on one key can overlap, and play as two notes.

And these act on the notes selected in the piano roll while it has the
focus, with the keys of Live's MIDI note editor. The arrow keys are the
piano roll's own ([below](#the-keymap-file)): they run these while it has
the focus, and nothing in the other panels.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `notes.transpose_up` | Transpose Up | Moves them a semitone up | Up | Up |
| `notes.transpose_down` | Transpose Down | A semitone down | Down | Down |
| `notes.octave_up` | Octave Up | An octave up | Shift+Up | Shift+Up |
| `notes.octave_down` | Octave Down | An octave down | Shift+Down | Shift+Down |
| `notes.nudge_left` | Nudge Notes Left | A step of the grid earlier | Left | Left |
| `notes.nudge_right` | Nudge Notes Right | A step of the grid later | Right | Right |
| `notes.lengthen` | Lengthen Notes | A step of the grid longer | Shift+Right | Shift+Right |
| `notes.shorten` | Shorten Notes | A step of the grid shorter, unless that's all they have | Shift+Left | Shift+Left |
| `notes.velocity_up` | Velocity Up | Starts them harder, by 10 of 127 | Ctrl+Up | Ctrl+Up |
| `notes.velocity_down` | Velocity Down | Starts them softer | Ctrl+Down | Ctrl+Down |
| `notes.toggle_draw` | Draw Mode | Turns the piano roll's Draw Mode on or off | B | B |

The grid's step is the one the piano roll shows at its zoom. With no notes
selected these don't apply, and the arrow keys run nothing in the other
panels, so there the keys go on to move the keyboard focus between controls
([app.md](app.md#keys)).

### Controllers

These choose what the lane under the piano roll's notes shows, and draw a
controller's curve in a MIDI clip, such as the mod wheel's or the sustain
pedal's ([project-format.md](project-format.md#track)). Like the
note commands, they take the clip as `clip`, or edit the one the piano roll
shows, and place points in the clip's own time. They take the controller as
`controller`, its MIDI CC number from 0 to 119, or else the one the lane
shows. A controller's values go from 0 to 1, a MIDI value over 127. Each
change is one transaction.

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `controllers.show` | Show Controller | `controller` | Shows a controller in the lane |
| `controllers.show_velocities` | Show Velocities | | Shows the notes' velocities in the lane, as it does at first |
| `controllers.draw` | Draw Controller | `start`, `end`, `at`, `values`, `controller`?, `clip`? | Draws over the stretch from `start` to `end`: a point at each of `at`, with the value in `values` in the same order, and shows the controller in the lane |
| `controllers.erase` | Erase Controller | `start`, `end`, `controller`?, `clip`? | Erases the stretch, so what came before holds through it |
| `controllers.clear` | Clear Controller | `controller`?, `clip`? | Takes away the whole curve, so the clip leaves the controller alone; it applies when the lane shows a controller the clip moves |

Drawing and erasing keep the rest of the curve as it was: from the
stretch's end on, it holds what it did, with a point there if it needs one
(`clip::redraw`). A drawn point that holds what came before is left out.
Show Velocities and Clear Controller are in the Notes menu, and the lane's
context menu has Clear Controller.

### Warping

These warp an audio clip, so it follows the tempo, edit its warp markers
and set its gain, for the Sample Editor's gestures, scripts and agents
(`crates/commands/src/warp.rs`). Each takes the clip as `clip`, or edits
the one the Sample Editor shows without it. A marker pins a frame of the
file to a position in the clip's own time, as the project file has them
([project-format.md](project-format.md#track)): no two markers share a
position, so the commands name markers by theirs. Each change is one
transaction, so a drag is one step to undo.

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `clip.open` | Open in Sample Editor | `clip` | Shows an audio clip in the Sample Editor, gives it the focus, and shows the Sample Editor |
| `clip.set_warp` | Warp | `on`?, `clip`? | Turns the clip's Warp on or off, or the other way from now without `on` |
| `clip.set_gain` | Set Clip Gain | `gain`, `clip`? | Sets the clip's gain, linear, from 0 to about 15.85 (+24 dB), where its gain curve doesn't say otherwise |
| `warp.select_markers` | Select Warp Markers | `markers`, `clip`?, `toggle`? | Selects the markers, or adds them to the selection or takes them away, and gives the Sample Editor the focus; selecting just them takes away the time selected |
| `warp.select_time` | Select Time in Sample Editor | `start`, `end`, `clip`? | Selects a stretch of the clip's own time, with the markers in it, as a drag across the waveform does; its transients are the selected ones. At the start, it selects nothing |
| `warp.insert_markers` | Insert Warp Marker | `at`?, `frames`?, `clip`? | Adds a marker at each position, pinning the audio that plays there, and at each of the file's frames, pinning it where it plays, or without them at each transient in the time selected, or else at the start marker, and selects them; gives back where they went |
| `warp.move_markers` | Move Warp Markers | `markers`, `by`, `clip`? | Moves markers some beats later or earlier, stretching the audio either side, as far as they can go before the markers either side or a tempo out of range; gives back how far they went |
| `warp.move_audio` | Move Audio Under Warp Marker | `marker`, `frame`, `clip`? | Has the file's frame `frame` play at the marker, which stays where it is; the frame must be between those of the markers either side |
| `warp.delete_markers` | Delete Warp Markers | `markers`?, `clip`? | Deletes the markers, or the selected ones without them; a warped clip keeps its first if they're all of them |
| `warp.set_tempo` | Set Clip Tempo | `bpm`, `keep_end`?, `clip`? | Sets the clip's tempo, from 5 to 999 BPM: its markers spread out or close up from its start, and its start stays where it is, or its end with `keep_end`, as the arrangement's Shift-drag of its start does |

Warp keeps the stretch of the file a clip plays, as Live's does, so
turning it on or off, or changing the clip's tempo, changes the clip's
length, and what the clip comes to cover on its track is overwritten, as
trimming does. Turning Warp on warps the clip as auto-warp's rule for its
file says, where analysis is sure of the file and the rule is on
([Auto-warp](#auto-warp)), and else puts one marker at the clip's start, at
the tempo `auto_warp::tempo_guess` gives: the one analysis found, or the
file's loop data's (`AudioFile::tempo`) or name's where that's near it at
any octave, or either alone before the file's analysed, or else the song's
where the clip starts, read at the octave nearest the song's. The Sample
Editor shows that tempo, greyed out, while the clip is unwarped. Turning
Warp off forgets the markers, the mode and its settings, and turning it on
starts in Re-Pitch with Live's settings. The clip's curves stay with its
audio both ways. A change
that would take the clip's tempo out of range, bring two markers onto one
tick or, with `keep_end`, put the clip's start before the song's is an
error that changes nothing.

And these act on the clip the Sample Editor shows, and its markers
selected while it has the focus, with the keys of Live's Sample Editor.
The Sample Editor's keys are its own ([below](#the-keymap-file)): they run
these while it has the focus. The Clip menu has them too.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `warp.insert_markers` | Insert Warp Marker | Puts a marker at each transient in the time selected, or else at the start marker | Ctrl+I | Ctrl+I |
| `warp.nudge_left` | Nudge Warp Markers Left | Moves the selected markers a step of the grid earlier | Left | Left |
| `warp.nudge_right` | Nudge Warp Markers Right | A step of the grid later | Right | Right |
| `warp.double_tempo` | Double Clip Tempo | Doubles the clip's tempo, so its audio plays half as fast, over twice the beats | none | ×2 |
| `warp.halve_tempo` | Halve Clip Tempo | Halves it, so its audio plays twice as fast | none | ÷2 |

The grid's step is the one the Sample Editor shows where the first
selected marker is, at its zoom. Delete and Select All delete and select
its markers while it has the focus ([Edit](#edit)), and Select All selects
all the clip's time too.

And these are the Sample Editor's right-click menu's, as Live's, which warp
a clip again from a point (`crates/commands/src/rewarp.rs`). The point is a
frame of the clip's file, as the menu finds it: a marker's, a transient's,
or the one that plays where it was opened. Each keeps the clip's curves
with its audio, overwrites what the clip comes to cover, and checks
everything before it changes anything, so an error changes nothing.

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `warp.set_downbeat` | Set 1.1.1 Here | `frame`, `clip`? | Makes the point the clip's 1.1.1: its markers' beats count from there, and the clip moves, and its offset with it, so the point plays on the bar line nearest where it played, cut at the song's start |
| `warp.from_here` | Warp From Here | `frame`, `clip`? | Keeps the markers before the point, adding one there, and warps the rest by analysis, as auto-warp's rule for the file would: straight at the file's tempo for a loop or a steady file, read at the octave nearest the clip's tempo there, and a marker on each downbeat and each beat that strays for a file that drifts. An error before the file's analysed, or for one without a pulse |
| `warp.from_here_at_song_tempo` | Warp From Here (Start at Song Tempo) | `frame`, `clip`? | The same, reading the file's tempo at the octave nearest the song's |
| `warp.from_here_straight` | Warp From Here (Straight) | `frame`, `clip`? | Plays the rest at one tempo: the file's, read at the octave nearest the clip's there, or the clip's there before the file's analysed |
| `warp.song_tempo_from_here` | Warp at Song Tempo From Here | `frame`, `clip`? | Plays the rest at the song's tempo where the point plays |
| `warp.selection_as_loop` | Warp Selection as a Loop | `start`?, `end`?, `clip`? | Makes the clip the stretch of its own time from `start` to `end`, or the time selected without them, fitted into the nearest whole number of bars from the bar line nearest where it starts: one marker at its start, at the tempo that fits it. It selects the loop. The Clip menu has it, while time is selected |

Warp From Here and its variants keep the stretch of the file the clip
plays, so its end moves. With one marker left, they set the warp's tempo;
with more, they add a marker where the file ends, or a beat after the last
one if the file's length isn't known.

### Warp modes

These set a warped clip's mode, and the settings its modes play it with,
for the Sample Editor's controls, scripts and agents
(`crates/commands/src/warp_mode.rs`). Like the warping commands, each
takes the clip as `clip`, or the one the Sample Editor shows without it,
and an unwarped clip is an error but for Show Clip Lane. A clip keeps each
mode's settings while another plays, as Live does, and each change is one
transaction; the value a setting has already is none.

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `warp.set_mode` | Set Warp Mode | `mode`, `clip`? | Sets how the audio between the markers plays: `re_pitch`, resampled so its pitch follows its speed, `slice`, cut into slices, each played at the file's own speed, `granular`, played as short overlapping grains, each at the file's own speed, or `spectral`, stretched in the frequency domain, keeping its pitch |
| `warp.set_transpose` | Set Clip Transpose | `semitones`, `clip`? | Sets how far every mode but Re-Pitch moves the audio's pitch, from -48 to 48 semitones |
| `warp.set_detune` | Set Clip Detune | `cents`, `clip`? | And by cents, from -50 to 50 |
| `warp.set_preserve` | Set Slice Preserve | `preserve`, `clip`? | Sets where Slice cuts the audio: `transients`, or on a grid of `bar`, `1/2`, `1/4`, `1/8`, `1/16` or `1/32` from the clip's 1.1.1 |
| `warp.set_transient_loop` | Set Transient Loop Mode | `mode`, `clip`? | Sets what a slice plays once its audio runs out before the next starts: `off`, `forward` or `back_and_forth` |
| `warp.set_transient_envelope` | Set Transient Envelope | `envelope`, `clip`? | Sets how much each slice fades as it plays, from 0, a very quick decay, to 100, no fade |
| `warp.set_follow_pitch` | Set Follow Pitch | `on`, `clip`? | Sets whether Granular fits its grains to the audio's pitch, where analysis found one, as Live's Tones does, or cuts them on a steady grid, as Live's Texture does |
| `warp.set_grain_size` | Set Grain Size | `milliseconds`, `clip`? | Sets how long each of Granular's grains is, from 5 to 500 ms |
| `warp.set_fluctuation` | Set Fluctuation | `fluctuation`, `clip`? | Sets, while Follow pitch is off, how far each grain strays from where and when it would play, as a share of its length, from 0 to 100 |
| `warp.set_formants` | Set Formants | `formants`, `clip`? | Sets how far Spectral keeps the formants in place as it transposes, from 0, moving with the pitch, to 100, staying where they were |
| `warp.set_high_quality` | Set High Quality | `on`, `clip`? | Sets whether Spectral renders with Rubber Band's R3 engine, the best quality and slower to render, rather than Signalsmith Stretch |
| `warp.set_envelope` | Set Spectral Envelope | `envelope`, `clip`? | Sets, while High quality is on, how fine the outline of the spectrum is that Spectral keeps the formants by, from 8 to 512 |
| `clip.show_lane` | Show Clip Lane | `param`, `clip`? | Shows the clip in the Sample Editor, with the curve of its parameter `param` in the lane: `gain`, `transpose`, `detune`, `transient_envelope`, `preserve`, `transient_loop`, `grain_size`, `fluctuation`, `formants` or `envelope`. It isn't a step to undo |

Each of these settings but the mode, Follow pitch and High quality is one
of the clip's parameters too (`ClipParam`, with Gain), so the automation commands give it
a curve in the clip ([Automation](#automation)), and setting its value
(`Transaction::set_param_value`) sets the warp's setting, kept in range and
to whole semitones, cents and steps (`Warp::set_param`). The parameters
play in the modes `ClipParam::plays_in` says, and the clip keeps the others'
curves for the modes that play them.

### Transients

A warped clip keeps its file's transients, where the sounds in it start,
as analysis found them, with the user's changes
([project-format.md](project-format.md#track)). These change them, for
the Sample Editor's gestures, scripts and agents
(`crates/commands/src/transient.rs`). Like the warping commands, each takes
the clip as `clip`, or the one the Sample Editor shows without it, and
names transients by their frames in the file. A clip whose file hasn't been
analysed yet has none to change, which is an error.

| Command | Title | Arguments | What it does | Default key |
|---|---|---|---|---|
| `transient.insert` | Insert Transient | `frames`?, `clip`? | Adds a transient at each frame, or at the start marker without them; gives back the frames it added at | Ctrl+Shift+I |
| `transient.move` | Move Transient | `frame`, `to`, `clip`? | Moves the transient at `frame` to `to`, between the transients either side | none |
| `transient.delete` | Delete Transients | `frames`?, `clip`? | Deletes the transients at the frames, or those in the time selected without them; gives back how many | Ctrl+Shift+Backspace |
| `transient.reset` | Reset Transients | `clip`? | Goes back to the transients analysis found, without the user's changes | none |

Deleting a transient analysis found hides it, and so does moving it, so
Reset Transients can bring it back even when the file's analysis is gone.
The keys are Live's, and the Sample Editor's own, and the Clip menu has
these but Move Transient.

Quantize ([Edit](#edit)) moves transients towards the grid
(`transient::quantized`): each gets a warp marker where it plays, which
changes nothing, or keeps the one on it, and then the markers move, the
transient nearest its line first. A marker in a transient's way that isn't
on a transient goes; one on a transient stops it, as does a line another
took, and the transient stays where it plays. With an `amount` under 1
nothing can pass anything, so only a whole move can be stopped. The clip's
timing stays, and so do its curves, in its own time, as when markers move. `warp.take_transients` isn't a command: it gives
the warped clips that haven't any yet the transients analysis has found, as
a change the project follows rather than a step to undo, and the app calls
it as analyses arrive and the project changes.

### Auto-warp

Auto-warp isn't a command: it's how Insert Audio, which drops and imports
run, warps the files it puts in, by what analysis found in them
(`crates/commands/src/auto_warp.rs`). `auto_warp::kind` sorts a file into a
loop of whole bars, a steady file, a drifting one or a sound without a
pulse, or nothing where analysis is unsure of it: where the classic
tracker's beats agree with the network's less than 0.8 as an F-measure, or
the beat slips. Each kind has a rule, and a switch in `Behaviour`, which
Settings keeps per profile ([app.md](app.md#settings-and-your-keymap)):

| Kind | Switch | What it gets |
|---|---|---|
| Loop | `warp_loops` | One marker at its start, at the tempo its length gives for its bars |
| Steady | `warp_steady` | One marker on its first downbeat, at its tempo, the downbeat on the bar line nearest where it was dropped, cutting a pickup of under 50 ms at the song's start |
| Drifting | `warp_drifting` | A marker on each downbeat, and on each beat that those would play more than 10 ms from where it is |
| No pulse | `sync_one_shots` | No warp: its sync point, the first strong attack or a riser's peak, plays where it was dropped |

A file's tempo comes from analysis, or from its loop data or name where
that keeps within 10 ms of analysis's beats across the file, and is read at
the octave nearest the song's (`least_stretch`). With
`tempo_from_first_file`, the first file with a pulse dropped into a song
with no clips gives it its tempo and meter. `warp_recordings` is for takes
([Take](#take)).

Insert Audio warps the files whose analyses the editor has as it puts them
in. For the others, it leaves a `Pending` in the editor, with its step in
the undo history, and the app calls `auto_warp::apply_pending` as analyses
land (`Session::auto_warp`), once none of a drop's files is waiting. That
joins the drop's step (`Document::amend`) while it's the last, and else is
a step of its own, Auto-Warp, which leaves other clips alone. A clip
changed since the drop stays as it is.

### Locator

| Command | Title | Arguments | What it does | Default key |
|---|---|---|---|---|
| `locator.add` | Add Locator | `at`?, `name`? | Adds a locator at the playhead while playing, or else at the start marker, as Live's Set button does | none |
| `locator.previous` | Previous Locator | | Jumps to the locator before the playhead, or before the start marker while stopped | none |
| `locator.next` | Next Locator | | Jumps to the locator after it | none |
| `locator.move` | Move Locator | `locator`, `at` | Moves a locator | none |
| `locator.rename` | Rename Locator | `locator`, `name` | Names a locator | none |
| `locator.delete` | Delete Locator | `locator` | Deletes a locator | none |

A new locator is named with a number, counting the locators. Live has no
keys for these either.

### Take

Recording keeps what each pass of the playhead recorded as a take, in a
lane under its track: a loop recorded four times makes four takes. Each
pass is comped onto its track as it's added, later passes over earlier
ones, so the track plays the last pass, and comping picks other parts from
there.

| Command | Title | Arguments | What it does | Default key |
|---|---|---|---|---|
| `take.comp` | Comp | `take`, `start`, `end` | Copies a stretch of a take to its track, replacing what the track has there, even where the take has nothing; gives back the copies' ids | none |
| `take.rename` | Rename Take | `take`, `name` | Names a take | none |
| `take.delete` | Delete Take | `take` | Deletes a take | none |

`take::add_recording` adds what the tracks recorded as one transaction,
"Record", with a take for each pass, named "Take" and a number one past
the track's highest, so one undo takes a whole recording back. A pass
starts on the frame it was recorded at, whatever the tempo, and one that
went on in another file, at WAV's 4 GB, is a take with a clip for each
file. With `Behaviour::warp_recordings`, as it is unless it's switched off,
an audio pass follows the tempo: its clip is warped, its own time the
song's, with a marker where it starts, and where it spans tempo changes, at
each of them and where it ends, so it plays sample for sample as it was
played, and stretches with the tempo if that changes later. A MIDI track's pass (`Recorded::Midi`) is a take with a MIDI clip of
the notes played, each starting and ending on its frames, with its
expression, and of how the controllers moved, as curves that start with
what each held as the pass began; a pass without notes makes no take.
Comping copies
the take's clips, so the take keeps them. `take::comped` says where a
track plays a take's clip: where its own clips play the same audio or the
same notes from the same place in the song, as comping copies them, and
not a copy moved since or whose notes changed. `view.toggle_take_lanes`
(see below) shows the takes.

### Track

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `track.new_audio_track` | New Audio Track | Adds an audio track | Ctrl+T | Ctrl+T |
| `track.new_midi_track` | New MIDI Track | Adds a MIDI track | Ctrl+Shift+T | Ctrl+Shift+T |
| `track.new_return_track` | New Return Track | Adds a return track | Ctrl+Alt+T | Ctrl+Alt+T |
| `track.new_group_track` | New Group Track | Adds an empty group track | none | none |
| `track.delete` | Delete Tracks | Deletes the selected tracks, or the ones its `tracks` argument names | none | Delete, with tracks selected |
| `track.rename` | Rename Track | Names the track its `track` argument names, `name` | none | none |

A new track is named after its kind and how many of that kind there are,
such as "Audio 3". Return tracks go at the end of the list, and other tracks
before the first return track, so the returns stay together, as in Live.
Return tracks start solo-safe. Live's Ctrl+G groups the selected tracks,
which needs a selection, so New Group Track has no key yet.

### Device

Devices go on a track or on the master, in the order the signal goes
through them. The devices panel and the browser run these, with the
device's id as `device`, or a list of ids as `devices`, which they take in
the order the devices play.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `device.add` | Add Device | Adds one of Musicable's own devices, `kind` (`eq`, `compressor`, `gate`, `delay`, `reverb`, `utility`, `sampler` or `synth`), at `index` or the end of the devices of `track`, the selected track, or the master with `master`, or with `new_track` on a new track named after it, as `device.add_plugin` does, with every parameter at its default. Returns the new device | none | Double-click in the browser |
| `device.add_plugin` | Add Plugin | Adds the plugin `plugin` of the format `format` (`clap` or `vst3`), called `name`, at `index` or the end of the devices of `track`, the selected track, or the master with `master`, or with `new_track` on a new track named after it: a MIDI track for an instrument or a MIDI effect, an audio track for an audio effect. `kind` says what it does with notes: `instrument`, `midi_effect`, or `audio_effect`, as without it. A MIDI effect goes no later than just before the chain's instrument, and an instrument no earlier than just after its last MIDI effect, and a MIDI effect where there are no notes is refused. Returns the new device | none | Double-click in the browser |
| `device.add_preset` | Add Preset | Adds the device the preset file `preset` holds, set as it was saved, where `device.add_plugin` puts one. Returns the new device | none | Double-click in the browser |
| `device.save_preset` | Save Device as Preset | Saves the device `device` as a Musicable preset called `name`, or the device's name and a number, in the presets folder, and returns its path. A name the device has a preset for already is refused | none | Save Preset, on the device's title bar |
| `device.set_param` | Set Device Parameter | Sets the parameter `param` of the device `device` to `value`. A built-in device's parameter is its place in the kind's table, from 0, as [project-format.md](project-format.md) numbers them, and needs a value, which is kept in the parameter's range. A plugin's is the plugin's id for it, and without a value it's left to the plugin's state | none | none |
| `device.set_bypass` | Set Device Bypass | Bypasses the devices `devices`, or brings them back with `bypassed` false: each stops, and what comes into it plays on, as late as its latency would make it, so the mix keeps its timing. It overrides their Bypass lanes, as moving a control does | none | The device's power button |
| `device.set_active` | Set Device Active | Deactivates the devices `devices`, or activates them with `active` true. A deactivated device is out of the signal, latency and all, though it keeps its place and its settings, and a plugin stays loaded | none | none |
| `device.toggle_bypass` | Toggle Device Bypass | Bypasses the devices selected in the devices panel, or brings them back if every one is bypassed | 0 in the devices panel | none |
| `device.select` | Select Devices | Selects the devices `devices`, all of one chain, which the devices panel then shows, or adds them to the selection or takes those already in it away with `toggle`, and gives the panel the focus. With none, none are selected | none | Clicking a device's title bar |
| `device.move` | Move Devices | Moves the devices `devices` to `index` or the end of the devices of `track`, the selected track, or the master with `master`, or with `new_track` onto a new track named after the first: a MIDI track if one is an instrument or a MIDI effect, an audio track if none is. `index` counts the chain without them. They keep their ids, so their lanes and their plugins go with them. A move that leaves a MIDI effect after its chain's instrument, or where there are no notes, is refused | none | Dragging a device's title bar |
| `device.move_left` | Move Devices Left | Moves each run of the selected devices one place towards the start of the chain, past the device before it | Ctrl+Left in the devices panel | none |
| `device.move_right` | Move Devices Right | Moves each run of the selected devices one place towards the end of the chain, past the device after it | Ctrl+Right in the devices panel | none |
| `device.copy` | Copy Devices | Puts copies of the devices `devices` where `device.move` puts them, but with `index` counting the chain as it is. Each is a new device with its original's settings, a plugin's state taken afresh, and lanes of its own with the original's points, unless the Behaviour setting says not to. A copy of a MIDI effect goes no later than just before the chain's instrument, and isn't made where there are no notes. Returns the copies | none | Ctrl-dragging a device's title bar |
| `device.set_sample` | Set Sample | Gives the sampler `device` the audio file `file` to play, a path as clips name files ([project-format.md](project-format.md)), or takes its sample away without one | none | Drop a file on Simpler |
| `device.load_sample` | Load Sample... | Asks for an audio file and gives it to the sampler `device` with `device.set_sample` | none | none |
| `device.show_window` | Show Plugin Window | Shows the plugin's own window, or brings it to the front | none | Ctrl+Alt+P (all of them) |
| `device.hide_window` | Hide Plugin Window | Closes it | none | Ctrl+Alt+P |
| `device.delete` | Delete Device | Deletes a device | none | Delete, with the device selected |

Adding, changing and deleting are transactions named after the device:
a built-in device by its kind, such as "Add EQ" and "Change EQ", and a
plugin by its name, such as "Delete Surge XT"; adding a preset is named
after the preset. Bypassing is "Bypass EQ" or "Bring Back EQ", and
"Bypass 3 Devices" for several, and deactivating "Deactivate EQ" or
"Activate EQ". The devices panel's Bypass button runs them
([app.md](app.md#the-devices-panel)), and so does a plugin's own bypass
button, in its window ([plugins.md](plugins.md#plugins-on-tracks)). So undo covers them, and dragging a control is one
gesture, which one undo takes back. A device added goes in sight: the
track it goes on, or the master, becomes the selected track, as a click on
its header makes it, if it isn't already, and the app is asked, with
`Request::ShowDevice`, to bring the devices panel in front of the piano
roll and scroll it to the device. A preset of a VST3 plugin needs the
plugin installed, and is refused without it.

Moved and copied devices, side by side in the order they played, go in
sight the same way, selected. Moving is "Move EQ", or "Move 2 Devices" for
several, one step however many go: the `MoveDevice` edit, which keeps the
device's id, so undo puts it back where it was, its lanes and plugin with
it. Copying is "Copy EQ". Selecting changes nothing to undo, and showing
another chain in the devices panel forgets the devices selected. While the
devices panel has the focus, the edit commands act on its selected devices
([Edit](#edit)): Cut and Copy take them, as they are, with the lanes that
automate them, into the panel's own clipboard (`editor::DeviceClipboard`);
Paste puts copies in front of the first selected device, or at the end of
the chain the panel shows; Duplicate puts copies just after the last
selected one and selects them, so doing it again makes more; and Delete
deletes them. Each is a step named after the devices, such as "Paste EQ"
or "Duplicate 2 Devices". A copy's plugin state is taken afresh from the
plugin, as Save as Preset takes it (`Library::plugin_state`), since a
change made in its window can reach the project late. Copies take their
originals' automation, as Live's have since 10.0.1, with lanes of their
own, unless the Behaviour setting `copies_take_automation` is off. Giving a sampler its sample
is "Load Sample", and taking it away "Remove Sample". `device.load_sample`
asks the app, with `Request::LoadSample`, which shows a dialog in the
project's folder, and a sample dragged from the browser onto a sampler
runs `device.set_sample` too; the sampler reads the file when the graph is
next built, and the status bar says if it can't. The window commands ask the app, with
`Request::PluginWindow`; a plugin that isn't playing has no window to show,
and the status bar says why. A new device starts with the plugin's own
defaults. Its settings fill
in as the plugin changes them ([plugins.md](plugins.md#plugins-on-tracks)).

Deleting a track or a device deletes the wires to and from it, and sets
the tracks that take their input from it to No Input, as edits of their own
in the same step, so undo brings them back ([Wires](#wires),
[The mixer's controls](#the-mixers-controls)). Moving a device onto the
master sets the tracks that take what it puts out to No Input the same
way, as edits before the move's, since the master's one tap for an input
is its Post Fader, for Resampling. A
copy of a device keeps the wires into it, at their gains and with their
lanes when the copy takes its original's, so a copied compressor keeps its
key: from the same taps, or from the copy of a device copied with it. A
wire whose copy would close a loop, as into a copy put before the device
it's keyed after, is left out, and the wires from a device's tap don't
come with its copy. A move that would close a loop through a wire or a
track's input is refused, with an error naming it, and changes nothing. Moving
several devices is checked once they've all moved
(`Transaction::at_once`), as one move can close a loop that the next
opens again.

### Wires

A wire takes the signal at a tap, a point on a track or the master, into a
device's port, the Compressor's and the Gate's Sidechain or one of a
plugin's side inputs, at a gain, and wires into the same port sum
([project-format.md](project-format.md#wire)). A note wire takes a MIDI
track's notes into an instrument's or a MIDI effect's note input instead,
on a MIDI channel or on their own, which is how a track plays an
instrument on another. The devices panel's Sidechain column, a plugin's
rows for its side inputs, the mixer's MIDI To and the Routing panel run
these ([app.md](app.md#the-devices-panel)).

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `wire.connect` | Connect Wire | Wires a tap into a side input of `device`, at `gain` (linear, 1 without it): the Sidechain of a Compressor or a Gate, which it turns on, or a plugin's side input `input`, by the plugin's id for it, its first without it. With `notes`, it wires a MIDI track's notes into the note input of `device`, an instrument or a MIDI effect, on `channel`, from 0 to 15, or each note on its own without it. Returns the new wire | none | Audio From, in a compressor's Sidechain; MIDI To |
| `wire.disconnect` | Disconnect Wire | Removes the wire `wire`, with the lane on its gain and the routings to it | none | No Input |
| `wire.set_source` | Set Wire Source | Has the wire `wire` take its signal from another tap, keeping its gain, its lane and its place | none | Audio From |
| `wire.set_gain` | Set Wire Gain | Sets the wire's `gain`, linear, from silence to about 15.85, +24 dB. It overrides the lane on it, as moving a control does | none | The sidechain's Gain |
| `wire.set_channel` | Set Wire Channel | Sets the MIDI channel the note wire `wire` plays every note on, from 0 to 15, or each note on its own without `channel` | none | MIDI To's channel |

A tap is `after`, a device, for what that device puts out, on any track;
or `track` or `master` with `tap`: `pre_fx` for what comes into a track's
devices, `post_fx` (without `tap`) for what comes out of the last of them,
and `post_fader` for what comes out of the fader and pan. The master has
no Pre FX, and a MIDI track's is notes, and so is the point after each of
its MIDI effects in front of its instrument, which only a note wire takes,
as does a MIDI track's Post FX, for the notes that reach its instrument.
The steps are "Connect Wire", "Disconnect Wire", "Wire Source", "Wire
Gain" and "Wire Channel", and an audio wire's gain is a parameter like any
other, which scripts write as "wire 1" ([Automation](#automation)). A note
wire has none, and an audio wire no channel, so `gain` with `notes`,
`channel` without it, `wire.set_gain` on a note wire and `wire.set_channel`
on an audio wire are refused. Modulators move it as they move a
send's level, as far as the wire's own range, and a modulator reaches it
where it reaches the device the wire feeds ([Modulation](#modulation)).

A wire is refused, with an error that says why, when the project hasn't
got the tap or the port, a plugin's side input among them if its device
doesn't list it (`PluginDevice::inputs`, which the app keeps as the plugin
lists its inputs), when the tap is wired to the port already, and
when it would make a loop: when what the device puts out reaches the tap,
as it does a group its track feeds, the master, or a point after itself,
such as "That would make a loop: what the Compressor on Bass puts out
reaches Master Post FX". The check follows the signal point to point
(`Project::check_wire`), so two tracks can key each other from their Pre
FX. Moving a device, an output or a send can close a loop through a wire
too, or through a track's input from a tap, and a `Transaction` refuses one
that does, with the `Route` in the way: `move_device`, `set_track_output`,
`add_send` and `set_track_input` give back a `Loop`, which `wire::refused`
makes the command's error, such as "That would make a loop, through the
wire from Kick Post FX to the Compressor on Bass" or "That would make a
loop, through Print's input from Kick Post FX".
Undo and redo go back to how the project was, so `Transaction::apply`
never refuses.

### Modulation

Modulators move parameters as the song plays without changing their
values ([project-format.md](project-format.md#modulation)): a Macro's
knobs and buttons, played by hand, an LFO, a shape that repeats, and an
Envelope, a shape that runs once each time it's started. Each track has a
strip of them, and so does the master, and a routing from a modulator's
output to a parameter says how far it moves it. A modulator reaches the
parameters of its own channel, a group's those of the tracks that feed it
too, and the master's every track's. The commands name a modulator by its
id as `modulator`, a routing by its id as `routing`, and a Macro's knob or
button by its number as `control`.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `modulation.add` | Add Modulator | Adds a modulator of `kind` (`macro`, `lfo` or `envelope`) at `index` or the end of the strip of `track`, the selected track, or the master with `master`, called `name`, or its kind's name and the first number free on the strip, such as "LFO 2", with every parameter at its default and a Macro with one knob. Returns the new modulator | none | none |
| `modulation.delete` | Delete Modulator | Deletes the modulator `modulator`, with its routings, the routings to its parameters and its lanes | none | none |
| `modulation.move` | Move Modulator | Moves the modulator `modulator` to `index` in its strip, counting the strip without it | none | none |
| `modulation.rename` | Rename Modulator | Names the modulator `modulator`, `name` | none | none |
| `modulation.set_param` | Set Modulator Parameter | Sets parameter `param` of the modulator `modulator`, its number in an LFO's or an Envelope's table, from 0, as [project-format.md](project-format.md#modulation) numbers them, or a Macro's knob or button, to `value`, kept in its range and to its steps. It overrides the parameter's lane, as moving a control does | none | none |
| `modulation.add_control` | Add Macro Control | Adds a control of `kind`, `knob`, `toggle` (a button each press turns on or off) or `momentary` (one that's on while it's held), a knob without it, to the Macro `modulator`, at `index` or the end, called `name`, or "Knob" or "Button" and the first number free on the Macro. Returns its number | none | none |
| `modulation.delete_control` | Delete Macro Control | Deletes the knob or button `control` of the Macro `modulator`, with the routings from it and to it and its lane | none | none |
| `modulation.rename_control` | Rename Macro Control | Names it `name` | none | none |
| `modulation.add_routing` | Add Routing | Has output `output` of the modulator `modulator`, its first without it, move the parameter `param` by `amount`, straight, with no scale and no lag. The parameter must be one modulators can move, in the modulator's reach, and not one that output moves already. The amount is a share of the parameter's range, from -1 to 1, along its control, so 0.25 moves a cutoff a quarter of its octaves up while the output is at 1, or for a plugin's parameter in the plugin's own units. Returns the routing | none | none |
| `modulation.delete_routing` | Delete Routing | Deletes the routing `routing` | none | none |
| `modulation.set_amount` | Set Routing Amount | Sets its `amount` | none | none |
| `modulation.set_curve` | Set Routing Curve | Sets how it bends its output, `curve`: `linear`, `exponential`, `logarithmic`, `s_curve`, `stepped` in `steps` (from 2 to 128, as many as it had, or 4, without it), `absolute`, `positive` or `negative` | none | none |
| `modulation.set_scale` | Set Routing Scale | Has output `output` of the modulator `modulator`, on the routing's channel, multiply its amount, as velocity would, or nothing without `modulator` | none | none |
| `modulation.set_lag` | Set Routing Lag | Sets how long it takes to follow its output, `lag`, in ms from 0, at once, to 10,000: about two thirds of the way in that time | none | none |
| `modulation.set_bypass` | Set Routing Bypass | Bypasses it, so it moves nothing and keeps its settings, or brings it back with `bypassed` false | none | none |

Each is a step in the undo history: adding is named after the kind, such
as "Add LFO", "Add Knob" or "Add Button", and deleting and moving after
the modulator or the control, such as "Delete LFO 2". Changing a
modulator's parameter is "Change LFO 2", and a run of changes to one
parameter merges into one step, as a control's drag does. A routing's
steps are "Add Routing", "Set Routing Amount" and so on, and bypassing it
"Bypass Routing" or "Bring Back Routing". Deleting a modulator, a control,
a device, a send, a wire or a track deletes the routings from it and to
its parameters as part of the same step, so undo puts them back. A routing
whose parameter goes out of the modulator's reach, as when its track
leaves the group, stays in the project and moves nothing until it's back.
The engine plays modulators without a new graph for each change of a
parameter or an amount ([engine.md](engine.md#modulation)). The app's
modulator strip is still to come.

### Automation

Automation moves a parameter through points over the song
([project-format.md](project-format.md#lane)). Each command names
its parameter with an argument of type `param`, a `ParamAddress`, which
scripts write as text: "track 3 volume", "master pan", "send 2",
"device 4 param 17", "device 4 bypass", "wire 1", "clip 5 transpose" or
"modulator 6 param 3" (`param_text`). A built-in device's parameter is its
number in its kind's table, a plugin's its id in the plugin, and a test
device's is 0. A modulator's is its number in an LFO's or an Envelope's
table, or a Macro's knob or button ([Modulation](#modulation)). Every
device has a Bypass, whatever its kind, which steps between off, 0, and
on, 1, where on is bypassed, as a plugin's own bypass parameter does. An audio clip's are its gain and its
warp's settings, by their keys as Show Clip Lane takes them
([Warp modes](#warp-modes)): their curves are in the clip, in its own
time, where the Sample Editor's lane shows them, and an unwarped clip plays
only its gain's.

| Command | Title | Arguments | What it does | Default key | Live |
|---|---|---|---|---|---|
| `automation.show` | Show/Hide Automation | `track` or `master` | Shows the automation lanes of a track, the selected one without either, or the master, or hides them: a lane for each parameter it automates, or else one for its volume | A | A (Automation Mode) |
| `automation.show_lane` | Show Automation Lane | `param`, `instead_of` | Shows a lane for a parameter, in place of the lane of `instead_of`, or under the others | none | none |
| `automation.hide_lane` | Hide Automation Lane | `param` | Hides a parameter's lane | none | none |
| `automation.set_points` | Edit Automation | `param`, `at`, `values` | Sets all of a parameter's points: where each is, and its value in the parameter's own units | none | none |
| `automation.add_point` | Add Automation Point | `param`, `at`, `value` | Adds a point, after any already there, so it ends a step | none | none |
| `automation.delete_points` | Delete Automation Points | `param`, `start`, `end` | Deletes the points from `start` to `end` | none | none |
| `automation.clear` | Clear Automation | `param` | Deletes a parameter's automation, which leaves it with the value it had at the playhead | none | none |
| `automation.re_enable` | Re-Enable Automation | `param` | Has a parameter follow its automation again after someone moved it by hand, or every overridden one without `param` | none | Re-Enable Automation, Back to Arrangement |

Values are kept within the parameter's range where the project knows it,
as it does for the mixer's, the built-in devices', the clips' and the test
devices' parameters, and a switch's, a choice's, a note's, a count's, a
Bypass's or a clip's Preserve's and Transient Loop Mode's are rounded to whole numbers,
as their steps; a plugin keeps its own in range. Deleting a lane's last point clears it. The arrangement's
gestures run these, and a drag of a point or a stretch of line, or a drag
that draws, runs Edit Automation once, when it ends
([app.md](app.md#the-arrangement)).

Moving a control by hand overrides its automation, as in Live:
`Transaction::set_param_by_hand` sets the value and marks the lane
overridden in the same step, so one undo puts both back. The mixer's
changes, `device.set_param` (and so the built-in devices' controls),
`device.set_bypass` (and so the Bypass button) and what plugins change in
their own windows all do. An overridden lane keeps its points, which can be edited as
usual, and the parameter holds the value it was moved to until Re-Enable
Automation.

### Browser

The browser's library is the app's, as the transport is: the samples are
on its disk, and its engine plays the previews. Commands reach it through
the `Library` in their `Context`, which the app implements, and a session
without one, such as an export's, says there's no library. What a search
or Find Similar finds is what the browser lists, so an agent's search shows
there too. A file is named by its full path.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `browser.search` | Search the Library | Finds the samples whose names, folders or tags have every word of `text`, best first, and returns the first `limit` of their paths, 100 without it | none | Typing in the browser's search field |
| `browser.find_similar` | Find Similar Samples | Finds the samples most like `file`, most alike first, and returns the first `limit` | none | Show Similar Files |
| `browser.preview` | Preview Sample | Plays the audio file `file` on its own, beside the song, in place of what was previewing | none | Clicking a sample, with Preview on |
| `browser.stop_preview` | Stop Preview | Stops it | none | none |
| `browser.tag` | Tag Sample | Gives `file` the tag `tag`, or with `on` false takes it away, and returns whether that changed it | none | Live 12's tags |
| `browser.add_folder` | Add Folder to Library... | Adds `folder` to the library and looks through it, or asks which folder without it | none | Add Folder..., under Places |
| `browser.remove_folder` | Remove Folder from Library | Takes `folder` out of the library, leaving its files where they are | none | Remove from Sidebar |
| `browser.rescan` | Rescan Library | Looks through `folder` again, or every folder and the presets | none | none |
| `browser.focus_search` | Search in Browser | Shows the browser, with the keys in the search field of the list showing | Ctrl+F | Ctrl+F |
| `browser.set_plugin_kind` | Set Plugin Kind | Marks the installed plugin `plugin` of the format `format` as `kind`, `instrument`, `midi_effect` or `audio_effect`, for when the scan's rule gets it wrong: the browser lists it in that kind's folder from then on, and adds it as that kind. Marking it as the kind the scan found takes the mark away | none | none |

The library's changes aren't the project's, so they aren't in its undo
history: a tag is taken away with `browser.tag` too, and a plugin marked
as another kind is marked back with `browser.set_plugin_kind`. Add Folder without a
folder and Search in Browser ask the app, with `Request::AddLibraryFolder`
and `Request::FocusBrowserSearch`.

### The mixer's controls

The mixer's faders, pan, sends, buttons and menus hand back a
`mixer::Change`. `Change::command` names the command that makes it, with
its arguments, such as `mixer.set_volume` with `track` and `gain`, and the
app runs that, so scripts and agents set the same values the same way.
`mixer::change` makes a change as one transaction directly, named for the
Edit menu:

| Change | What it does | Named |
|---|---|---|
| `Gain`, `MasterGain` | Sets a fader | Volume |
| `Pan`, `MasterPan` | Sets a balance | Pan |
| `Muted` | Mutes or unmutes a track | Mute, Unmute |
| `Solo` | Solos or unsolos a track; `exclusive` unsolos the others | Solo, Unsolo |
| `SoloSafe` | Leaves a track playing while others are soloed | Solo-Safe |
| `Armed` | Arms a track for recording | Arm, Disarm |
| `Input` | Sets what a track records from: an interface's input, another track's tap, Resampling, or nothing | Input |
| `Monitor` | Sets when a track plays its input instead of its clips: In, Auto or Off | Monitor |
| `Output` | Sends a track's output to the master, a group it can feed, or nowhere: Sends Only | Output |
| `ClipType`, `MasterClipType` | Sets a channel's clip stage | Clip Type |
| `SendLevel` | Sets a send's level, adding the send if there isn't one | Send Level |
| `SendPreFader` | Takes a send before or after the fader | Pre/Post |
| `RemoveSend` | Removes a send | Remove Send |
| `Delete` | Deletes a track; what fed it feeds the master, sends to it go, and tracks taking their input from it go to No Input | Delete Track |
| `MidiTo` | Has a MIDI track play an instrument on another track, on a MIDI channel or on the notes' own, or none | MIDI To |

The commands are `mixer.set_volume`, `set_pan`, `set_mute`, `set_solo`,
`set_solo_safe`, `set_arm`, `set_input`, `set_monitor`, `set_output`,
`set_clip_type`, `set_send_level`, `set_send_pre_fader`, `remove_send`,
`set_midi_to`, `set_master_volume`, `set_master_pan` and
`set_master_clip_type`; deleting a track is `track.delete`. Gains are linear, 1 is unity; pans go from -1
(left) to 1. A change the routing rules don't allow, such as an output to
a group that feeds the track, does nothing. An input, an output or a new
send that would close a loop through a track's input or a wire is refused,
with an error naming it ([Wires](#wires)), which the app shows in its
status bar. `mixer.set_output` feeds the master without `group`, and
nothing with `sends_only`: the track is heard through its sends and the
tracks that take its sound alone.

`mixer.set_input` takes an input of the kind the track records: the input's
`device` id, as the audio settings list it, or the default input without
one, and its `name`, to show while it's missing. On an audio track,
`channel` is the first channel, from 0, and `stereo` says whether it takes
the next one too (it does without it). On a MIDI track, `channel` is the
one MIDI channel to take, from 0 to 15, or every channel without it.
`none` records from nothing. Buses take no input.

An audio track can take another track's tap instead, as a wire does
([Wires](#wires)): `from`, a track, with `tap`, `pre_fx`, `post_fx`
without it, or `post_fader`, or `after`, a device on another track, for
what it puts out. `resampling` takes the master's Post Fader, the one tap
of the master's a track can take: the track records the mix and plays
nothing while it does. A track that takes a tap in place of what it took
monitors In, in the same step, so it plays what it takes, but for
Resampling, which leaves its monitoring alone. A MIDI track's Pre FX is
notes, and so is the point after each of its MIDI effects in front of its
instrument, which an audio track can't take. A MIDI track takes them from
another MIDI track, `from` without a `tap` taking its Pre FX and `after` a
MIDI effect what that plays, and its Post FX too, `post_fx`, the notes
that reach its instrument: a tap that carries sound alone is refused. An input that would close a loop, from a track
the track's own sound reaches, is refused with an error that says so, such
as "That would make a loop: what comes out of Print reaches Kick Post FX"
(`Project::check_input`). Resampling never closes one.

`mixer.set_midi_to` sets a MIDI track's MIDI To: the note wires from its
Post FX, which play the notes that reach its instrument, or would, on
another track's. It plays `device`, an instrument on another track, on
`channel`, from 0 to 15, or each note on its own without it, and nothing
without `device`. It keeps the wire into `device`, if there is one, with
its id and its place, and removes the track's others, so a track wired
into several in the Routing panel plays one again. An instrument on the
track itself or on the master, one that isn't an instrument, a track that
isn't a MIDI track, and an instrument whose sound reaches the track's
notes are refused, such as "Synth is on Strings itself, which plays it
already".

### Options and View

The app's windows are commands too, so their keys can be changed like any
other. They push a `Request`, as the file commands do.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `options.settings` | Settings... | Shows the Settings window, at the page it showed last | Ctrl+Comma | Ctrl+Comma (Preferences) |
| `options.audio_settings` | Audio Settings... | Shows the Settings window's Audio page | none | none |
| `options.keyboard_shortcuts` | Keyboard Shortcuts... | Shows the Settings window's Keys page, where keys are changed | Ctrl+K | Ctrl+K (Key Map Mode) |
| `options.appearance` | Appearance... | Shows the Settings window's Appearance page, where the theme is picked | none | none |
| `options.profiles` | Profiles... | Shows the Settings window's Profiles page, where profiles are made and switched between | none | none |
| `options.agents` | Agents... | Shows the Settings window's Agents page, where agents are let in ([agents.md](agents.md)) | none | none |
| `options.next_profile` | Switch to Next Profile | Switches to the next profile, in the Profiles page's order, and from the last back to Default, saving the workspace in use first | none | none |
| `view.command_palette` | Command Palette... | Shows the command palette | Ctrl+Shift+P | none |
| `view.toggle_mixer` | Mixer | Shows or hides the mixer under the arrangement | Ctrl+Alt+M | Ctrl+Alt+M |
| `view.toggle_routing` | Routing | Shows or hides the Routing panel, in a tab beside the mixer | Ctrl+Alt+I | Ctrl+Alt+I (the mixer's In/Out) |
| `view.toggle_performance` | Performance | Shows or hides the Performance panel, which lists what each device and each track's strip take of the CPU, in a tab beside the mixer | Ctrl+Alt+C | none |
| `view.toggle_devices` | Devices | Shows or hides the devices panel | Ctrl+Alt+L | Ctrl+Alt+L (Detail View) |
| `view.toggle_clip_editor` | Clip Editor | Shows or hides the editor that fits the clip selected or opened last: the piano roll, or the Sample Editor for an audio clip | Ctrl+Alt+3 | Ctrl+Alt+3 (Clip View) |
| `view.switch_devices_piano_roll` | Switch Devices and Clip Editor | Brings that editor in front of the devices, or the devices in front of it | F12 | F12, Shift+Tab |
| `view.toggle_piano_roll` | Piano Roll | Shows or hides the piano roll, in a tab beside the devices | none | none |
| `view.toggle_sample_editor` | Sample Editor | Shows or hides the Sample Editor, in a tab beside the devices | none | none |
| `view.toggle_browser` | Browser | Shows or hides the browser | Ctrl+Alt+B | Ctrl+Alt+B (Browser) |
| `view.toggle_history` | History | Shows or hides the History panel, in a tab beside the browser | Ctrl+Alt+H | none |
| `view.move_to_new_window` | Move Panel to New Window | Moves the panel with the focus to a new window of its own, as its tab's menu does; the arrangement and a window's last panel stay where they are | none | none |
| `view.move_to_main_window` | Move Panel to Main Window | Sends the panel with the focus back to where it was in the main window | none | none |
| `options.rescan_plugins` | Rescan Plugins | Scans for plugins again, trying the libraries left out as bad too | none | Rescan, in Preferences |
| `view.zoom_in` | Zoom In | Zooms the view with the focus in, around the start marker | Equals, NumpadAdd | + |
| `view.zoom_out` | Zoom Out | Zooms it out | Minus, NumpadSubtract | - |
| `view.zoom_to_selection` | Zoom to Selection | Shows the selection across the arrangement, the selected notes in the piano roll, or the selected warp markers in the Sample Editor | Z | Z |
| `view.toggle_follow` | Follow | Scrolls the arrangement to keep the playhead in sight, or stops | Ctrl+Shift+F | Ctrl+Shift+F |
| `view.toggle_take_lanes` | Show/Hide Take Lanes | Shows the takes of the selected tracks, or of the track the panels show, in lanes under them, or hides them; `tracks` names other tracks | Ctrl+Alt+U | Ctrl+Alt+U |
| `view.narrow_grid` | Narrow Grid | Brings the grid's lines closer | Ctrl+1 | Ctrl+1 |
| `view.widen_grid` | Widen Grid | Takes them further apart | Ctrl+2 | Ctrl+2 |
| `view.triplet_grid` | Triplet Grid | Turns triplet lines on or off | Ctrl+3 | Ctrl+3 |
| `view.toggle_snap` | Snap to Grid | Turns snapping on or off | Ctrl+4 | Ctrl+4 |
| `view.fixed_grid` | Fixed Grid | Keeps the grid's step whatever the zoom, or lets it follow the zoom | Ctrl+5 | Ctrl+5 |

`view.set_viewport` (`start`, `zoom`, `top`, `width`) and
`view.set_track_height` (`track`? or `master`?, `height`) carry the
arrangement's scrolling, zooming and the heights of its tracks and of the
master's row, `view.set_piano_roll_viewport` (`start`, `zoom`, `top`,
`width`, `height`, `key_height`?) the piano roll's, with how tall its rows
of keys are, and `view.set_sample_editor_viewport` (`start`, `zoom`,
`width`) the Sample Editor's. `view.set_focus` gives a panel the focus
(`panel`: `arrangement`, `mixer`, `devices`, `piano_roll`, `sample_editor`,
`browser`, `history` or `routing`), the piano roll and the Sample Editor
only while they show a clip. They and the take lanes change the editor's
state, not the project, so undo leaves them alone. Live has no command
palette. Ctrl+Shift+P is the usual key for one in other apps. Live 12 also
switches between its clip view and its device view with Shift+Tab; here
Shift+Tab moves the keyboard focus back, as it does everywhere else.

### Workspace

Workspaces lay the panels out, and keep the parts of the transport bar,
the track headers and the mixer strips ([app.md](app.md#workspaces)).
They're the app's, not the project's, so these push a `Request` too, and
don't change the project. Each of the user's workspaces, up to nine, is in
a numbered slot, the first being Default, so its switch command, and its
key, stay the same as others come and go. The Workspaces menu shows each
slot's workspace by name, and leaves empty slots out.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `workspace.switch_1` | Switch to Workspace 1 | Switches to Default, saving the workspace in use first | Alt+1 | none |
| `workspace.switch_2` to `workspace.switch_9` | Switch to Workspace 2 to 9 | Switches to the workspace in that slot, if there's one | Alt+2 to Alt+9 | none |
| `workspace.save_as` | Save Workspace As... | Asks for a name, and keeps the layout and the views' parts as a new workspace under it, in the lowest slot free, which is in use from then on | none | none |
| `workspace.rename` | Rename Workspace... | Asks for a new name for the workspace in use; Default keeps its name | none | none |
| `workspace.delete` | Delete Workspace... | Deletes the workspace in use, once the user says so, and switches to Default, which can't be deleted | none | none |
| `workspace.reset` | Reset Workspace | Lays the panels out as at first, all in the main window, and shows the views' parts as at first | none | none |

Live has one layout, so no workspaces, and a second window that shows
its other view, rather than windows of their own for its panels.

## The editor's state

What the arrangement, the piano roll, the Sample Editor and the devices
panel edit besides the project lives in an `Editor`, kept with the `Document` (`Document::editor` and
`editor_mut`), outside the widgets. Commands read and change it just as the
UI does, and so will extensions and agents. It isn't saved with the
project, undo leaves it alone, and opening a project starts a new one, but
for the behaviour settings.

- **The selection** (`Selection`): clips, a time selection (a stretch of
  beats on some tracks, and maybe on the master, which has no clips for
  the edit commands but loops, zooms and exports as they do), or tracks,
  and the selected track, which new clips
  and pasting start from, and whose devices the devices panel shows. The
  selected track (`selected_track`, a `Chain`) can be the master track,
  which is selected alone: new devices and Show/Hide Automation go to the
  master then, and the commands that need a track, such as Paste, act as
  if none were selected.
- **The start marker**: where Play/Stop plays from, Paste pastes to and
  Split splits the selected clips. Clicking in the arrangement puts it
  there.
- **The grid** (`Grid`): adaptive, with lines at least `Grid::MIN_SPACING`
  points apart and a density Narrow and Widen Grid change, or fixed at a
  step; triplets; and whether gestures snap to it.
- **The viewport** (`Viewport`): the position at the left edge, the zoom in
  points per beat, how far down the tracks are scrolled, and the timeline's
  width, which the arrangement reports so zooming to fit works. Follow
  turns its pages while the playhead plays out of sight.
- **Track heights** in points, between `view::MIN_TRACK_HEIGHT` and
  `MAX_TRACK_HEIGHT`, for the tracks whose height was changed, and the
  master's row, by their `Chain`.
- **The focus** (`Focus`): the panel the keys and the edit commands act in,
  by its id (`crates/commands/src/panel.rs`): the arrangement, the mixer,
  the devices, the piano roll, the Sample Editor, the browser, the history
  or the routing. `Focus::edits` says what the edit commands act on there
  (`Edits`): the arrangement's selection in the arrangement and the mixer,
  the notes in the piano roll, the warp markers in the Sample Editor, the
  selected devices in the devices panel, and nothing in the others. The zoom
  acts on the piano roll or the Sample Editor while it has the focus, and on
  the arrangement otherwise. Clicking in a panel or on its tab gives it the
  focus ([app.md](app.md#the-panels)), and so do the selection commands of
  the arrangement, the piano roll, the Sample Editor and the devices panel.
  A panel's own keys work while it has it ([below](#the-keymap-file)).
- **The piano roll** (`editor::PianoRoll`): the MIDI clip it shows, its
  selected notes, its own viewport (song positions across, as the
  arrangement's, so the start marker, the playhead and the grid line up,
  and how far down the keys are scrolled), how tall a key's row is, the
  rows' height on screen, Draw Mode, what Cut and Copy took from it, and
  what the lane under the notes shows (`Lane`: the velocities, or a
  controller). The grid is the arrangement's, at the piano roll's zoom.
- **The Sample Editor** (`editor::SampleEditor`): the audio clip it shows,
  its selected warp markers, by their positions in the clip's own time,
  its own viewport, song positions across as the piano roll's are, and
  which of the clip's parameters its lane shows (`ClipParam`). Its grid is
  the arrangement's too, at its zoom.
- **The devices panel** (`editor::Devices`): the devices selected there,
  which are always of the chain it shows, the selected track's, so
  selecting another track forgets them (`Editor::select_track`), and
  `Editor::selected_devices` gives them in the order they play; and what
  Cut and Copy took from it (`DeviceClipboard`), the devices as they were,
  a plugin's state and all, with the lanes that automated them.
- **The clip editor** (`Editor::clip_editor`): the panel of the editor
  that fits the clip selected or opened last, the piano roll's or the
  Sample Editor's, which Clip Editor and Switch Devices and Clip Editor
  show.
- **Behaviour** (`Behaviour`): how editing behaves where people's habits
  differ, kept as settings so profiles can follow other DAWs. So far:
  where Play/Stop plays from, what middle C is called (`MiddleC`: C3 as
  in Live, C4, or C5), and whether copies of devices take their automation
  (`copies_take_automation`).
- **Audio files** (`AudioFile`): the sample rate, length and channels of
  each file clips play, by the path the clips give, and the tempo its loop
  data gives, if it has any, which Warp starts a clip at. The app reads
  them ([app.md](app.md#audio-files)). Trimming keeps an audio clip inside
  its file, and positions inside a file count in the file's own frames.
- **The clipboard**: the clips or time Cut and Copy took in the
  arrangement, for Paste.
- **Take lanes**: the tracks whose takes show in lanes under them.
- **Automation lanes** (`Editor::automation`): the parameters whose lanes
  each track, or the master, shows, top to bottom.

## Transactions

Commands change the project only through transactions. The `Document` that
holds the open project hands out `&Project` only, so every change goes
through `Document::transact`:

```rust
ctx.document.transact("Toggle Metronome", |tx| {
    tx.set_metronome(!tx.project().metronome);
});
```

A `Transaction` has a setter for each part of the project that commands
change: the tempo map, the loop region, looping, punching in and out and
the metronome; adding and removing tracks and sends; each mixer setting of
a track, its input and monitoring among them, a send and the master; adding
and removing clips, and their timing, fades and names; adding and removing
notes, and their timing, keys and velocities; adding, naming and removing
takes; adding, moving, naming and removing locators; adding and removing
devices, bypassing and deactivating them, and a plugin's parameters,
settings and side inputs; and each parameter's automation, and whether
it's overridden.
`Transaction::editor` reads the editor's state, such as the files' sample
rates. Each setter records an `Edit` with the new value and the one it replaced. A
setter that changes nothing records nothing, and a transaction with nothing
recorded leaves no trace. Any other is committed under its name, which the
Edit menu shows, and becomes a step in the undo history (see below).

Whatever keeps itself in step with the project takes the committed
transactions from `Document::take_committed`. The app sends the engine what
they changed (see below), and ARA change notifications (Tier 2) will go out
from the same place.

Playing, stopping and moving the playhead don't change the project, so
commands send them straight to the `Transport`, and undo leaves them alone.

A new kind of change gets a setter on `Transaction` and a variant of `Edit`.
`Edit` isn't `#[non_exhaustive]`, on purpose: the app's match on it stops
compiling until it says what the engine should do about the new variant,
and the matches in `document.rs` until undo knows how to take it back (see
below).

## Undo and redo

The `Document` keeps each committed transaction as a step in its undo
history, up to 1,000 of them. `Document::undo` takes the last step back: it
applies the inverse of each of its edits, last edit first, as a new
transaction named after the step, such as "Undo Tempo Up".
`Document::redo` makes it again, as "Redo Tempo Up". Since they're
transactions like any other, the engine follows them the same way. A new
transaction forgets what was undone, so there's nothing left to redo.
`Document::follow` commits a change the project only follows, such as a
plugin changing its own state as it starts: the engine follows it, but
it's no step, so undo doesn't take it back, redo carries on, and the
project isn't modified for it.

- `Document::undo_name` and `redo_name` say what Undo and Redo would
  change, for the Edit menu. `Document::steps` lists every step by name,
  the oldest first, with how many of them are done, for the History panel,
  and `Document::go_to` undoes or redoes until that many are.
- **Gestures.** Dragging the tempo field makes a transaction on every frame
  it moves. Between `Document::begin_gesture` and `end_gesture`,
  transactions with the same name join one step, so one undo takes the whole
  drag back. A gesture that ends where it started leaves no step. Undo and
  redo end a gesture.
- **Unsaved changes.** `Document::is_modified` follows the history: undo
  back to where the project was saved, and it's unchanged again.
- Opening a project starts a new history.
- **Note ids.** A MIDI clip hands out its notes' ids, and taking one isn't
  an edit. So redo makes a clip again as it was made, before it handed out
  the ids of the notes then drawn in it, and `Transaction::add_note` has the
  clip take a note's id it hasn't handed out yet, so the notes come back.

A new kind of edit needs an arm in each match in
[`crates/commands/src/document.rs`](../crates/commands/src/document.rs):
`Transaction::apply` makes it, `Edit::inverse` swaps its old and new values,
`Edit::absorb` merges two in a row for gestures, and `Edit::is_noop` says
when merged ones cancel out. Add an example to `one_of_each` in
`history.rs`, and the tests undo and redo it with every other kind.

### Testing every command

Every command has an example in
[`crates/commands/src/every_command.rs`](../crates/commands/src/every_command.rs):
arguments that act on its test project, which has something of each kind,
and what the command does. It changes the project as one step; or it leaves
the project and its history alone, as selecting, playing and asking the app
for something do; or it steps through the history, as undo and redo do. A
command without an example fails `every_command_has_an_example`, and one
that works more than one way, such as on clips or on notes, or with
arguments or without, has an example of each.

The tests run each example on its own, then 256 random runs of up to 80 of
them, with undo and redo in between, each acting on what the ones before
left and made. They check that:

- undo takes each step back, and redo makes it again, exactly, but for the
  counters that hand out ids, which never go back, so no id is used twice;
- the commands that don't edit leave the project and the history alone;
- the Edit menu's names and the unsaved-changes flag follow the history;
- a copy of the project that only applies the committed edits, as the
  engine follows them, stays in step;
- each project a step makes saves, and loads back as it was.

Each example also runs as an agent runs it, with its arguments written as
JSON and read back (`json::agent_call`), and must do just what it does from
the UI, result and all, unless it's left to the user, which a command that
asks for a window or a question must be, or needs an argument the app would
have asked the user for. Random runs of examples in one batch check that
it's one step, which undo takes back whole, or no step at all if one of
them fails, whatever was there to redo, and that commands that step through
the history run alone.

To try more runs:

```sh
PROPTEST_CASES=5000 cargo test -p musicable-commands --lib every_command
```

## Running commands in the app

`musicable_app::Session` holds the open project's document, the commands, the
keymap, the engine's handle and a `Compiler`. Opening a project, with
`Session::new` or `Session::open`, compiles its graph on the compiler's
thread and sends the engine its tempo map and loop; `Session::open` also
stops the transport at the start of the song. Then:

- `Session::key_pressed` looks a chord up in the keymap and runs the command
  bound to it where the focus is (`Keymap::lookup_focused`): the panel's
  own, or else the one everywhere. It returns `false` if no command is
  bound, so the key can go elsewhere. `Session::takes_key` says whether a chord's command applies
  now; the window leaves the keys it doesn't take to egui.
- `Session::run` runs a command by id, for menus and the command palette,
  and `Session::run_with` runs one with arguments, for the arrangement's
  gestures and context menus, and returns its result.
  `Session::run_batch` runs several as one step, for agents.
- `Session::editor` and `editor_mut` are the editor's state. The app puts
  the audio files' headers there as it reads them.
- After each command, the session sends the engine what the committed
  transactions changed:

  | Edit | Sent to the engine |
  |---|---|
  | Tempo map | `EngineHandle::set_tempo_map`, with the last new map |
  | Loop region, looping | `TransportCommand::Loop`, with the loop as it is now |
  | Metronome | The metronome's `Metronome::ENABLED` parameter |
  | Track or master gain and pan, send level | The fader's or the send's `Gain::GAIN` or `Gain::PAN` parameter |
  | Mute, solo, solo-safe | `Gain::MUTE` for each output stage and send whose mute changed |
  | Adding or removing a track or a send, an output, pre/post, a clip type | A new graph, which the compiler's thread builds |
  | Adding or removing a clip, its timing, its fades | A new graph |
  | Adding or removing a note, its timing, key or velocity | A new graph, which lets go of a note that was sounding where it changed |
  | Adding or removing a device | A new graph, after its plugin loads or goes |
  | Bypassing a device | Its switch's `Switch::BYPASS` parameter ([engine.md](engine.md#bypassing-devices)), and the link sets the plugin's own bypass ([plugins.md](plugins.md#plugins-on-tracks)) |
  | Deactivating or activating a device | A new graph |
  | A plugin's parameters and settings | Nothing: the plugin gets them ([plugins.md](plugins.md#plugins-on-tracks)) |
  | Track and clip names, arm, locators | Nothing yet |

  Play, stop and go to start send their `TransportCommand` as they run.
- `Session::take_requests` hands over what the commands asked of the app,
  oldest first.
- `Session::set_tempo` is for a tempo field: it sets the tempo at the
  playhead, as one transaction, with `transport::set_tempo`.
- `Session::change_mixer` makes a change from the mixer's controls, and
  `Session::meters` has the meters the engine measures into, for the mixer
  to read.
- `Session::begin_gesture` and `end_gesture` make a drag one step in the
  undo history, and `Session::history` lists its steps, for the History
  panel and the Edit menu.
- `Session::revision` goes up with every change to the project, undone and
  redone ones too, and when another project opens, for what follows the
  project, such as the app's reading of its audio files.
- `Session::heard_position` is where the playhead is in what's heard:
  while playing, the engine's position less the graph's latency
  (`Stats::latency`) and the device's output latency, which the app passes
  in. The transport bar and the arrangement show that, and
  `Session::beats_at` turns it into beats.
- `Session::tick` belongs in every UI frame. While the engine's queue is
  full, as when the audio device has stopped, the session holds on to what
  doesn't fit, and `tick` sends it, in order, once there is room. It also
  sends the graphs the compiler has finished, and drops the graphs and
  tempo maps the engine is done with. And it polls the plugins: what they
  ask for, and the changes made in their windows, which it makes
  transactions of.
- **The browser's library.** `Session::set_library` gives the session the
  app's library, which the browser commands reach. `Session::tick` takes in
  what its file work found, hands the engine the previews it opened, and
  tells the library's queue whether the engine plays or records, so scans
  slow down ([app.md](app.md#behind-the-browser)).
  `Session::set_preview_gain` sets how loud previews play.
- **Plugins.** `Session::set_installed_plugins` says which plugins are
  installed, from a scan, and loads the plugins of the project's devices
  among them. `Session::plugin` is a device's plugin instance, once it has
  loaded, `plugin_problem` why it hasn't, and `take_problems` what went
  wrong since. `Session::saved_project` is the project with each plugin's
  state saved afresh, for writing to a file.
- **Waiting for the graph.** A newly opened project's tempo map and loop,
  and whatever follows them, such as Play, wait for its graph: until it
  comes, the engine plays silence and doesn't start. The project in the
  window changes at once. `Session::compiling` says whether a graph is on
  its way, so the window can tick more often, and `Session::wait_for_graph`
  blocks until it has arrived, for offline playing such as tests and the
  null test. A graph compiled for a project that has since been closed is
  dropped.
- `Session::replace_engine` moves the project to a new engine, such as one
  built for another device's sample rate or buffer size. The new engine
  takes over where the old one was, in bars and beats, playing if the old one
  was, once the project's graph for it has compiled.

[`crates/app/tests/key_presses.rs`](../crates/app/tests/key_presses.rs)
drives an engine this way from key presses, undo and redo included,
rendering offline,
[`crates/app/tests/app_shell.rs`](../crates/app/tests/app_shell.rs) checks
what the app shell asks of the session,
[`crates/app/tests/mixer.rs`](../crates/app/tests/mixer.rs) checks that the
mixer's changes reach the engine and the meters move, and
[`crates/app/tests/slow_graphs.rs`](../crates/app/tests/slow_graphs.rs)
checks that a node that's slow to prepare doesn't hold up the session.
[`crates/app/tests/plugins.rs`](../crates/app/tests/plugins.rs) puts the
test plugins on tracks, and checks that changes made in their windows
undo and redo.
[`crates/app/tests/piano_roll.rs`](../crates/app/tests/piano_roll.rs) edits
notes with the piano roll's commands and keys, and checks that the test
instrument plays them as they are after each change and each undo.

## Key chords

A key chord is a `KeyChord`: its `Modifiers` and its `Key`, which keymap
files write joined by `+`, as `Space`, `Ctrl+L` or `Ctrl+Shift+Up`, in any
case and with the modifiers in any order. Displaying a chord writes it with
the modifiers as Ctrl, Alt, Shift, Cmd
([`key.rs`](../crates/commands/src/key.rs)). [Key
names](../manual/src/customising/keys.md#key-names) in the manual lists
the modifiers and the keys, with the other spellings they take, and
[app.md](app.md#keyboard-layouts) says how key presses on other keyboard
layouts get their names. The default keymap uses Ctrl for now; a macOS
default with Cmd comes with macOS support.

## The keymap file

The default keymap is
[`crates/commands/keymaps/default.toml`](../crates/commands/keymaps/default.toml),
built into the app, and a user's keymap file holds only their changes to it,
as [Your keymap file](../manual/src/customising/keys.md#your-keymap-file) in
the manual describes. `KeymapFile` is such a file:

- `KeymapFile::parse` reads one. An error means the file as a whole can't be
  used ([below](#mistakes)); a mistake in one entry comes back with the file,
  as a `Problem`, and the entry is skipped.
- Entries under `[bindings]` work everywhere (`Scope::Everywhere`), and
  those under a panel's table in `[panels]` in that panel only, while it has
  the focus (`Scope::Panel`), where they win over the ones everywhere. A
  chord runs one command in each place. The default keymap's arrow keys for
  notes are the piano roll's, and Up and Down are Undo and Redo in the
  History panel.
- The app writes only the commands whose chords differ from the default,
  everywhere or in a panel ([below](#changing-keys-in-the-app)), so new
  default shortcuts in later versions still reach the user. `format` and
  `version` are optional in a file written by hand; Musicable writes both,
  like the header of a project file.

### Mistakes

`KeymapFile::parse` reports a `Problem` for:

- an entry that isn't a chord or a list of them, or has a chord that
  doesn't parse, which leaves the command its default chords;
- a chord that two entries of one table claim, which then runs neither
  command there;
- a table for a panel Musicable doesn't have, or a panel's entry that isn't
  a table, which is skipped.

A binding for a command this version doesn't have isn't a problem: it's
kept, in case a later version or a plugin adds it, and `Keymap::dangling`
lists it. A `KeymapFileError` means the file as a whole can't be used: it
isn't valid TOML, has an unknown field, names another `format`, or comes
from a newer version. Musicable then uses the default keymap and shows the
error. The Settings window's Keys page lists the problems and the dangling
bindings ([Mistakes](../manual/src/customising/keys.md#mistakes) in the
manual).

### Trying it out

`musicable keymap [FILE]` prints the chords a keymap file gives every
command, along with any mistakes in it, without opening the window:

```sh
cargo run -p musicable-app -- keymap path/to/keymap.toml
```

Leave out the path to see the default keymap. Keys that work in one panel
only say which, as in `Up in piano_roll`.

## Changing keys in the app

The Settings window's Keys page
([Keys](../manual/src/customising/keys.md#the-keys-page) in the manual)
builds on these:

- `Keymap::bind` returns the command a chord had, which the page warns
  about before it takes the chord, and `bind_in` binds one in a panel
  (`Scope::Panel`). `Keymap::reset_command` gives a command its default
  chords back, everywhere and in each panel, taking them from whichever
  commands have them there now.
- `Keymap::chords_focused` is what menus and the palette show beside a
  command: its chords in the panel with the focus, and those everywhere
  that the panel doesn't give another command.
- `KeymapFile::diff(&defaults, &keymap)` is what the app saves: only the
  commands whose chords differ from the default. The app keeps the user's
  file with `UserKeymap` (`crates/app/src/keymap_file.rs`), which also makes
  the backup copy.
- `musicable_ui::KeyPresses::capture` takes the next chord pressed, any key
  at all, while the page waits for one.
- `Keymap::dangling` lists the chords bound to commands this version doesn't
  have, which the page shows with a Remove button.

[`crates/app/src/app/remapping.rs`](../crates/app/src/app/remapping.rs)
drives the whole window as eframe would: it gives every command that takes
no arguments new keys on the Keys page, finds each in the command palette
with its keys, and starts the app again to see they're kept.
