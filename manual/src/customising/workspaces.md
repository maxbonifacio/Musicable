# Workspaces

A workspace is a layout of the panels, in the main window and in windows of
their own, with where each window is: its corner, its size, and whether
it's maximized. It keeps the parts the transport bar, the track headers and
the mixer strips show, too ([below](#hiding-and-moving-parts)). What you
change goes into the workspace in use as you change it, so the windows open
as you left them. Switching workspaces closes the windows of the one you
leave and opens those of the one you switch to.

## The Workspaces menu

The **Workspaces** menu lists your workspaces, up to nine, with a tick on
the one in use:

- **Alt and a workspace's number** switch to it, Alt+1 being Default. A new
  workspace takes the lowest number free, and keeps it as others come and
  go.
- **Save Workspace As...** keeps the layout and the views' parts as a new
  workspace, under the name you type, and uses it from then on; the
  workspace you were in keeps what it had. A name can't be another
  workspace's, in any letters, or have characters a file name can't.
- **Rename Workspace...** and **Delete Workspace...** rename or delete the
  workspace in use. Deleting asks first, then switches to Default. Default
  keeps its name and can't be deleted.
- **Reset Workspace** lays the panels out and shows the views' parts as at
  first, with every panel back in the main window, which stays where it is.

The Settings window's **Workspaces** page lists them too, with the keys that
switch to each and the same commands, and **Open Workspaces Folder** shows
their files.

The zoom and the scrolling are the project's: switching workspaces leaves
them as they are. A project remembers the workspace it was saved with, and
opening it switches there, if you have a workspace by that name. A window
goes back to the monitor it was on, if that monitor's still there. If not,
the main window stays on the one it's on, and a window of its own opens in
the middle of the main monitor. A window you move or resize is saved once
it's stayed put for a second.

## Hiding and moving parts

The transport bar, the track headers and the mixer strips are made of
parts, and each part can be hidden, shown again and moved, and some
resized:

- **Right-click a part** for **Hide** with the part's name, and
  **Customise...** for its view. A control with a menu of its own, such as a
  track's header or a send, has them at the end of its menu. Right-clicking
  between the parts of the transport bar or of a strip has Customise alone.
- **The Customise window** lists the view's parts in order. Tick a part to
  show it or untick it to hide it, drag it by its handle to move it, and
  give a size in points to those that can take one: the width of the CPU
  meter, and of the headers' colour and meter. **Reset** shows the view as
  Musicable does at first, and Escape closes the window.

| View | Its parts, in the order they show at first |
| --- | --- |
| The transport bar (`transport_bar`) | Go to Start (`go_to_start`) · Play and Stop (`play_stop`) · Record (`record`) · Capture (`capture`) · Loop and Punch (`loop_punch`) · Metronome (`metronome`) · Tempo (`tempo`) · Time Signature (`signature`) · Position (`position`) · Flexible Space (`space`) · CPU Meter (`cpu`) |
| The track headers (`track_header`) | Colour (`colour`) · Name (`name`) · Automation Button, A (`automation`) · Mute, Solo and Arm (`buttons`) · Meter (`meter`) |
| The mixer strips (`mixer_strip`) | Name (`name`) · Input (`input`) · Monitoring (`monitor`) · Output (`output`) · Sends (`sends`) · Pan (`pan`) · Fader and Meter (`fader`) · Gain and Peak (`readouts`) · Mute, Solo and Arm (`buttons`) · Clip Type (`clip_type`) |

The names in brackets are what the workspace file calls them.

The transport bar's Flexible Space takes the room the other parts leave,
which keeps the parts after it at the bar's right end. Across a header the
name takes what's left, and down a strip the fader does, so a hidden part
gives the fader its room. Each part takes as many rows in every strip, so
the faders line up even where a track has no input or sends, and return and
group tracks keep M and S where the other tracks have them, with an empty
place for the arm button.

## Workspace files

The `workspaces` folder in Musicable's config folder
([Settings](settings.md#the-config-folder)) holds a file for each workspace,
named after it, such as `Mixing.toml`: `Default.toml` once you've changed
Default. The workspace in use is written whenever you change its layout or
its views' parts, once you let go of the tab, the edge or the part you're
dragging, and the one Musicable opens with is read as it starts:

```toml
format = "musicable-workspace"
version = 1
slot = 2

[views.mixer_strip]
hidden = ["monitor"]
parts = ["name", "output", "input", "monitor", "sends", "pan", "fader", "readouts", "buttons", "clip_type"]

[views.track_header.sizes]
meter = 14

[[windows]]
at = [-1912, 120]
size = [1600, 900]
maximized = true
hidden = ["browser", "piano_roll"]
layout = { across = [
    { panel = "browser" },
    { down = [
        { panel = "arrangement" },
        { panel = "mixer", size = 372 },
        { tabs = ["devices", "piano_roll"], front = "devices" },
    ] },
] }

[[windows]]
at = [3880, 200]
size = [480, 900]
layout = { panel = "browser" }
```

- `slot` is the workspace's number in the Workspaces menu, which Alt and the
  number switch to: a file without one, or with one another file has, takes
  the lowest free.
- A window's `at` is where its top left corner is on the desktop, in pixels,
  which a monitor left of the main one makes negative, and `size` how big
  its inside is, in pixels at 100% scaling, which its monitor's scaling
  makes bigger. While it's `maximized`, they're where it goes back to.
- A part's `size` is its width in a row of parts (`across`) or its height in
  a column (`down`), in points; parts without one take their panels' default
  sizes, and the part with the arrangement takes what's left.
- `hidden` lists the panels hidden, which keep their place.
- The windows after the first are windows of their own, laid out the same
  way, with the panels moved out of the main window, which has them hidden,
  keeping their place there. A panel in two windows goes in the first, and
  the arrangement only in the main window.
- `views` has only the views you changed, by the names in the table
  [above](#hiding-and-moving-parts). A view's `parts` are its parts in
  order, hidden ones too, `hidden` lists those hidden, and `sizes` the sizes
  given in the Customise window, in points. Parts the file doesn't name go
  after the part before them in the first order, and parts Musicable doesn't
  have are left out.

The panels are `arrangement`, `mixer`, `devices`, `piano_roll`, `browser`
and `history`. Panels the file doesn't have go where they go at first,
hidden, and a file that can't be used leaves the layout as it was, and says
why. Deleting Default's file brings back the first layout.
