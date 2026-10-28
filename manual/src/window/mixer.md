# The mixer

Each track has a channel strip, in the project's order, and the master's
strip is on the right. Tracks scroll sideways when they don't fit. The
**Track** menu adds tracks: audio, MIDI, return and group tracks.

## A strip

From the top, a strip has these parts, which can be hidden and moved
([Workspaces](../customising/workspaces.md#hiding-and-moving-parts)):

- **The name.** Clicking it selects the track, as clicking its header in the
  arrangement does, and so does clicking the strip where it has no control:
  Ctrl-click or Shift-click adds the track to the selection or takes it out,
  a double-click renames it, and the master's selects the master, for the
  devices panel to show its devices. A selected track's strip is lit, as its
  header is. Hovering the name shows the track's kind and test tone.
  Right-clicking it has **Solo-Safe** and **Delete Track**.
- **The input**, on audio and MIDI tracks: what the track records from
  ([Recording and comping](../using/recording.md#inputs)), and on an audio
  track, another track's sound or Resampling, the master
  ([Routing](../using/routing.md)).
- **Monitoring**, on audio and MIDI tracks: **In**, **Auto** or **Off**
  ([Recording and comping](../using/recording.md#monitoring)).
- **The output**: the master, a group track the track can feed, or **Sends
  Only**, nothing but its sends ([Routing](../using/routing.md#sends-only)).
  Return tracks feed the master or Sends Only.
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
  Ctrl-click solos it along with the others.
- **The clip stage**: Off, Hard or Soft.

## Moving the controls

Drag a fader, a send or the pan to move it, with Ctrl held to move it
finely. Double-click a fader for 0 dB, a send to silence it, or the pan to
centre it. A whole drag is one step in the undo history. The Settings
window's Mouse page changes what the gestures do
([The mouse](../customising/mouse.md)).

A fader, a send or a pan with automation follows it, showing its value at
the playhead, and has a red dot at its top left. Right-clicking one of them
has **Show Automation**, which shows its lane in the arrangement,
**Re-Enable Automation** and **Clear Automation**
([Automation](../using/automation.md)).

Modulators move faders, sends and pans too. Drag a modulator's handle onto
one of them to have the modulator move it, or right-click it for the
routings that move it, and **Add Modulation**. One that's modulated has a
green line, beside a fader or along the foot of a send or a pan, from as low
to as high as its modulators can take it, and a dot where they have it
([Modulation](../using/modulation.md#routing-an-output)).

## The meters

Each strip's meter shows its channel's output, after mute and solo, left and
right: the peak in a light shade, and the RMS level, averaged over about
0.3 seconds, solid. They're green up to -6 dB, yellow to 0 dB and red over.
The peak falls back at 20 dB a second once the signal drops.

On a Mac, Cmd takes Ctrl's place in the clicks and drags on this page.
