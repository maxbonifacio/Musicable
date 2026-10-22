# Commands and keymaps

Every user action in Musicable is a named command, and key presses reach
commands only through a keymap. Menus, the command palette and the
arrangement's mouse gestures run the same commands, and scripts and agents
will, so remapping a shortcut is just editing the keymap. This page covers
the commands so far, the editor's state they read and change, the
transactions they change the project through, undo and redo, how the app
runs them, key chords and the keymap file. How the app's window hands key
presses to the keymap, and how keys are changed there, is in
[app.md](app.md#keys).

## Commands

A command has a dotted id, `area.action` in lower case, and a title in title
case for menus and the palette. Keymap files name commands by id, so a
command keeps its id once it has shipped. Each area registers its commands
with the `Registry`, as `transport::register` does, and
`musicable_commands::register_all` registers every area's. A running command
gets a `Context`: the open project's `Document`, with the editor's state
(below), the engine's `Transport`, and a list of `Request`s for what only
the app can do, such as showing a file dialog.

### Arguments and results

A command can take arguments, and give back a result, so the arrangement's
gestures, scripts and agents can say exactly what to change. Its `params`
list them: each `Param` has a name, a `Type` (a bool, an integer, a number,
text, a position in beats, a track, a clip, a locator, or a list of one of
those), whether it's optional, and a line on what it's for.

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
  out: the keyboard shortcuts window and the palette don't list them. The
  arrangement's context menus fill them in from where they were opened
  ([app.md](app.md#the-arrangement)).

### When a command applies

A command's `when` says when it applies without arguments: always, or only
with clips selected, with something to paste, and so on (`When`). Menus,
context menus and the palette grey out a command that doesn't apply, and a
key that runs one does nothing: `Registry::run` returns without running it,
and `run_with` returns `CommandError::NotNow`. A command given arguments
runs whatever is selected, as they say what it works on.

### Transport

The default keymap follows Ableton Live's shortcuts where Live has one.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `transport.play_stop` | Play/Stop | Plays from the start marker, or stops if playing | Space | Space |
| `transport.play` | Play | Plays from the playhead: where playing stopped | Shift+Space | Shift+Space (continue from the stop point) |
| `transport.stop` | Stop | Stops, leaving the playhead where it is | Ctrl+Period | none |
| `transport.record` | Record | Records the armed tracks, playing from the start marker as Play/Stop does; while playing, turns recording on or off | F9 | F9 (Arrangement Record) |
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
([app.md](app.md#settings-and-your-keymap)) has it play from wherever the
playhead stopped instead. Live has no keys for stop and tempo; the ones
chosen do nothing in Live outside its MIDI note editor.

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
| `file.open` | Open Project... | Asks for a project file and opens it | Ctrl+O |
| `file.save` | Save Project | Saves the project, asking where if it has no file yet | Ctrl+S |
| `file.save_as` | Save Project As... | Asks where to save the project, and saves it there | Ctrl+Shift+S |
| `file.import_audio` | Import Audio Files... | Asks for audio files, and puts them on the selected track | none |
| `file.quit` | Quit | Closes Musicable | Ctrl+Q |

New, New Demo, Open and Quit ask about unsaved changes first.

### Edit

The edit commands work on the selection ([below](#the-editors-state)):
the selected clips, or else the time selection, or else the selected
tracks.

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
| `edit.select_all` | Select All | Selects every clip | Ctrl+A | Ctrl+A |
| `edit.rename` | Rename | Asks for a new name for the selected clip, or the selected track; `clip`, `track`, `locator` or `take` names something else | Ctrl+R | Ctrl+R |

And for the arrangement's clicks, scripts and agents:

| Command | Title | Arguments | What it does |
|---|---|---|---|
| `edit.select_clips` | Select Clips | `clips`, `toggle`? | Selects the clips, or adds them to the selection or takes them away |
| `edit.select_time` | Select Time | `start`, `end`, `tracks` | Selects a stretch of time on some tracks |
| `edit.select_tracks` | Select Tracks | `tracks`, `toggle`? | Selects tracks |

The Edit menu names the change, as in "Undo Tempo Up". Ctrl+Shift+Z is
the other common key for redo. Clips that land where others are, by
pasting, moving or duplicating, overwrite what's there, as in Live: it's
trimmed back, split around them, or removed. Cut, Copy and Paste keep the
clips in the editor's clipboard, not the system's.

### Clip

These change the clips their arguments name, for the arrangement's
gestures, scripts and agents. Splitting and trimming keep a clip's content
where it plays in the song, so an audio clip split in two plays exactly as
before.

| Command | Title | Arguments | What it does | Default key |
|---|---|---|---|---|
| `clip.move` | Move Clips | `clips`, `by`?, `tracks`?, `copy`? | Moves the clips, or copies them, some beats later or earlier and some tracks down or up; gives back the ids of the clips moved or the copies | none |
| `clip.trim` | Trim Clip | `clip`, `start`?, `end`? | Moves a clip's start or end. An audio clip doesn't go past its file's end | none |
| `clip.set_fades` | Set Fades | `clip`, `fade_in`?, `fade_out`? | Sets an audio clip's fades, which fit inside it | none |
| `clip.rename` | Rename Clip | `clip`, `name` | Names a clip | none |
| `clip.split` | Split Clips | `clips`, `at` | Splits the clips at a position | none |
| `clip.delete` | Delete Clips | `clips` | Deletes clips | none |
| `clip.insert_midi_clip` | Insert MIDI Clip | `track`?, `start`?, `end`? | Adds an empty MIDI clip: over the time selection on its MIDI tracks, or else a bar from the start marker on the selected track | Ctrl+Shift+M |
| `clip.insert_audio` | Insert Audio | `files`, `track`?, `at`? | Adds clips that play whole audio files, one after another, on the selected audio track or a new one, from the time selection's start or the start marker | none |

When the clips can't all land on tracks that hold them, such as an audio
clip on a MIDI track, they keep their tracks and only move in time. Moving
clips before the start of the song stops at the start. Insert Audio needs each file's header read into
the editor first ([below](#the-editors-state)): the app reads it when a file
is dropped or imported.

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
file. A MIDI track's pass (`Recorded::Midi`) is a take with a MIDI clip of
the notes played, each starting and ending on its frames, with its
expression; a pass without notes makes no take. Comping copies
the take's clips, so the take keeps them. `view.toggle_take_lanes` (see
below) shows the takes.

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
through them. The devices panel and the plugin browser run these, with the
device's id as `device`.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `device.add_plugin` | Add Plugin | Adds the plugin `plugin` of the format `format` (`clap` or `vst3`), called `name`, at `index` or the end of the devices of `track`, the selected track, or the master with `master`. `instrument` says it plays notes. Returns the new device | none | Double-click in the browser |
| `device.set_param` | Set Plugin Parameter | Sets the parameter `param` of the plugin `device` to `value`, or leaves it to the plugin's state without one | none | none |
| `device.show_window` | Show Plugin Window | Shows the plugin's own window, or brings it to the front | none | Ctrl+Alt+P (all of them) |
| `device.hide_window` | Hide Plugin Window | Closes it | none | Ctrl+Alt+P |
| `device.delete` | Delete Device | Deletes a device | none | Delete, with the device selected |

Adding, changing and deleting are transactions named after the plugin, such
as "Add Surge XT", "Change Surge XT" and "Delete Surge XT", so undo covers
them. The window commands ask the app, with `Request::PluginWindow`; a
plugin that isn't playing has no window to show, and the status bar says
why. A new device starts with the plugin's own defaults. Its settings fill
in as the plugin changes them ([plugins.md](plugins.md#plugins-on-tracks)).

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
| `Input` | Sets what a track records from, or nothing | Input |
| `Monitor` | Sets when a track plays its input instead of its clips: In, Auto or Off | Monitor |
| `Output` | Sends a track's output to the master or a group it can feed | Output |
| `ClipType`, `MasterClipType` | Sets a channel's clip stage | Clip Type |
| `SendLevel` | Sets a send's level, adding the send if there isn't one | Send Level |
| `SendPreFader` | Takes a send before or after the fader | Pre/Post |
| `RemoveSend` | Removes a send | Remove Send |
| `Delete` | Deletes a track; what fed it feeds the master, and sends to it go | Delete Track |

The commands are `mixer.set_volume`, `set_pan`, `set_mute`, `set_solo`,
`set_solo_safe`, `set_arm`, `set_input`, `set_monitor`, `set_output`,
`set_clip_type`, `set_send_level`, `set_send_pre_fader`, `remove_send`,
`set_master_volume`, `set_master_pan` and `set_master_clip_type`; deleting a
track is `track.delete`. Gains are linear, 1 is unity; pans go from -1
(left) to 1. A change the routing rules don't allow, such as an output that
would make a loop, does nothing.

`mixer.set_input` takes an input of the kind the track records: the input's
`device` id, as the audio settings list it, or the default input without
one, and its `name`, to show while it's missing. On an audio track,
`channel` is the first channel, from 0, and `stereo` says whether it takes
the next one too (it does without it). On a MIDI track, `channel` is the
one MIDI channel to take, from 0 to 15, or every channel without it.
`none` records from nothing. Buses take no input.

### Options and View

The app's windows are commands too, so their keys can be changed like any
other. They push a `Request`, as the file commands do.

| Command | Title | What it does | Default key | Live |
|---|---|---|---|---|
| `options.audio_settings` | Audio Settings... | Shows the audio settings | Ctrl+Comma | Ctrl+Comma (Preferences) |
| `options.keyboard_shortcuts` | Keyboard Shortcuts... | Shows the keys, where they're changed | Ctrl+K | Ctrl+K (Key Map Mode) |
| `view.command_palette` | Command Palette... | Shows the command palette | Ctrl+Shift+P | none |
| `view.toggle_mixer` | Mixer | Shows or hides the mixer under the arrangement | Ctrl+Alt+M | Ctrl+Alt+M |
| `view.toggle_devices` | Devices | Shows or hides the devices panel | Ctrl+Alt+L | Ctrl+Alt+L (Detail View) |
| `view.toggle_browser` | Plugin Browser | Shows or hides the plugin browser | Ctrl+Alt+B | Ctrl+Alt+B (Browser) |
| `options.rescan_plugins` | Rescan Plugins | Scans for plugins again, trying the libraries left out as bad too | none | Rescan, in Preferences |
| `view.zoom_in` | Zoom In | Zooms the arrangement in, around the start marker | Equals, NumpadAdd | + |
| `view.zoom_out` | Zoom Out | Zooms it out | Minus, NumpadSubtract | - |
| `view.zoom_to_selection` | Zoom to Selection | Shows the selection across the arrangement | Z | Z |
| `view.toggle_follow` | Follow | Scrolls the arrangement to keep the playhead in sight, or stops | Ctrl+Shift+F | Ctrl+Shift+F |
| `view.toggle_take_lanes` | Show/Hide Take Lanes | Shows the takes of the selected tracks, or of the track the panels show, in lanes under them, or hides them; `tracks` names other tracks | Ctrl+Alt+U | Ctrl+Alt+U |
| `view.narrow_grid` | Narrow Grid | Brings the grid's lines closer | Ctrl+1 | Ctrl+1 |
| `view.widen_grid` | Widen Grid | Takes them further apart | Ctrl+2 | Ctrl+2 |
| `view.triplet_grid` | Triplet Grid | Turns triplet lines on or off | Ctrl+3 | Ctrl+3 |
| `view.toggle_snap` | Snap to Grid | Turns snapping on or off | Ctrl+4 | Ctrl+4 |
| `view.fixed_grid` | Fixed Grid | Keeps the grid's step whatever the zoom, or lets it follow the zoom | Ctrl+5 | Ctrl+5 |

`view.set_viewport` (`start`, `zoom`, `top`, `width`) and
`view.set_track_height` (`track`, `height`) carry the arrangement's
scrolling, zooming and track heights. They and the take lanes change the
editor's state, not the project, so undo leaves them alone. Live has no
command palette. Ctrl+Shift+P is the usual key for one in other apps.

## The editor's state

What the arrangement edits besides the project lives in an `Editor`, kept
with the `Document` (`Document::editor` and `editor_mut`), outside the
widgets. Commands read and change it just as the UI does, and so will
extensions and agents; the piano roll will share it. It isn't saved with the
project, undo leaves it alone, and opening a project starts a new one, but
for the behaviour settings.

- **The selection** (`Selection`): clips, a time selection (a stretch of
  beats on some tracks), or tracks, and the selected track, which new clips
  and pasting start from.
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
  `MAX_TRACK_HEIGHT`, for the tracks whose height was changed.
- **Behaviour** (`Behaviour`): how editing behaves where people's habits
  differ, kept as settings so profiles can follow other DAWs. So far:
  where Play/Stop plays from.
- **Audio files** (`AudioFile`): the sample rate, length and channels of
  each file clips play, by the path the clips give. The app reads them
  ([app.md](app.md#audio-files)). Trimming keeps an audio clip inside its
  file, and positions inside a file count in the file's own frames.
- **The clipboard**: what Cut and Copy took, for Paste.
- **Take lanes**: the tracks whose takes show in lanes under them.

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
and removing clips, and their timing, fades and names; adding, naming and
removing takes; adding, moving, naming and removing locators; and adding
and removing devices, and a plugin's parameters and settings.
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

- `Document::undo_name` and `redo_name` say what Undo and Redo would
  change, for the Edit menu.
- **Gestures.** Dragging the tempo field makes a transaction on every frame
  it moves. Between `Document::begin_gesture` and `end_gesture`,
  transactions with the same name join one step, so one undo takes the whole
  drag back. A gesture that ends where it started leaves no step. Undo and
  redo end a gesture.
- **Unsaved changes.** `Document::is_modified` follows the history: undo
  back to where the project was saved, and it's unchanged again.
- Opening a project starts a new history.

A new kind of edit needs an arm in each match in
[`crates/commands/src/document.rs`](../crates/commands/src/document.rs):
`Transaction::apply` makes it, `Edit::inverse` swaps its old and new values,
`Edit::absorb` merges two in a row for gestures, and `Edit::is_noop` says
when merged ones cancel out. Add an example to `one_of_each` in
`history.rs`, and the tests undo and redo it with every other kind.

## Running commands in the app

`musicable_app::Session` holds the open project's document, the commands, the
keymap, the engine's handle and a `Compiler`. Opening a project, with
`Session::new` or `Session::open`, compiles its graph on the compiler's
thread and sends the engine its tempo map and loop; `Session::open` also
stops the transport at the start of the song. Then:

- `Session::key_pressed` looks a chord up in the keymap and runs the command
  bound to it. It returns `false` if no command is bound, so the key can go
  elsewhere.
- `Session::run` runs a command by id, for menus and the command palette,
  and `Session::run_with` runs one with arguments, for the arrangement's
  gestures and context menus, and returns its result.
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
  | Adding or removing a device | A new graph, after its plugin loads or goes |
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
  undo history, and `undo_name` and `redo_name` are for the Edit menu.
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

## Key chords

A key chord is its modifiers and its key, joined by `+`: `Space`, `Ctrl+L`,
`Ctrl+Shift+Up`. Case doesn't matter and the modifiers can come in any
order. Musicable writes them as Ctrl, Alt, Shift, Cmd.

| Modifier | Also written | Notes |
|---|---|---|
| `Ctrl` | `Control` | |
| `Alt` | `Option` | |
| `Shift` | | |
| `Cmd` | `Command`, `Super`, `Win` | Command on macOS, the Windows key on Windows |

The keys:

- Letters `A` to `Z` and digits `0` to `9`.
- `F1` to `F24`.
- `Space`, `Enter`, `Tab`, `Backspace`, `Escape`, `Insert`, `Delete`, `Home`,
  `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`.
- Punctuation, named after the character the key types without Shift on a US
  layout: `Minus`, `Equals`, `OpenBracket`, `CloseBracket`, `Backslash`,
  `Semicolon`, `Quote`, `Backtick`, `Comma`, `Period`, `Slash`. The character
  itself works too: `Ctrl+.` is `Ctrl+Period`. The `+` key is `Equals` (or
  `NumpadAdd`), since `+` joins the parts of a chord.
- The numeric keypad: `Numpad0` to `Numpad9`, `NumpadAdd`, `NumpadSubtract`,
  `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter`.
- Other spellings: `Esc`, `Return`, `Del`, `PgUp`, `PgDn`, and `ArrowUp`,
  `ArrowDown`, `ArrowLeft`, `ArrowRight`.

How key presses on other keyboard layouts get these names is in
[app.md](app.md#keyboard-layouts). The default keymap uses Ctrl for now; a macOS default with Cmd comes with
macOS support.

## The keymap file

The default keymap is
[`crates/commands/keymaps/default.toml`](../crates/commands/keymaps/default.toml),
built into the app. Your keymap file holds only your changes to it:

```toml
format = "musicable-keymap"
version = 1

[bindings]
"transport.toggle_metronome" = ["M", "Ctrl+M"]
"transport.toggle_loop" = []
```

- Each line gives a command the chords that run it, in place of its default
  ones. A single chord can be written without the list: `"transport.stop" =
  "Escape"`.
- An empty list, `[]`, unbinds the command.
- Commands that aren't listed keep their default chords.
- A chord runs one command. Giving a chord to one command takes it from the
  command that had it.
- Bare dotted keys work too: `transport.play = ["P"]` is the same as
  `"transport.play" = ["P"]`.
- `format` and `version` are optional when you write the file by hand.
  Musicable writes both, like the header of a project file.

When Musicable saves your keymap, it writes only the commands whose chords
differ from the default, so new default shortcuts in later versions still
reach you.

### Mistakes

A line with a mistake in it, such as a misspelt key, is skipped and the
command keeps its default chords. A chord that two lines claim runs neither
command. Musicable reports both and loads the rest of the file. A binding for
a command that doesn't exist is kept, in case a later version or a plugin
adds it, and reported too. Options > Keyboard Shortcuts lists them all, with
a button to give a chord two lines claim to one of them.

A file that isn't valid TOML, has an unknown field or comes from a newer
version is ignored as a whole: Musicable uses the default keymap and shows
the error.

### Trying it out

The app reads your keymap file from its config folder at startup
([app.md](app.md#settings-and-your-keymap)). Options > Keyboard Shortcuts
shows the keys and any mistakes, and saves every change you make there to
the file ([app.md](app.md#changing-keys)). Without opening the window, this
prints the bindings a keymap file gives, along with any mistakes in it:

```sh
cargo run -p musicable-app -- keymap path/to/keymap.toml
```

Leave out the path to see the default keymap.

## Changing keys in the app

The keyboard shortcuts window ([app.md](app.md#changing-keys)) builds on
these:

- `Keymap::bind` returns the command a chord had, which the window warns
  about before it takes the chord. `Keymap::reset_command` gives a command
  its default chords back, taking them from whichever commands have them
  now.
- `KeymapFile::diff(&defaults, &keymap)` is what the app saves: only the
  commands whose chords differ from the default. The app keeps the user's
  file with `UserKeymap` (`crates/app/src/keymap_file.rs`), which also makes
  the backup copy.
- `musicable_ui::KeyPresses::capture` takes the next chord pressed, any key
  at all, while the window waits for one.
- `Keymap::dangling` lists the chords bound to commands this version doesn't
  have, which the window shows with a Remove button.

[`crates/app/src/app/remapping.rs`](../crates/app/src/app/remapping.rs)
drives the whole window as eframe would: it gives every command that takes
no arguments new keys in the keyboard shortcuts window, finds each in the
command palette with its keys, and starts the app again to see they're
kept.
