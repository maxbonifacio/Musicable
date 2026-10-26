# Warping

An audio clip plays its file at the file's own speed, whatever the song's
tempo, until it's warped. A warped clip follows the tempo: its audio is
placed on beats, so it stays in time when the tempo changes, and a loop
recorded at one tempo lines up with a song at another. Audio files dropped
into the song are warped as they come in, by what Musicable finds in them
([Auto-warp](#auto-warp)), and a clip can be warped by hand with **Warp** in
the [Sample Editor](../window/sample-editor.md), or **Clip > Warp** for the
clip the Sample Editor shows.

## Auto-warp

Musicable analyses the project's audio files in the background, finding
their tempo, their beats and downbeats, and their transients, and the status
bar says so while it does. An audio file dropped into the song, from the
browser or elsewhere, or imported, is warped by the rule for its kind of
sound:

- **A loop** of whole bars at a steady tempo gets one warp marker at its
  start, at the tempo its length gives, so it fills its bars exactly.
- **A file at a steady tempo**, such as a bounced song or a stem, gets one
  marker on its first downbeat, at its tempo, and that downbeat lands on the
  bar line nearest where the file was dropped, with any pickup before it.
- **A file whose tempo drifts**, such as a band recorded without a click,
  gets a marker on every downbeat, and on each beat between them that
  strays, so it locks to the grid but keeps its feel.
- **A sound without a pulse**, such as a one-shot, an effect or speech,
  stays unwarped, and plays its first hit, or the peak a riser builds to,
  where it was dropped.

A file's tempo is read at the octave nearest the song's, so it stretches
least: an 85 BPM loop in a 170 BPM song plays at its own speed, as
half-time. The first file with a pulse dropped into an empty song gives the
song its tempo and meter, rather than being stretched.

A file dropped before Musicable has analysed it comes in as it is, and is
warped once its analysis is ready, unless you've changed the clip since.
That's part of the drop in the undo history while the drop is the last step,
and else a step of its own. Where analysis is unsure of a file, as when the
beat slips, the file comes in as it is: the Sample Editor shows the best
guess at its tempo, greyed out, and **Warp** warps it at that tempo.

Each rule can be switched off on the Behaviour page of the Settings window,
and so can [recordings](#recordings) following the tempo and the first file
setting it ([Settings](../customising/settings.md#behaviour)).

## Turning Warp on

Warp warps the clip as auto-warp's rule for its file says, where analysis is
sure of the file and the rule is switched on. Otherwise it starts the clip
with one warp marker, at its start, at the tempo analysis found in the file,
read at the octave nearest the song's. Where the file's loop data or name
gives a tempo near that, at any octave, it takes that one instead: the
tempo in a WAV file made for loops, or else a tempo in its name, such as
"Drums 120 BPM.wav", or a number in its name that the file is a whole
number of beats long at, as in "Bass_Am_128.wav". Before the file's been
analysed, it takes the tempo its loop data or name gives, or else the
song's tempo where the clip starts.

The clip keeps the stretch of the file it plays, as in Live, so it gets
longer or shorter if the tempo it starts at isn't the song's, covering what
it grows into on its track, as trimming does. Turning Warp off does the
same the other way, and forgets the clip's markers and its mode and
settings: turned on again, it starts in Re-Pitch.

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

## Quantize

**Quantize** (Ctrl+U) moves the hits in warped audio onto the grid, as it
moves notes in the piano roll. In the Sample Editor, it moves the transients
in the time selected, or all those the clip plays if none are; in the
arrangement, all those of the selected warped clips, and the notes of the
selected MIDI clips. Each transient gets a warp marker where it plays, or
keeps the one on it, and the marker moves to the nearest line of the grid,
as the panel shows it at its zoom, stretching the audio either side.

The transient nearest its line goes first. One that would then land on a
line another took, as the second hit of a flam would, or pass a marker on
another transient, stays where it plays. A marker in a transient's way that
isn't on a transient goes, as the marker at the start of a recording does
when its first hit was late. Quantize is on the Edit menu and the
arrangement's right-click menu for clips, and on the Sample Editor's, where
it moves the transient it was opened on.

## Warping again from a point

Right-clicking the waveform, a marker or a transient's pseudo marker opens
a menu with Live's warp commands, which act at that point:

- **Set 1.1.1 Here** makes the point the clip's first downbeat: the clip's
  bars count from there, and the clip moves so the point plays on the bar
  line nearest where it played.
- **Warp From Here** warps the rest of the clip again by analysis, from the
  point to its end, keeping the markers before it, as auto-warp would warp
  the file. **Warp From Here (Start at Song Tempo)** reads the file's tempo
  at the octave nearest the song's, rather than the clip's.
- **Warp From Here (Straight)** plays the rest of the clip at one tempo, the
  file's, or the clip's there before the file's been analysed, and **Warp
  at Song Tempo From Here** at the song's tempo there.
- **Warp Selection as a Loop**, in the time selected, makes the clip that
  stretch, fitted into the nearest whole number of bars from the bar line
  nearest its start, as a loop. It's on the Clip menu too.

Each keeps the clip's curves with its audio, and the clip covers what it
grows into on its track, as trimming does.

## Recordings

A clip recorded into the song follows the tempo it was played to, sample
for sample, so it stays in time when the song's tempo changes later, even
where the tempo changed while it was played. **Recordings follow the
tempo**, on the Behaviour page of the Settings window, switches that off,
for recordings that play at their own speed.

## Modes

The clip's mode, the menu after **Warp** in the Sample Editor, says how its
audio plays between the warp markers. Changing it keeps the markers, and the
clip keeps each mode's settings while it plays in another, as in Live. Every
mode sounds the same while the song plays and in an export: Spectral once
it has rendered the clip ([below](#spectral)).

### Re-Pitch

Re-Pitch plays the audio faster or slower to fit the beats, so its pitch
follows its speed, as a record's does on a turntable: twice as fast is an
octave up. It's how Live's Re-Pitch mode plays. As its pitch is its speed's,
its Transpose and Detune are greyed out.

### Slice

Slice is for drums and other percussive loops, as Live's Beats mode is. It
cuts the audio into slices, at its transients, and plays each slice at the
file's own speed from where its start lands on the beats, so each hit keeps
its sound and its pitch at any tempo. Where the song plays the file faster
than its own speed, each slice is cut short where the next starts, with a
fade of ten milliseconds so the cut doesn't click. Where it plays slower, a
slice's audio runs out before the next one starts, and Loop says what fills
the gap. A slice that runs out stops five milliseconds short of the next
one, so neither its end nor its loop plays the start of the next hit. At
the file's own speed, with no transpose and no envelope, Slice plays the
file sample for sample.

Its settings are under the mode in the Sample Editor:

- **Preserve** says where Slice cuts the audio: at its **Transients**, or on
  a grid of notes from the clip's 1.1.1, whatever the audio: **1 Bar**, as
  long as a bar of the song's time signature where the clip starts, **1/2**,
  **1/4**, **1/8**, **1/16** or **1/32**. Until Musicable has analysed the
  file, Transients cuts on a grid of sixteenths.
- **Loop**, Live's Transient Loop Mode, says what a slice plays once its
  audio runs out: **Off**, silence until the next slice, **Forward**, the
  end of the slice over and over, or **Back-and-Forth**, the end of the
  slice backwards and forwards. The end it loops is the slice's second
  half, or its last half second in a longer slice. Each time round, and
  at each turn, a loop crossfades over ten milliseconds, or less in a
  short slice, so a kick's tail or a bass under it doesn't click. A clip
  starts on Back-and-Forth, as Live's does.
- **Envelope**, Live's Transient Envelope, fades each slice as it plays.
  At 100 it doesn't fade, and the lower it is, the quicker each slice fades
  for its length: at 60, a slice has faded to about a third by where the
  next one starts, and at 0 it's a click. A lower envelope tightens a
  loop played slower, where the slices' loops fill the gaps.

### Granular

Granular is for voices, bass lines and leads, with one note at a time, and
for pads, textures and noise, as Live's Tones and Texture modes are. It
plays the audio as short grains, each fading in as the one before fades
out, so two play at once. Each grain plays the file at its own speed from
where the song has got to in it, so the audio keeps its pitch at any tempo:
where the song plays the file slower than its own speed, the grains go over
some of its audio again, and where faster, they leave some out. Transpose
and Detune play each grain faster or slower, resampled, so they move the
pitch without changing the speed. Where the clip plays its file at the
file's own speed and isn't transposed, Granular plays the file sample for
sample.

Its settings are under the mode in the Sample Editor:

- **Follow pitch**, on as a clip starts, is Live's Tones. Where Musicable's
  analysis has found a pitch in the file, each grain is a whole number of
  its periods long and starts where one does, so the grains that overlap
  are in step and a note keeps its sound. Elsewhere, in noise and silence,
  and until the file is analysed, the grains are cut as with it off, but
  not spread. Off is Live's Texture: the grains are cut on a steady grid,
  whatever the audio, for pads, chords and noise.
- **Grain Size**, from 5 to 500 milliseconds, is how long each grain is:
  30 ms as a clip starts. Short grains follow quick notes and hits closely;
  longer ones smooth the audio into a texture, and repeat its attacks where
  the song plays it slower. Following the pitch, a grain is a whole
  number of the audio's periods long, within a period of its size, and two
  periods at least.
- **Fluctuation**, Live's Flux, shows while Follow pitch is off: how far
  each grain strays, earlier or later and reading from further back or on,
  as a share of its length, from **0%**, a steady grid, to **100%**, a
  cloud. **25%** as a clip starts. The grains stray the same way every time
  the clip plays, and in an export.

The grains are placed in the clip's own time, from its start, so splitting
or moving the clip doesn't move them, and when the song's tempo changes,
they're placed again for it.

### Spectral

Spectral is for full mixes, chords, voices and anything else with more than
one note at once, as Live's Complex mode is. It stretches the audio in the
frequency domain, with Signalsmith Stretch, so the audio keeps its pitch at
any speed without being sliced, and Transpose and Detune move its pitch
without changing its speed. Where the clip plays its file at the file's own
speed and isn't transposed, Spectral plays the file sample for sample.

Its setting, **Formants**, is under the mode in the Sample Editor. A voice's
formants are the peaks in the outline of its spectrum that give it its
character, and they stay where they are as a singer sings higher. At
**100%**, as it starts, Spectral keeps them in place as it transposes, so a
voice moved up a fourth sounds like the same voice singing higher; at
**0%** they move with the pitch, as a record's do played faster, and the
voice sounds smaller. Formants changes nothing while the clip isn't
transposed. Without High quality, it keeps the formants closest for voices
up to about 250 Hz, around B3: a higher voice keeps less of its character,
and moved up a long way, can come out a few decibels quieter.

**High quality**, the checkbox above Formants, renders the clip with Rubber
Band instead, as Live's Complex Pro mode does: the best sound, for voices
and mixes above all, and several times slower to render. With it on,
**Envelope** shows too, as Complex Pro has it: how finely Spectral follows
the outline of the spectrum that it keeps the formants by, from 8 to 512.
**128**, as it starts, suits most audio. A high voice, whose harmonics are
far apart, can sound better lower, and a low one higher. Envelope changes
nothing while the formants move with the pitch: with the clip not
transposed, or Formants at 0%. In High quality, a hit in audio played
slower comes out a little early, by about 3 ms where it plays half as long
again, and 7 ms where it plays twice as long, as Rubber Band stretches it.

Spectral's stretcher remembers what it has just played, so a clip in
Spectral plays audio rendered ahead of time, in segments of 4 to 8 seconds
of its file, cut at the strongest hits. They render in the background,
those the playhead plays next first, whenever the clip's mode, markers,
tempo, pitch or curves change, or the song's tempo, and the status bar says
how many segments are still to render. Until a segment is ready, the clip
plays a live stretch, which sounds close to it, and moves over to the
rendered audio with a short crossfade where the segment starts. The live
stretch is Signalsmith Stretch's even with High quality on, so High quality
is heard once the segments are ready, which takes longer. When the song's
tempo changes, the clip crossfades to the live stretch there and then,
without a break, until the segments at the new tempo are ready. An export
waits until every segment is rendered, so it's what the song plays once
they are, to the sample.

What's rendered is kept in the render cache, in [the cache
folder](../customising/settings.md#the-cache-folder), so the next time any
project plays a segment, it's ready. A minute of a stereo file at 44.1 kHz
takes about 21 MB. The render cache holds up to 20 GB, and always leaves
5 GB of its disk free, letting go of the segments used longest ago first;
the Files page of the Settings window changes both
([Settings](../customising/settings.md)).

## Transpose and Detune

**Transpose**, from -48 to +48 semitones (st), and **Detune**, from -50 to
+50 cents (ct), move the audio's pitch in every mode but Re-Pitch. Slice
plays each slice faster or slower to move its pitch, resampled, so a slice
an octave up plays twice as fast, for half as long. Granular does the same
with each grain, keeping the grains as long. Spectral moves the pitch in its
stretcher, keeping the audio's speed, and keeps the formants where Formants
says. Drag them in the Sample Editor, or click them and type.

## Curves in the clip

Every setting of a warped clip but its mode, Follow pitch and High quality
can follow a curve, as Live's clip envelopes can move its volume and
transpose: its gain, its Transpose and Detune, Slice's Envelope, Preserve
and Loop, Granular's Grain Size and Fluctuation, and Spectral's Formants and
Envelope. The lane under the Sample Editor's
waveform shows one of the curves at a time: click its name, beside it, to
choose which. A curve is part of the clip, in the clip's own time, so it
moves, copies and splits with the clip and stays with its audio.

Where a setting has a curve, the curve gives its value all through the
clip: the first point's value before the first point, and the last's after
the last. The gain follows its curve smoothly, and Slice reads the others
where each slice starts, so a change in the middle of a slice waits for the
next one. Granular reads them where each grain is centred, so they move a
grain at a time. Spectral renders the curves of Transpose, Detune, Formants and
Envelope into its segments, reading them every 30 milliseconds, or about
every 5 with High quality, so a curve redrawn renders again only the
segments it crosses. Preserve and Loop are steps: each point holds its
value until the next, and a curve of Preserve says how the audio is cut
from each point on.

## Not yet

These come in later versions:

- Quantize Settings, for moving hits part of the way to the grid, or to a
  grid of their own: Quantize goes all the way, to the grid the panel shows.
- The tempo in AIFF files' loop data, as Apple Loops have it: only WAV
  files' is read.
