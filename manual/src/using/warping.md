# Warping

An audio clip plays its file at the file's own speed, whatever the song's
tempo, until it's warped. A warped clip follows the tempo: its audio is
placed on beats, so it stays in time when the tempo changes, and a loop
recorded at one tempo lines up with a song at another. Turn it on with
**Warp** in the [Sample Editor](../window/sample-editor.md), or
**Clip > Warp** for the clip the Sample Editor shows.

## Turning Warp on

Warp starts the clip with one warp marker, at its start, and a tempo for its
audio: the tempo the file's loop data gives, as in a WAV file made for loops,
or else a tempo in its name, such as "Drums 120 BPM.wav", or a number in its
name that the file is a whole number of beats long at, as in
"Bass_Am_128.wav", or else the song's tempo where the clip starts. Warp
doesn't use the tempo Musicable finds in a file's audio yet, so a file
without loop data or a tempo in its name may need its tempo set by hand.

The clip keeps the stretch of the file it plays, as in Live, so it gets
longer or shorter if the tempo it starts at isn't the song's, covering what
it grows into on its track, as trimming does. Turning Warp off does the
same the other way, and forgets the clip's markers.

## The clip's tempo

The tempo field says how fast the clip's audio goes: with a marker or two,
from the first marker to the last. Changing it makes the clip play its audio
faster or slower, over fewer or more beats, and the clip grows or shrinks
from its start. **×2** doubles it, playing the audio half as fast, and
**÷2** halves it, as in Live. In the arrangement, **Shift-dragging a warped
clip's edge** stretches it, changing its tempo so its audio fills it:
dragging its end keeps its start where it is, and dragging its start keeps
its end.

## Warp markers

Each warp marker pins a moment of the file to a beat of the clip, and the
audio between two markers stretches to fill the beats between them. Before
the first marker and after the last, the audio plays at the speed next to
it. To line a loop up with the song:

1. Turn Warp on, and set the clip's tempo if it isn't right.
2. Double-click the waveform's top half where a beat of the audio is, such
   as a drum hit, to put a marker there.
3. Drag the marker onto the beat it should be on. The audio before and
   after it stretches to follow.

Shift-dragging a marker moves the audio under it instead, so another moment
of the file plays on that beat. [The Sample Editor](../window/sample-editor.md#the-mouse)
has the rest of the gestures and the keys.

## Transients

Musicable finds the transients in the project's audio files, the moments
where their sounds start, such as each drum hit, as it analyses them in the
background. A warped clip shows its file's transients as ticks along the top
of its waveform in the Sample Editor, and keeps them in the project, with
the changes you make to them.

Pointing at a transient in the waveform's top half shows a pseudo marker
there, a faint warp marker. **Drag it** to make it a warp marker and move
it, so the hit lands on another beat, or **double-click it** to pin the hit
where it plays. To put markers on many hits at once, drag across the
waveform to select a stretch of the clip and press **Ctrl+I**, which puts a
warp marker on each transient in it.

Analysis can miss a soft sound or find one too many in a long one, so the
transients can be changed:

- **Shift-drag a transient** to move it, between the transients either side.
- **Ctrl+Shift+I** adds one at the start marker, where the waveform was
  clicked last.
- **Ctrl+Shift+Backspace** deletes those in the time selected.
- **Reset Transients**, on the right-click menu, brings back the ones
  analysis found, without yours.

A clip warped before Musicable has analysed its file takes the transients
once it has, and so does a clip in a project saved by an earlier version.

## Re-Pitch

Re-Pitch, the one mode so far, plays the audio faster or slower to fit the
beats, so its pitch follows its speed, as a record's does on a turntable:
twice as fast is an octave up. It's how Live's Re-Pitch mode plays, and it
sounds the same while the song plays and in an export.

## Not yet

These come in later versions:

- The other modes, Slice, Granular and Spectral, which keep the audio's
  pitch as its speed changes, and the clip's Transpose and Detune.
- Warp putting markers on a file's beats, as Live's auto-warp does.
  Musicable already finds the tempo and the beats of the project's audio
  files in the background, with their transients, and the status bar says
  so while it does, but Warp doesn't use the tempo and the beats yet.
- Recordings that follow the tempo: a clip recorded into a song plays
  unwarped, at its own speed.
- The tempo in AIFF files' loop data, as Apple Loops have it: only WAV
  files' is read.
