# Sampler

Plays a sample at each note's pitch. **Load Sample...** picks a WAV, AIFF
or FLAC file for it, or drag a sample from the browser onto it. The devices
panel shows the file's name beside the sampler's: right-click the name to
remove the sample. If the file is in the project's folder, the project
keeps where it is in the folder, so the folder can move. A file that can't
be read leaves the sampler silent, and the status bar says why. Loading and
removing a sample are steps in the undo history.

A note plays the sample from **Start** to **End**, higher or lower by the
note's distance from the **Root**, plus **Transpose** and **Detune** and
the note's pitch bend. With **Loop** on, it plays on from the end back to
the **Loop Start** until its sound ends, crossfading over the **Loop Fade**
so the loop doesn't click. Start, end and loop points are percentages of the
sample's length.

In Classic **Mode**, a note fades out over the amp's **Release** when it's
let go. One-Shot plays from start to end however short the note is, which
suits drums. **Velocity** sets how far a note's velocity turns its level
down from full, and the **Filter** is off until you pick its type. A note
past the number of **Voices** takes the place of the voice let go longest
ago.

## Controls

{{#include ../generated/devices/sampler.md}}
