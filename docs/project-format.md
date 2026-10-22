# Project files

Musicable saves each project as one JSON file with the extension
`.musicable`. The format is open: this page describes all of it, so other
tools can read and write projects too. `crates/io` implements it, in
`musicable_io::project`.

## Example

A project with a bass track playing a recording and sending to a reverb on
a return track, and a keys track playing a note through the test
instrument, bending it up a tone. It loops the first four bars, with the
master's soft clip stage on, and a locator marks the chorus at bar 3:

```json
{
  "format": "musicable-project",
  "version": 8,
  "ticks_per_beat": 322560,
  "tempo": {
    "tempos": [
      {
        "at": 0,
        "bpm": 120.0
      }
    ],
    "signatures": [
      {
        "bar": 0,
        "numerator": 4,
        "denominator": 4
      }
    ]
  },
  "loop_region": {
    "start": 0,
    "end": 5160960
  },
  "looping": true,
  "metronome": true,
  "punch_in": false,
  "punch_out": false,
  "next_track_id": 3,
  "next_send_id": 1,
  "next_device_id": 1,
  "next_clip_id": 2,
  "next_locator_id": 1,
  "next_take_id": 0,
  "tracks": [
    {
      "id": 0,
      "name": "Bass",
      "kind": "audio",
      "gain": 0.8,
      "pan": -0.25,
      "muted": false,
      "solo": false,
      "solo_safe": false,
      "armed": false,
      "input": {
        "audio": {
          "device": "",
          "name": "",
          "channel": 0,
          "stereo": true
        }
      },
      "monitor": "auto",
      "output": null,
      "sends": [
        {
          "id": 0,
          "to": 1,
          "level": 0.5,
          "pre_fader": false
        }
      ],
      "tone": null,
      "clips": [
        {
          "id": 0,
          "name": "Bass",
          "start": 0,
          "length": 5160960,
          "content": {
            "audio": {
              "file": "Audio/Bass.wav",
              "offset": 0,
              "gain": 1.0,
              "fade_in": 0,
              "fade_out": 80640
            }
          }
        }
      ],
      "takes": [],
      "devices": [],
      "clip_type": "off",
      "console": {}
    },
    {
      "id": 1,
      "name": "Reverb",
      "kind": "return",
      "gain": 1.0,
      "pan": 0.0,
      "muted": false,
      "solo": false,
      "solo_safe": true,
      "armed": false,
      "input": null,
      "monitor": "auto",
      "output": null,
      "sends": [],
      "tone": null,
      "clips": [],
      "takes": [],
      "devices": [],
      "clip_type": "off",
      "console": {}
    },
    {
      "id": 2,
      "name": "Keys",
      "kind": "midi",
      "gain": 1.0,
      "pan": 0.0,
      "muted": false,
      "solo": false,
      "solo_safe": false,
      "armed": false,
      "input": {
        "midi": {
          "device": "",
          "name": "",
          "channel": null
        }
      },
      "monitor": "auto",
      "output": null,
      "sends": [],
      "tone": null,
      "clips": [
        {
          "id": 1,
          "name": "Chord",
          "start": 1290240,
          "length": 1290240,
          "content": {
            "midi": {
              "offset": 0,
              "next_note_id": 1,
              "notes": [
                {
                  "id": 0,
                  "start": 0,
                  "length": 645120,
                  "key": 60,
                  "velocity": 0.8,
                  "release_velocity": 0.5,
                  "expression": {
                    "pitch_bend": [
                      [0, 0.0],
                      [322560, 2.0]
                    ],
                    "pressure": [],
                    "timbre": []
                  }
                }
              ],
              "controllers": [
                {
                  "controller": 64,
                  "points": [
                    [0, 1.0],
                    [967680, 0.0]
                  ]
                }
              ]
            }
          }
        }
      ],
      "takes": [],
      "devices": [
        {
          "id": 0,
          "kind": {
            "test_instrument": {
              "level": 0.5
            }
          }
        }
      ],
      "clip_type": "off",
      "console": {}
    }
  ],
  "master": {
    "gain": 1.0,
    "pan": 0.0,
    "devices": [],
    "clip_type": "soft",
    "console": {}
  },
  "locators": [
    {
      "id": 0,
      "name": "Chorus",
      "at": 2580480
    }
  ]
}
```

`crates/io/tests/fixtures/` has more, one folder for each version of the
format.

## Rules

- A project file is UTF-8 JSON. A byte order mark at the start is ignored.
- `format` is always `"musicable-project"`, and `version` says which version
  of the format the file uses. Musicable reads its own version and every
  older one, upgrading old files as it loads them. It refuses a file from a
  newer version rather than lose what it can't read. It always saves its own
  version.
- Every field below is required, once. Fields the format doesn't define are
  refused.
- Loading checks every rule on this page and names the field that breaks
  one, such as `tempo.tempos[1].bpm: must be from 20 to 999`. Musicable never
  repairs a file quietly. Saving checks the same rules, so it never writes a
  file it can't load.
- Musical positions are whole numbers of **ticks**: 322,560 to the beat, where
  a beat is a quarter note whatever the time signature. That divides by every
  tuplet up to 10 and by 1024. Positions count from the start of the song and
  can be negative, before it.
- A position is at most 9,007,199,254,740,991 ticks (2⁵³ − 1) either side of
  the start of the song, so JSON readers that use doubles, such as
  JavaScript's, read every position exactly. That's about 440 years at 120
  BPM.
- Other numbers are whole numbers or floats as the tables say. Floats must be
  finite, since JSON has no NaN or infinity.

**Same project, same bytes.** Musicable writes the fields in the order the
tables list them, a field or an item to a line, indents with two spaces,
ends every line with `\n`, and writes each float in the shortest form that
reads back as exactly the same number. Arrays in arrays, such as the points
of an expression curve, take a line each. So saving an unchanged project gives the same bytes, and a project
in version control shows only what really changed.

## Fields

### The file

| Field | Type | Meaning |
|---|---|---|
| `format` | string | Always `"musicable-project"`. |
| `version` | whole number | The format version: 7 for this page. |
| `ticks_per_beat` | whole number | Always 322560. It's there so the file explains its own positions. |
| `tempo` | tempo map | Tempo and time signature over the song. |
| `loop_region` | range | The region playback loops over. It's kept while looping is off. |
| `looping` | boolean | Whether playback loops. |
| `metronome` | boolean | Whether the metronome clicks while the transport plays. |
| `punch_in` | boolean | Whether recording waits for the start of `loop_region`: it punches in there. |
| `punch_out` | boolean | Whether recording stops at the end of `loop_region`: it punches out there. |
| `next_track_id` | whole number | The id the next new track gets. It's above every track's id, and ids of deleted tracks aren't used again, so it can be higher still. |
| `next_send_id` | whole number | The same for sends. |
| `next_device_id` | whole number | The same for devices, on tracks and on the master. |
| `next_clip_id` | whole number | The same for clips. |
| `next_locator_id` | whole number | The same for locators. |
| `next_take_id` | whole number | The same for takes. |
| `tracks` | array of tracks | Top to bottom, as the arrangement shows them. |
| `master` | master | The master channel. |
| `locators` | array of locators | Named places in the song, in any order. |

### Tempo map

| Field | Type | Meaning |
|---|---|---|
| `tempos` | array of tempo changes | In order of position, the first at 0. A tempo holds until the next change. |
| `signatures` | array of time signature changes | In order of bar, the first at bar 0. Each holds until the next change. |

The first tempo and the first time signature also cover the time before the
song starts. Tempo changes are steps: there are no ramps yet.

A **tempo change**:

| Field | Type | Meaning |
|---|---|---|
| `at` | position | Where the tempo starts: 0 for the first, and after the change before it for the rest. |
| `bpm` | float | Quarter notes per minute, from 20 to 999. |

A **time signature change**:

| Field | Type | Meaning |
|---|---|---|
| `bar` | whole number | The bar it starts at, counting from 0: 0 for the first, and after the change before it for the rest. |
| `numerator` | whole number | The beats in a bar, from 1 to 255. |
| `denominator` | whole number | The note those beats are: 1, 2, 4, 8, 16, 32 or 64. In 6/8 a bar is six eighth notes. |

Bar 0 starts at position 0, and each bar is as long as its time signature
says: three beats for a bar of 6/8, for example. The bar a time signature
change starts at must start within the position limit.

### Range

| Field | Type | Meaning |
|---|---|---|
| `start` | position | Where the range starts. |
| `end` | position | Where it ends, just after its last tick. A range that doesn't end after it starts is empty, and doesn't loop. |

### Track

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_track_id`, and no other track has it. It stays with the track. |
| `name` | string | Any text, empty included. |
| `kind` | string | `"audio"`, `"midi"`, `"return"` or `"group"`. Return and group tracks are buses: see Routing. |
| `gain` | float | The fader, linear: 1.0 is unity and 0.5 about −6 dB. Negative gains invert the signal. |
| `pan` | float | Balance, from −1.0 (left) to 1.0 (right). At 0.0 both channels pass unchanged. Turned to one side, the other channel falls in a straight line to silence at the end. |
| `muted` | boolean | Whether the track is muted. |
| `solo` | boolean | Whether the track is soloed. |
| `solo_safe` | boolean | Whether the track keeps playing while other tracks are soloed. |
| `armed` | boolean | Whether the track is armed for recording. Only audio and MIDI tracks can be. |
| `input` | input or `null` | What the track records from: an audio input on an audio track, a MIDI input on a MIDI track, or `null` for nothing. Return and group tracks record nothing, so theirs is `null`. |
| `monitor` | string | When the track plays what comes into its input instead of its clips: `"in"`, always; `"auto"`, while it's armed and the transport is stopped or recording it; or `"off"`, never. |
| `output` | whole number or `null` | The id of the group track the track feeds, or `null` for the master. |
| `sends` | array of sends | The track's sends to return tracks. |
| `tone` | test tone or `null` | A test oscillator, Tier 0's only sound source. It plays along with the track's clips, and on a group or return track with what feeds the track. |
| `clips` | array of clips | What the track plays: audio clips on an audio track, MIDI clips on a MIDI track, in any order. Return and group tracks have none. |
| `takes` | array of takes | What the track recorded, top to bottom. Return and group tracks have none. |
| `devices` | array of devices | The track's devices, in signal order. |
| `clip_type` | string | How the track's clip stage clips: `"off"`, clean floating point that never clips; `"hard"`, which cuts everything beyond full scale; or `"soft"`, which rounds off peaks above half scale so they get ever closer to full scale without reaching it. |
| `console` | object | Console modelling settings. It's empty until console mode arrives, and has no fields yet. |

A **test tone** is a sine wave that sounds while the transport plays:

| Field | Type | Meaning |
|---|---|---|
| `frequency` | float | In Hz. |
| `level` | float | Linear amplitude: 1.0 is full scale. |

An **input** is an object with one field: `audio`, holding an audio
input, or `midi`, holding a MIDI input. An **audio input** is some channels
of an audio device's input:

| Field | Type | Meaning |
|---|---|---|
| `device` | string | The input, as Musicable's audio backend names it on the computer that chose it, or empty for the default input: the system's, or the ASIO driver's while one plays. A loopback input, which records what an output device plays, is `"loopback:"` and the output's id. |
| `name` | string | The input's name when it was chosen, to show while it isn't there. Empty for the default input. |
| `channel` | whole number | The first channel it takes, counting from 0. |
| `stereo` | boolean | Whether it takes the channel after `channel` too, for a stereo recording, rather than one channel, which plays in the middle of the track's two. A stereo input's `channel` is below 65535. |

A **MIDI input**:

| Field | Type | Meaning |
|---|---|---|
| `device` | string | The input, as Musicable's MIDI backend names it, or empty for every MIDI input. |
| `name` | string | The input's name when it was chosen, to show while it isn't there. |
| `channel` | whole number or `null` | The one MIDI channel it takes, from 0 to 15, or `null` for every channel, as MPE controllers need. |

A **take** is what a track recorded in one pass of a recording, kept in a
lane under the track's own clips. Takes don't play: comping copies the best
parts of them to the track's clips.

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_take_id`, and no other take in the project has it. |
| `name` | string | Any text, empty included. |
| `clips` | array of clips | What the take recorded: clips of the track's kind, in any order. Their ids are clip ids like any other's: no two clips in the project, on tracks or in takes, have the same one. |

A **clip** plays on its track from `start` for `length`. Clips can
overlap: overlapping clips play together, and where audio clips' fades
overlap, they crossfade.

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_clip_id`, and no other clip in the project has it. It stays with the clip. |
| `name` | string | Any text, empty included. |
| `start` | position | Where the clip starts in the song: 0 or after. |
| `length` | whole number | How long it plays, in ticks: more than 0, and it ends within the position limit. |
| `content` | object | What the clip plays, as an object with one field: `audio`, holding an audio clip, or `midi`, holding a MIDI clip. |

An **audio clip** plays part of an audio file, WAV, AIFF or FLAC, at the
file's own speed whatever the tempo, since there's no warping yet. A file
at another sample rate than the audio device's is resampled.

| Field | Type | Meaning |
|---|---|---|
| `file` | string | The file's path: relative to the folder the project file is in, with `/` between names, or absolute. Not empty. |
| `offset` | whole number | The frame of the file the clip starts playing from, up to 2⁵³ − 1. Where the clip runs on past the end of the file, it's silent. |
| `gain` | float | Linear, like a track's `gain`. |
| `fade_in` | whole number | How long the clip fades in from silence, from its start, in ticks: from 0 to the clip's length. |
| `fade_out` | whole number | How long it fades out to silence, up to its end. Together with `fade_in`, at most the clip's length. |

Fades are equal-power quarter sines, so two clips whose fades overlap by
the same length crossfade at constant power.

A **MIDI clip** plays notes, and moves MIDI controllers such as the sustain
pedal:

| Field | Type | Meaning |
|---|---|---|
| `offset` | position | Where in the clip's own time it starts playing: a note at `offset` plays at the clip's start. Notes that start before it, or at or after `offset` plus the clip's length, don't play, and a note still sounding when the clip ends stops there. |
| `next_note_id` | whole number | The id the next new note in the clip gets, above every note's id in the clip, like `next_track_id`. |
| `notes` | array of notes | In any order. Notes on the same key can overlap: they sound as two notes. |
| `controllers` | array of controller curves | In order of controller number, at most one for each. A controller the clip doesn't move has none. |

A **note**:

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below the clip's `next_note_id`, and no other note in the clip has it. It stays with the note however the notes around it change. |
| `start` | position | Where the note starts, in the clip's own time. |
| `length` | whole number | In ticks: more than 0, and it ends within the position limit. |
| `key` | whole number | The MIDI note number, from 0 to 127: 60 is middle C, and 69 the A at 440 Hz. |
| `velocity` | float | How hard the note starts, from 0 to 1. |
| `release_velocity` | float | How quickly it's let go, from 0 to 1. Keyboards that don't measure it give 0.5. |
| `expression` | expression | How the note changes while it sounds. |

A note's **expression** is the three per-note dimensions that MPE
controllers send and that VST3 and CLAP note expression carry. Each is a
curve: an array of points `[at, value]`, where `at` is in ticks from the
note's start, 0 or after, and the points are in order of `at`. A point's
value holds until the next point's, and of two points on the same tick the
later one wins. Points at or after the note's end don't play, but they're
kept, so a note shortened and lengthened again keeps them. A curve with no
points leaves the instrument to start the dimension at rest.

| Field | Values | Meaning |
|---|---|---|
| `pitch_bend` | −120 to 120 | Semitones from the note's key. |
| `pressure` | 0 to 1 | Pressure on the key after it went down (aftertouch). |
| `timbre` | 0 to 1 | MPE's third dimension, usually moving a finger up and down the key (CC 74, "slide"). |

A **controller curve** says how the clip moves one MIDI controller:

| Field | Type | Meaning |
|---|---|---|
| `controller` | whole number | MIDI's control change number, from 0 to 119: 1 is the mod wheel, 7 volume, 11 expression and 64 the sustain pedal. 120 to 127 are MIDI's channel mode messages, which aren't controllers. |
| `points` | array of points | At least one point `[at, value]`, where `at` is a position in the clip's own time, as a note's `start` is, and `value` is from 0 to 1: a MIDI value from 0 to 127 is that over 127. In order of `at`. |

A point's value holds until the next point's, and of two points on the same
tick the later one wins, as in an expression curve. The clip leaves the
controller alone before its first point. Wherever playback starts, the
controller starts at what the clips before then left it at, so a song
played from its middle sounds as it does played from the start. Controllers
64 to 69 are switches, such as the sustain pedal, on from 0.5: one that's on
when its clip ends, or when playback stops, is let go there, as notes are.

A **send** feeds some of the track's signal to a return track:

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_send_id`, and no other send in the project has it. |
| `to` | whole number | The id of the return track it feeds. |
| `level` | float | Linear, like `gain`. |
| `pre_fader` | boolean | Whether it takes the signal before the track's fader and pan, rather than after them. |

A **device** processes the signal on its way through the channel:

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_device_id`, and no other device in the project has it, on a track or on the master. |
| `kind` | object | What the device is, as an object with one field: the kind's name, holding the kind's settings. |

There are four kinds of device: Musicable's own, plugins, and two test
devices.

| Kind | Settings | Meaning |
|---|---|---|
| `native` | native device | One of Musicable's own devices, as below. |
| `plugin` | plugin | A CLAP or VST3 plugin, as below. |
| `test_gain` | `gain`: float | Multiplies the signal by `gain`, linear like the fader. |
| `test_instrument` | `level`: float | Plays the notes of the track's MIDI clips with a plain tone, added to the signal coming in: a sine at each note's pitch, bent by its pitch bend, brighter with its timbre, and louder with velocity and pressure, peaking at `level` at full velocity. |

So a test device that halves the signal is
`{"id": 3, "kind": {"test_gain": {"gain": 0.5}}}`. On a MIDI track, the
first instrument gets the notes: the first device that plays notes, which
is a sampler, a synth, a test instrument or a plugin instrument.

A **native device**'s settings say which of Musicable's own devices it is
and how it's set:

| Field | Type | Meaning |
|---|---|---|
| `kind` | string | `"eq"`, `"compressor"`, `"gate"`, `"delay"`, `"reverb"`, `"utility"`, `"sampler"` or `"synth"`. |
| `sample` | string or `null` | A sampler's sample: the file's path, as an audio clip's `file` has it. Not empty. `null` for a sampler without one, and for every other kind. |
| `params` | object | Each parameter's value, under its key, as the tables below list them: a float within the parameter's range, and a whole number for switches, choices, notes and counts. Each key at most once. |

A parameter missing from `params` takes its default, so a kind can gain
parameters, at the end of its table, without a new version of the format.
Musicable writes every parameter, in the table's order. A kind's parameters
are numbered by their place in its table, from 0, as the `#` column shows,
and `device.set_param` and automation point at them by number. Parameters
are only ever added at the end, so the numbers stay. What each parameter
does is in the kind's file in `crates/model/src/native/`.

**EQ** (`"eq"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `band1_on` | Band 1 On | 0 off, 1 on | 0 |
| 1 | `band1_shape` | Band 1 Shape | 0 Low Cut 12, 1 Low Cut 24, 2 Low Cut 48, 3 Low Shelf, 4 Bell, 5 Notch, 6 High Shelf, 7 High Cut 12, 8 High Cut 24, 9 High Cut 48 | 1 |
| 2 | `band1_freq` | Band 1 Freq | 10 to 22000 Hz | 30 |
| 3 | `band1_gain` | Band 1 Gain | -24 to 24 dB | 0 |
| 4 | `band1_q` | Band 1 Q | 0.1 to 18 | 0.7071067811865476 |
| 5 | `band2_on` | Band 2 On | 0 off, 1 on | 1 |
| 6 | `band2_shape` | Band 2 Shape | as `band1_shape` | 3 |
| 7 | `band2_freq` | Band 2 Freq | 10 to 22000 Hz | 100 |
| 8 | `band2_gain` | Band 2 Gain | -24 to 24 dB | 0 |
| 9 | `band2_q` | Band 2 Q | 0.1 to 18 | 0.7071067811865476 |
| 10 | `band3_on` | Band 3 On | 0 off, 1 on | 1 |
| 11 | `band3_shape` | Band 3 Shape | as `band1_shape` | 4 |
| 12 | `band3_freq` | Band 3 Freq | 10 to 22000 Hz | 250 |
| 13 | `band3_gain` | Band 3 Gain | -24 to 24 dB | 0 |
| 14 | `band3_q` | Band 3 Q | 0.1 to 18 | 0.7071067811865476 |
| 15 | `band4_on` | Band 4 On | 0 off, 1 on | 1 |
| 16 | `band4_shape` | Band 4 Shape | as `band1_shape` | 4 |
| 17 | `band4_freq` | Band 4 Freq | 10 to 22000 Hz | 800 |
| 18 | `band4_gain` | Band 4 Gain | -24 to 24 dB | 0 |
| 19 | `band4_q` | Band 4 Q | 0.1 to 18 | 0.7071067811865476 |
| 20 | `band5_on` | Band 5 On | 0 off, 1 on | 1 |
| 21 | `band5_shape` | Band 5 Shape | as `band1_shape` | 4 |
| 22 | `band5_freq` | Band 5 Freq | 10 to 22000 Hz | 2500 |
| 23 | `band5_gain` | Band 5 Gain | -24 to 24 dB | 0 |
| 24 | `band5_q` | Band 5 Q | 0.1 to 18 | 0.7071067811865476 |
| 25 | `band6_on` | Band 6 On | 0 off, 1 on | 1 |
| 26 | `band6_shape` | Band 6 Shape | as `band1_shape` | 4 |
| 27 | `band6_freq` | Band 6 Freq | 10 to 22000 Hz | 6000 |
| 28 | `band6_gain` | Band 6 Gain | -24 to 24 dB | 0 |
| 29 | `band6_q` | Band 6 Q | 0.1 to 18 | 0.7071067811865476 |
| 30 | `band7_on` | Band 7 On | 0 off, 1 on | 1 |
| 31 | `band7_shape` | Band 7 Shape | as `band1_shape` | 6 |
| 32 | `band7_freq` | Band 7 Freq | 10 to 22000 Hz | 10000 |
| 33 | `band7_gain` | Band 7 Gain | -24 to 24 dB | 0 |
| 34 | `band7_q` | Band 7 Q | 0.1 to 18 | 0.7071067811865476 |
| 35 | `band8_on` | Band 8 On | 0 off, 1 on | 0 |
| 36 | `band8_shape` | Band 8 Shape | as `band1_shape` | 8 |
| 37 | `band8_freq` | Band 8 Freq | 10 to 22000 Hz | 18000 |
| 38 | `band8_gain` | Band 8 Gain | -24 to 24 dB | 0 |
| 39 | `band8_q` | Band 8 Q | 0.1 to 18 | 0.7071067811865476 |
| 40 | `output` | Output | -24 to 24 dB | 0 |

**Compressor** (`"compressor"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `threshold` | Threshold | -60 to 0 dB | -18 |
| 1 | `ratio` | Ratio | 1 to 20, a ratio to 1 | 4 |
| 2 | `attack` | Attack | 0.1 to 200 ms | 10 |
| 3 | `release` | Release | 1 to 3000 ms | 100 |
| 4 | `knee` | Knee | 0 to 24 dB | 6 |
| 5 | `output` | Output | -24 to 24 dB | 0 |
| 6 | `mix` | Mix | 0 to 100 % | 100 |
| 7 | `detector` | Detector | 0 Peak, 1 RMS | 0 |

**Gate** (`"gate"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `threshold` | Threshold | -80 to 0 dB | -40 |
| 1 | `return` | Return | 0 to 24 dB | 3 |
| 2 | `attack` | Attack | 0.02 to 100 ms | 0.5 |
| 3 | `hold` | Hold | 0 to 1000 ms | 10 |
| 4 | `release` | Release | 1 to 3000 ms | 100 |
| 5 | `floor` | Floor | -80 to 0 dB | -80 |

**Delay** (`"delay"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `sync` | Time Sync | 0 off, 1 on | 1 |
| 1 | `time` | Time Time | 1 to 5000 ms | 250 |
| 2 | `note` | Time Note | 0 1/32, 1 1/16T, 2 1/16, 3 1/16D, 4 1/8T, 5 1/8, 6 1/8D, 7 1/4T, 8 1/4, 9 1/4D, 10 1/2T, 11 1/2, 12 1/2D, 13 1/1 | 6 |
| 3 | `feedback` | Feedback | 0 to 100 % | 40 |
| 4 | `ping_pong` | Ping Pong | 0 off, 1 on | 0 |
| 5 | `low_cut` | Filter Low Cut | 20 to 20000 Hz | 100 |
| 6 | `high_cut` | Filter High Cut | 20 to 20000 Hz | 8000 |
| 7 | `mix` | Mix | 0 to 100 % | 30 |

**Reverb** (`"reverb"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `predelay` | Pre-Delay | 0 to 250 ms | 10 |
| 1 | `size` | Size | 0 to 100 % | 50 |
| 2 | `decay` | Decay | 0.1 to 30 s | 2 |
| 3 | `damping` | Damping | 1000 to 20000 Hz | 6000 |
| 4 | `low_cut` | Low Cut | 20 to 1000 Hz | 80 |
| 5 | `width` | Width | 0 to 100 % | 100 |
| 6 | `mix` | Mix | 0 to 100 % | 25 |

**Utility** (`"utility"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `gain` | Gain | 0 to 15.848931924611133, linear gain | 1 |
| 1 | `pan` | Pan | -1 to 1, left to right | 0 |
| 2 | `width` | Width | 0 to 200 % | 100 |
| 3 | `invert_left` | Invert L | 0 off, 1 on | 0 |
| 4 | `invert_right` | Invert R | 0 off, 1 on | 0 |
| 5 | `mute` | Mute | 0 off, 1 on | 0 |
| 6 | `dc` | DC Filter | 0 off, 1 on | 0 |

**Sampler** (`"sampler"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `root` | Sample Root | 0 to 127, a MIDI note | 60 |
| 1 | `transpose` | Sample Transpose | -48 to 48 semitones, whole | 0 |
| 2 | `detune` | Sample Detune | -50 to 50 cents | 0 |
| 3 | `start` | Sample Start | 0 to 100 % | 0 |
| 4 | `end` | Sample End | 0 to 100 % | 100 |
| 5 | `loop` | Sample Loop | 0 off, 1 on | 0 |
| 6 | `loop_start` | Sample Loop Start | 0 to 100 % | 0 |
| 7 | `mode` | Sample Mode | 0 Classic, 1 One-Shot | 0 |
| 8 | `attack` | Amp Attack | 0.1 to 20000 ms | 1 |
| 9 | `decay` | Amp Decay | 1 to 20000 ms | 600 |
| 10 | `sustain` | Amp Sustain | 0 to 100 % | 100 |
| 11 | `release` | Amp Release | 1 to 20000 ms | 50 |
| 12 | `velocity` | Amp Velocity | 0 to 100 % | 50 |
| 13 | `volume` | Amp Volume | -48 to 12 dB | 0 |
| 14 | `filter` | Filter Type | 0 Off, 1 Low Pass, 2 High Pass, 3 Band Pass | 0 |
| 15 | `cutoff` | Filter Cutoff | 20 to 20000 Hz | 2000 |
| 16 | `resonance` | Filter Resonance | 0 to 100 % | 0 |
| 17 | `voices` | Voices | 1 to 32, whole | 8 |
| 18 | `loop_fade` | Sample Loop Fade | 0 to 1000 ms | 10 |

**Synth** (`"synth"`):

| # | Key | Control | Values | Default |
|---|---|---|---|---|
| 0 | `osc1_wave` | Osc 1 Wave | 0 Sine, 1 Triangle, 2 Saw, 3 Square | 2 |
| 1 | `osc1_octave` | Osc 1 Octave | -3 to 3, whole | 0 |
| 2 | `osc1_level` | Osc 1 Level | 0 to 100 % | 100 |
| 3 | `osc2_wave` | Osc 2 Wave | as `osc1_wave` | 2 |
| 4 | `osc2_pitch` | Osc 2 Pitch | -24 to 24 semitones, whole | 0 |
| 5 | `osc2_detune` | Osc 2 Detune | -100 to 100 cents | 0 |
| 6 | `osc2_level` | Osc 2 Level | 0 to 100 % | 0 |
| 7 | `noise` | Noise Level | 0 to 100 % | 0 |
| 8 | `filter` | Filter Type | 0 Low Pass 12, 1 Low Pass 24, 2 High Pass 12, 3 Band Pass 12 | 1 |
| 9 | `cutoff` | Filter Cutoff | 20 to 20000 Hz | 2000 |
| 10 | `resonance` | Filter Resonance | 0 to 100 % | 10 |
| 11 | `filter_env` | Filter Envelope | -96 to 96 semitones | 24 |
| 12 | `key_track` | Filter Key Track | 0 to 100 % | 50 |
| 13 | `filter_attack` | Filter Env Attack | 0.1 to 20000 ms | 1 |
| 14 | `filter_decay` | Filter Env Decay | 1 to 20000 ms | 400 |
| 15 | `filter_sustain` | Filter Env Sustain | 0 to 100 % | 30 |
| 16 | `filter_release` | Filter Env Release | 1 to 20000 ms | 200 |
| 17 | `amp_attack` | Amp Env Attack | 0.1 to 20000 ms | 1 |
| 18 | `amp_decay` | Amp Env Decay | 1 to 20000 ms | 600 |
| 19 | `amp_sustain` | Amp Env Sustain | 0 to 100 % | 80 |
| 20 | `amp_release` | Amp Env Release | 1 to 20000 ms | 150 |
| 21 | `volume` | Volume | -48 to 12 dB | -6 |
| 22 | `velocity` | Velocity | 0 to 100 % | 50 |
| 23 | `voices` | Voices | 1 to 32, whole | 8 |
| 24 | `glide` | Glide | 0 to 2000 ms | 0 |


A **plugin**'s settings say which plugin it is and how it's set:

| Field | Type | Meaning |
|---|---|---|
| `format` | string | `"clap"` or `"vst3"`. |
| `id` | string | The plugin's id in its format, which finds the plugin when the project opens: a CLAP plugin's id, such as `"org.surge-synth-team.surge-xt"`, or a VST3 plugin's class id. Not empty. |
| `name` | string | The plugin's name when it was added, to show while it's missing. Any text, empty included. |
| `instrument` | boolean | Whether it plays notes. |
| `state` | string or `null` | The state the plugin last saved, in its own format, as base64 with padding (RFC 4648). `null` for a plugin as it starts, new. |
| `params` | array | The values of parameters set after the state was saved, as pairs `[id, value]`: the plugin's id for the parameter, a whole number up to 4,294,967,295, and its value in the plugin's own units, a float. In order of id, at most one for each. |

Opening the project gives the plugin its state, then the parameter values.
Musicable saves a fresh state from each plugin it has open, so `params` is
usually empty in a file it writes. A plugin that isn't on the computer
passes the signal through unchanged, and keeps its settings as they are, so
the project still has them when it opens where the plugin is.

### Master

The master channel: every track that doesn't feed a group feeds the
master, and the master feeds the audio device.

| Field | Type | Meaning |
|---|---|---|
| `gain` | float | The fader, as for a track. |
| `pan` | float | Balance, as for a track. |
| `devices` | array of devices | In signal order. |
| `clip_type` | string | As for a track. |
| `console` | object | As for a track. |

### Locator

A named place in the song, shown on the arrangement's ruler for finding
the parts of a song and jumping between them.

| Field | Type | Meaning |
|---|---|---|
| `id` | whole number | Below `next_locator_id`, and no other locator has it. It stays with the locator. |
| `name` | string | Any text, empty included. |
| `at` | position | Where it is. Two locators can be at the same place. |

### Routing

Signal flows from tracks to buses without loops:

- A track's `output` feeds the master or a group track. Return tracks
  always feed the master, so theirs is `null`.
- Groups can feed other groups, but not themselves, through any number of
  other groups.
- Sends feed return tracks only, at most one from each track to each
  return track. Return tracks have no sends.
- Any track, group tracks included, can feed a group or send to a return
  track, wherever it is in the list.

## Versions

| Version | Date | Changes |
|---|---|---|
| 1 | 2026-10-21 | The first version: tempo map, loop, metronome and tracks with test tones, clip types and console settings. |
| 2 | 2026-10-21 | The mixer: each track's pan, solo, solo-safe, arm, output and sends, devices on tracks and on the master, the master channel, and the `"hard"` and `"soft"` clip types. Version 1 tracks open centred, feeding the master with no sends or devices, return tracks solo-safe, and the master at unity. |
| 3 | 2026-10-21 | Clips: audio clips that play files, MIDI clips of notes with per-note expression, and the test instrument. Version 2 tracks have no clips. |
| 4 | 2026-10-22 | Locators: named places in the song. Version 3 files have none. |
| 5 | 2026-10-22 | Plugin devices, with their state and parameters. Version 4 files have none. |
| 6 | 2026-10-22 | Recording: each track's input and monitoring, takes, and punching in and out. Version 5 audio tracks record from the default input's first two channels, MIDI tracks from every MIDI input, all with `"auto"` monitoring and no takes, and punching in and out is off. |
| 7 | 2026-10-22 | Controllers in MIDI clips, such as the mod wheel and the sustain pedal. Version 6 MIDI clips move none. |
| 8 | 2026-10-22 | Musicable's own devices: an EQ, a compressor, a gate, a delay, a reverb, a utility, a sampler and a synth. Version 7 files have none. |

## Changing the format

Any change to what a project file holds is a new version: a new field, a
renamed one, a new value for a field, or a new rule. The one exception is a
new parameter at the end of a built-in device's table, since files without
it take its default. To make a new version:

1. Raise `VERSION` in `crates/io/src/project.rs`. It won't compile until the
   next step is done.
2. Add the step that upgrades files of the version before to `STEPS` in
   `crates/io/src/project/migrate.rs`. Steps work on the parsed JSON and stay
   forever, so files of every version keep loading.
3. Change the types in `crates/io/src/project/schema.rs`, which define the
   format, and their conversions to and from the model. The conversions name
   every field of the model types, so a new model field won't compile until
   the format handles it.
4. Write the new version's fixtures with
   `MUSICABLE_BLESS=1 cargo test -p musicable-io --test fixtures`. That writes
   `crates/io/tests/fixtures/v<N>/` and leaves the older folders alone. The
   tests load the older versions' fixtures through your step and check they
   come out as the same projects. To cover what's new, add a fixture rather
   than change one.
5. Update this page: the tables, and a line in the version list.

## Coming later

- A variation seed for each channel in `console`, so a mix with console mode
  on sounds the same every time it opens.
- DAWproject and Ableton `.als` import and export are separate formats in
  `crates/io`, not versions of this one.
