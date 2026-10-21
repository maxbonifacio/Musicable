# Project files

Musicable saves each project as one JSON file with the extension
`.musicable`. The format is open: this page describes all of it, so other
tools can read and write projects too. `crates/io` implements it, in
`musicable_io::project`.

## Example

A project with one track, looping the first four bars:

```json
{
  "format": "musicable-project",
  "version": 1,
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
  "next_track_id": 1,
  "tracks": [
    {
      "id": 0,
      "name": "Bass",
      "kind": "audio",
      "gain": 0.8,
      "muted": false,
      "tone": {
        "frequency": 55.0,
        "level": 0.5
      },
      "clip_type": "off",
      "console": {}
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
tables list them, indents with two spaces, ends every line with `\n`, and
writes each float in the shortest form that reads back as exactly the same
number. So saving an unchanged project gives the same bytes, and a project
in version control shows only what really changed.

## Fields

### The file

| Field | Type | Meaning |
|---|---|---|
| `format` | string | Always `"musicable-project"`. |
| `version` | whole number | The format version: 1 for this page. |
| `ticks_per_beat` | whole number | Always 322560. It's there so the file explains its own positions. |
| `tempo` | tempo map | Tempo and time signature over the song. |
| `loop_region` | range | The region playback loops over. It's kept while looping is off. |
| `looping` | boolean | Whether playback loops. |
| `metronome` | boolean | Whether the metronome clicks while the transport plays. |
| `next_track_id` | whole number | The id the next new track gets. It's above every track's id, and ids of deleted tracks aren't used again, so it can be higher still. |
| `tracks` | array of tracks | Top to bottom, as the arrangement shows them. |

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
| `kind` | string | `"audio"`, `"midi"`, `"return"` or `"group"`. |
| `gain` | float | Linear: 1.0 is unity and 0.5 about −6 dB. Negative gains invert the signal. |
| `muted` | boolean | Whether the track is muted. |
| `tone` | test tone or `null` | A test oscillator, Tier 0's only sound source. |
| `clip_type` | string | How the track's clip stage clips. Only `"off"` so far: clean floating point, which never clips. |
| `console` | object | Console modelling settings. It's empty until console mode arrives, and has no fields yet. |

A **test tone** is a sine wave that sounds while the transport plays:

| Field | Type | Meaning |
|---|---|---|
| `frequency` | float | In Hz. |
| `level` | float | Linear amplitude: 1.0 is full scale. |

## Versions

| Version | Date | Changes |
|---|---|---|
| 1 | 2026-10-21 | The first version: tempo map, loop, metronome and tracks with test tones, clip types and console settings. |

## Changing the format

Any change to what a project file holds is a new version: a new field, a
renamed one, a new value for a field, or a new rule. To make one:

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

- Clips and notes, with per-note expression (MPE) in the note from its first
  version, so editing expression later doesn't change the format again.
- A variation seed for each channel in `console`, so a mix with console mode
  on sounds the same every time it opens.
- DAWproject and Ableton `.als` import and export are separate formats in
  `crates/io`, not versions of this one.
