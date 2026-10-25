# The window

Musicable's window has the menu bar and the transport bar along its top,
the status bar along its foot, and panels in between: the arrangement in
the middle, with the mixer, the devices panel, the piano roll and the
Sample Editor under it, and the browser and the History panel on the left
once you show them.

## The menu bar

The menu bar lists the commands with their keys, under File, Edit, Track,
Clip, Notes, Automation, Locator, Transport, Browser, View, Workspaces and
Options. [Every command](../reference/commands.md) lists them all.

Undo and Redo name the change they would make, as in "Undo Set Tempo", and
are greyed out when there's nothing to undo or redo, as is any command that
doesn't apply now, such as Split with nothing selected. Toggles, such as
Follow and Snap to Grid, are ticked while they're on.

## The transport bar

The transport bar has go to start, play, stop, record, Capture, loop, punch
in and punch out, the metronome, the tempo, the time signature, and the
playhead in bars, beats and sixteenths and in minutes and seconds. At its
right end are the CPU meter and the dropout counter ([Setting up
audio](../getting-started/audio.md#the-cpu-meter-and-the-dropouts)).
Hovering a button shows its command and keys, hovering Loop shows the loop
region, and hovering Punch In and Punch Out shows where they punch
([Recording and comping](../using/recording.md#punching-in-and-out)).

**The tempo field** sets the tempo where the playhead is, and moves any
other tempos in the song by as much, as Tempo Up and Down do. Drag it, or
click it and type a tempo, which applies on Enter. A whole drag is one step
in the undo history, "Set Tempo".

The transport bar's parts can be hidden, moved and resized
([Workspaces](../customising/workspaces.md#hiding-and-moving-parts)).

## The panels

- **The arrangement** is in the middle: the tracks over time, with their
  clips, under a ruler ([The arrangement](arrangement.md)).
- **The mixer** is under the arrangement, with a strip for each track and
  the master's on the right ([The mixer](mixer.md)). **View > Mixer**
  (Ctrl+Alt+M) hides or shows it.
- **The devices panel** is under the mixer: the selected track's devices,
  or the master's while the master track is selected
  ([The devices panel](devices-panel.md)). **View > Devices** (Ctrl+Alt+L)
  hides or shows it.
- **The piano roll** is in a tab beside the devices panel. Double-clicking
  a MIDI clip, or **View > Piano Roll**, shows it
  ([The piano roll](piano-roll.md)).
- **The Sample Editor** is in a tab beside them. Double-clicking an audio
  clip, or **View > Sample Editor**, shows it
  ([The Sample Editor](sample-editor.md)). **View > Clip Editor**
  (Ctrl+Alt+3) shows or hides whichever of the piano roll and the Sample
  Editor fits the clip selected or opened last, as Live's clip view does,
  and **View > Switch Devices and Clip Editor** (F12) switches between it
  and the devices.
- **The browser** is a panel on the left with the samples, the devices and
  the presets, hidden until Add Device, **Browser > Search in Browser**
  (Ctrl+F) or **View > Browser** (Ctrl+Alt+B) shows it
  ([The browser](browser.md)).
- **The History panel** is in a tab beside the browser, hidden until
  **View > History** (Ctrl+Alt+H) shows it: every step of the undo history
  by name, and clicking one takes the project back or forward to it
  ([The History panel](history.md)).

### Moving and sizing them

- **Drag a tab** onto another panel to split it, beside, over or under,
  where the blue preview shows, or onto a tab bar to join its tabs. A group
  of tabs moves together by its tab bar.
- **Drag the edge** between two panels to size them. A panel keeps its size
  as the window grows and shrinks, and when it moves, and the part with the
  arrangement takes what's left. A group of tabs is as big as the tab in
  front, until its edge is dragged, which sizes them all.
- **A tab's ×** hides its panel, as the View menu's command does, and the
  panel keeps its place: the View menu shows it there again, in front of the
  tabs beside it. The arrangement stays.
- **The devices and the clip editors follow the selection**: selecting a
  MIDI clip brings the piano roll forward, and an audio clip the Sample
  Editor; selecting another track brings the devices forward, and so does
  adding a device, however it's added, which also scrolls the panel to it.
  None of them comes back from hidden by itself.

Devices and presets dragged from the browser go in the devices panel's
chain wherever the panel is.

A [workspace](../customising/workspaces.md) keeps the layout, and can be
reset to the first one.

### The focus

One panel has the focus, and is outlined. Clicking in a panel or on its tab
gives it the focus, and so does moving the keyboard focus into it with Tab;
a click in a menu or in a window over the panels doesn't.

The keys and the edit commands act where the focus is. Copy and Delete take
the notes selected in the piano roll while it has the focus, the clips, time
or tracks selected in the arrangement while the arrangement or the mixer has
it, and nothing in the devices panel, the browser and the History panel.
Delete and Select All act on the Sample Editor's warp markers while it has
the focus. The piano roll and the Sample Editor take the focus once they
show a clip. A panel's own keys,
such as the piano roll's arrow keys, work while it has the focus, and the
menus and the command palette show the keys that work there. When the panel
with the focus is hidden or goes behind another tab, the arrangement takes
it.

### Windows of their own

Any panel but the arrangement can go in a window of its own, on another
monitor say:

- **Move to New Window**, in a tab's right-click menu, or **View > Move
  Panel to New Window** for the panel with the focus, puts the panel in a
  new window, a little down and right of the one it was in, as big as it
  was. **Dragging a tab out** past the window's edge does the same, with the
  new window's title bar where you let go. The arrangement stays in the main
  window, and so does a window's last panel. A window is named after its
  panels and the project.
- **Dragging a tab into another window** puts its panel in that window's
  splits and tabs, where the window shows it would go: back in the main
  window, or in another window of its own. A window whose last panel goes
  closes.
- In a window of its own, panels split and join tabs as in the main window,
  and their tabs can be dragged out to windows of their own again.
- **Move to Main Window**, in the tab's menu, or **View > Move Panel to
  Main Window**, sends the panel back to where it was in the main window.
  **Closing the window** sends all its panels back. Hiding its last panel
  closes it.
- **Each window has its own focus.** Coming back to a window gives the
  focus to the panel that last had it there. The keys go to the window
  you're in: a panel's own keys work in its window, the keys that work
  everywhere, such as Space for play, work in every window, and a text field
  that's being typed in, such as the browser's search, still takes every
  key.
- **Drags cross windows**: samples, devices and presets dragged from a
  browser in a window of its own go on a track, below the tracks or in the
  devices panel in another window, as they do in the main window.
- **Plugin windows** open over the window the devices panel is in, on its
  monitor, and stay in front of that window, on Windows
  ([Plugins](../using/plugins.md#plugin-windows)).
- Each window draws at its own monitor's scaling, and keeps its size as it
  moves between monitors.

## The status bar

The status bar shows errors and notices until they're dismissed, and:

- the audio device's sample rate, buffer, and latency as the device's clock
  measures it: click it for the audio settings;
- while an agent is connected, its last change, such as "Agent: Move Notes":
  click it for the Agents page ([The command
  line](../reference/command-line.md#agents));
- while Musicable finds the tempo and the beats of the project's audio
  files, "Analysing" and the file's name, or how many files there are. It
  does this in the background, once for each file
  ([Warping](../using/warping.md#not-yet));
- the project's name, with "(not saved)" while it has changes. The window's
  title marks them with a `*`.

## And more

- **View > Command Palette** (Ctrl+Shift+P) finds any command by name or
  keys and runs it ([The command palette](command-palette.md)).
- **Options > Settings** (Ctrl+Comma) has a page for each area of the
  settings ([Settings](../customising/settings.md)).
- **The Workspaces menu** switches between layouts of the panels
  ([Workspaces](../customising/workspaces.md)).

**Double-clicks** count when the second click comes soon enough after the
first: on Windows, within the double-click time in its mouse settings, as in
other Windows apps; elsewhere, within 0.3 seconds.
