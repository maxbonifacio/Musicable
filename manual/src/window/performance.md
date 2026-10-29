# The Performance panel

The Performance panel lists what the project costs the CPU, busiest first:
each device, and each track's clips, modulators and mixer, with the share
of a core it took over the last second and its longest block. **View >
Performance** (Ctrl+Alt+C) shows it, in a tab beside the mixer at first.
It's where to look when the CPU meter climbs or dropouts start, for the
plugin to swap, freeze or take out.

## What the figures say

- **Average** is the share of one core the device needs to keep up: 12% is
  an eighth of a core, and 100% a whole one. Shares add up: a track's is
  its devices' and the rest of its strip's, and the **Total** row's is the
  whole project's, so 340% keeps three and a half cores busy. Musicable
  renders on all the cores, so a total over 100% is fine while there are
  cores for it.
- **Longest** is the device's longest block over the last second, as a
  share of the audio device's buffer. Each buffer has to be ready before
  the audio device plays it, so a device that takes 60% of a buffer by
  itself leaves the rest of the project little time, whatever its average.
  Over 100%, it alone takes longer than a buffer lasts.
- An average turns amber over 25% of a core, and red over 50%.

The figures show while an audio device plays, whether the song plays or
not. They count the time each device takes on whichever of Musicable's
threads runs it, and the work a CLAP plugin shares out on them, but not the
threads some plugins start for themselves, nor the time the audio driver
and other apps take, which the CPU meter's load
([Setting up audio](../getting-started/audio.md#the-cpu-meter-and-the-dropouts))
does count.

## The rows

- **A device**: its track or the master, and its name.
- **Clips**: what a track plays, its clips and the input it monitors.
- **Modulators**: a track's or the master's modulators.
- **Mixer**: the rest of a track's or the master's strip: its fader and
  pan, its sends, its bus, the wires into its devices' side inputs, and the
  delays that line it up with the other tracks.
- **Metronome**.

**Clicking a row** selects its device, which the devices panel brings
forward, scrolled to it, or else its track or the master. While the pointer
is over the list, the rows keep their places, so a click lands on the row
it was aimed at, and when it leaves, the busiest goes to the top again.

## Elsewhere

- Each device's average is in its title bar in the devices panel, with its
  longest block in its tooltip
  ([The devices panel](devices-panel.md#adding-devices)).
- Each track's, its devices' and its strip's together, is in the mixer's
  **CPU** row ([The mixer](mixer.md#a-strip)).
- The CPU meter's tooltip, in the transport bar, has the project's total.
- Agents read the rows with `get_performance`
  ([The command line](../reference/command-line.md#agents)).

The Performance panel docks, moves to a window of its own and goes in
workspaces as the other panels do ([The window](index.md#the-panels)).
