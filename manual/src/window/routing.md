# The Routing panel

The Routing panel lists every connection in the project, a row each: each
track's output, the sends, the inputs tracks take from other tracks, the
keys wired into devices' side inputs, and the notes tracks play on other
tracks' instruments ([Routing](../using/routing.md),
[Sidechains](../using/sidechains.md)). Live shows a track's routing in its
mixer's In/Out section; here the whole project's is in one list. **View >
Routing** (Ctrl+Alt+I, Live's key for In/Out) shows it, in a tab beside the
mixer at first.

## The rows

A row has where the connection comes from, an arrow, where it goes, and its
gain. The rows go under the kind of connection they are, in the order the
tracks are in, and a kind without any has no heading:

- **Outputs**: each track's output, to the master, a group track or Sends
  Only, with the track's fader.
- **Sends**: each send to a return track, with its level and its **Pre**
  switch.
- **Inputs**: each track that takes another track's sound or notes, or
  resamples the master, from the point it takes.
- **Keys**: each source of a device's side input, from the point it takes,
  with its gain, in the order the devices are in.
- **Notes**: each track's [MIDI To](../using/routing.md#playing-another-tracks-instrument),
  from the point whose notes it plays to the instrument, with the MIDI
  channel it plays them on, in the order the instruments are in.

**Show**, over the rows, shows every track's connections, or one track's
or the master's alone: those that come from it, and those that go to it or
to one of its devices.

## Showing and changing them

- **Clicking a name**, at either end of a row, shows it: a track or the
  master is selected, as clicking its strip selects it, and a device is
  selected in the devices panel, which comes forward scrolled to it. Sends
  Only isn't anywhere, so it isn't a link.
- **The button beside the end a connection picks** opens the menu of the
  control it comes from: the output menu's buses beside where an output
  goes, and beside where an input comes from, **No Input** and the input
  menu's other tracks and Resampling. The strip's input menu has the
  interfaces' inputs, which take a track out of the list. Beside where a key
  comes from is the Sidechain column's menu of sources, and beside where
  notes come from, the same menu of the points that carry notes.
- **A note row's channel**, in place of a gain, is a menu: **Any Channel**,
  each note on its own, or **Channel 1** to **Channel 16**.
- **The gains** move as their controls do, by dragging, with Ctrl held to
  move finely, a whole drag being one step in the undo history. An output's
  fader goes from silence on the left to +6 dB on the right, with a mark at
  0 dB, and a double-click sets 0 dB. A send goes up to 0 dB, and a
  double-click silences it. A key's gain is the Sidechain column's bar.
- **Right-clicking a gain** has its control's menu: **Show Automation** and
  the other automation commands, and the modulators that move it. A send's
  has **Remove Send** too, and a key's a field to type its gain in, as any
  bar's has.

The Routing panel docks, moves to a window of its own and goes in
workspaces as the other panels do ([The window](index.md#the-panels)).
