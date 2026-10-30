# Routing

Each track's sound goes to the master, or to a group track that feeds it,
and its sends take it to return tracks too. Beyond that, a track can take
its input from another track, to record it or to play it through devices
of its own, a MIDI track can play and record another's notes, a track can
resample the master, a MIDI track can play an instrument on another, and a
track can be heard through its sends alone. A key from one track into a
device on another is a [sidechain](sidechains.md).
The [Routing panel](../window/routing.md) lists them all, with their gains,
and changes them as the strips do.

## Taking another track's sound

An audio track's input menu, in its strip, lists the other tracks after the
audio inputs, under **Tracks**, each opening to its points, as the
Sidechain column's menu does: **Pre FX**, what comes into its devices from
its clips or its input, then **after** each of its devices, **Post FX**,
what comes out of the last of them, and **Post Fader**, after its fader and
pan. Picking one has the track take that sound as its input, and the strip
reads, say, **from Kick, Post FX**. A MIDI track's Pre FX is notes, and so
is the point after each [MIDI effect](plugins.md#midi-effects) in front of
its instrument, which an audio track can't take, so its menu starts after
the instrument.

Picking one sets the track to monitor **In**, so it plays what it takes
through its own devices, sends and fader: a copy of a vocal for a parallel
effect, say, or a group's sum through another chain. On Auto it would be
heard only while it's armed. Set it to **Off** to record it without hearing
it twice.

A point comes before its track's mute and solo, so a track taking a muted
track's sound still has it, as a key does.

### Printing a track

Arm a track that takes another's sound, and record, and it records what it
takes: from Post Fader, the same samples as the other track's stem in an
export. It lands where it played, to the sample, whatever latency the
devices on either track have, without the recording offset an interface's
input needs ([Where it lands](recording.md#where-it-lands)). Capture holds
what it takes too, while it's armed or monitors In, as it holds an input
([Capture](recording.md#capture)).

## Taking another track's notes

A MIDI track's input menu lists the other tracks with notes after the MIDI
inputs, under **Tracks**, each opening to its points that carry notes. On
a MIDI track, they're:

- **Pre FX**: its notes, from its clips and its input, with its
  controllers, such as the sustain pedal.
- **after** each of its [MIDI effects](plugins.md#midi-effects): the notes
  that one plays, such as an arpeggiator's, or after one that's bypassed or
  deactivated, the notes that come into it.
- **Post FX**: the notes that reach its instrument, after all its MIDI
  effects.

And on any track, the point **after** a [plugin that plays notes
out](plugins.md#notes-a-plugin-plays-out) carries the notes it plays, such
as a synth's own arpeggios, or the notes a pitch tracker hears in a vocal.
The track taking them plays them at the same moment the plugin does.

Picking one has the track play those notes as it would a keyboard's, along
with its own clips', and the strip reads, say, **from Lead, after Arp**. It
monitors **In**, as a track taking sound does, so a second instrument can
double a part, arpeggio and all. A track without an instrument can still
give its notes to one that has, and like sound, notes come before their
track's mute and solo.

### Recording the notes

Arm the track and record, and it records the notes it takes. The other
track's clips' notes land exactly where they are, with their velocities and
the controllers' moves, as a clip of its own you can edit, and so do the
notes a MIDI effect plays on them, and those a plugin plays out: recording
what an arpeggiator plays turns the arpeggio into notes you can edit. Notes played
live on the other track land where Musicable played them, a moment after
that track's own take would have them: as long as monitoring them took.
Capture holds them too, while the track is armed or monitors In.

## Playing another track's instrument

A MIDI track without an instrument has **MIDI To** in its strip, where its
output would be: it plays the track's notes on an instrument on another
track. Its menu lists **No Output**, then the instruments on the other
tracks, by their tracks' names, each opening to the MIDI channels: **Any
Channel**, then **Channel 1** to **Channel 16**. Picking one plays the
track's notes, after its [MIDI effects](plugins.md#midi-effects), on that
instrument, along with the notes of the instrument's own track, and the
strip reads, say, **Strings 3**.
**No Output** stops it.

- **Any Channel** plays each note on its own, as the instrument's own
  track's notes play, with its own pitch bend and expression.
- **A channel** plays every note on it, as a keyboard set to that channel
  would. That's how a **multi-timbral** instrument, such as a sampler with
  a part on each MIDI channel, plays a part for each track: four tracks on
  channels 1 to 4 play its four parts. Musicable's own instruments play
  every channel alike.

The instrument's track's modulators hear the notes too, so an Envelope
there starts on each of them. Unlike notes taken as an input, MIDI To
follows the track's mute and solo, as its sound would: a muted track plays
nothing on the instrument, and soloing it plays its notes on the
instrument's track, with the rest quiet. Soloing the instrument's track
plays them as well.

Choosing MIDI To, or another channel, is a step in the undo history, **MIDI
To**, and the [Routing panel](../window/routing.md) lists each under
**Notes**, with its channel. A track wired into several instruments, by the
`wire.connect` command, reads, say, **2 Instruments**, and picking one
plays that one alone.

## Resampling

**Resampling**, at the end of an audio track's input menu, records the
master: the whole mix, after the master's fader. The track never plays it.
It plays its own clips as any track does, and while it records, nothing at
all, from the first frame, so it isn't in what it records: not through its
sends, nor through a track that takes its sound, nor in a reverb tail of
its own. Only a track that takes the point after one of its devices still
has that device's tail. Its monitoring stays as it was.

Capture takes the master as it was heard, so if the resampling track's own
clips played then, they're in the capture.

## Sends Only

**Sends Only**, at the end of a track's output menu, feeds nothing: the
track is heard only through its sends, and through the tracks that take its
sound. A ghost kick that keys a bass through a compressor, or a synth that
only feeds a vocoder, goes there. Its meter still shows what it would feed.
A return track can go to Sends Only too, to be heard only through the tracks
that take its sound.

## Loops

Signal never goes round in a loop. A track can't take its input from a
track its own sound reaches: a group it feeds, a return it sends to, or a
track that takes its sound already, and two MIDI tracks can't take each
other's notes. The input menu shows those greyed out. Resampling never
makes one, since the track plays nothing while it records.

A move, an output, a send or a wire that would close a loop through a
track's input is refused, with a message naming the input in the way, such
as "That would make a loop, through Print's input from Kick Post FX."

## Undoing and deleting

Choosing an input is a step in the undo history, **Input**, with the
monitoring it sets in the same step. Deleting a track, or a device whose
point another track takes, sets that track's input to **No Input** in the
same step, so undoing brings it back. So does moving such a device onto the
master, as the one point on the master a track can take is its Post Fader,
for [Resampling](#resampling).
