<!-- Made from the code by crates/app/src/toolkit/manual.rs: don't edit it. `MUSICABLE_BLESS=1 cargo test -p musicable-app --lib manual` makes it again. -->

The examples run in a folder, `/music`, of three files: `tone.wav`, 2 seconds of stereo 24-bit 44.1 kHz audio, half a second of silence and then a 441 Hz tone at -12 dBFS for a second and at -6 dBFS for half a second; `loop.flac`, a second of four noise hits, in stereo 16-bit 44.1 kHz; and `voice.aif`, 1.5 seconds of a 240 Hz tone that swells and fades, in mono 32-bit floating point at 48 kHz.

### convert

**Convert Audio File**: `musicable audio convert FILE [options]`, or the tool `audio_convert`.

Writes a file again at another sample rate, in another encoding, with other channels or in another format, which out's extension says. Resampling keeps everything up to 20 kHz from 44.1 kHz, and integers too narrow for the samples are dithered. A conversion that changes only the format, widens the samples or makes a mono file stereo copies them exactly, which it reads back to check.

| Option | Argument | What it's for |
|---|---|---|
| `FILE` | `file` | The audio file to read, by its path: a WAV, AIFF or FLAC file. Required. |
| `--out <path>` | `out` | Where to write the new file, by its path. Its extension says the format: .wav, .aif or .flac. Required. |
| `--rate <n>` | `rate` | The sample rate to write, in hertz, such as 48000. Without it, the file's. |
| `--encoding <int8\|int16\|int24\|int32\|float32\|float64>` | `encoding` | How to store the samples: int8 to int32 for integers of that many bits, or float32 or float64. FLAC files take integers of up to 24 bits. Without it, as the file does. |
| `--channels <n>` | `channels` | 1 mixes the channels down to one, and 2 copies a mono file's channel to two. Without it, as many as the file has. |
| `--dither` | `dither` | Dithers samples written as integers too narrow to hold them exactly, so what rounding loses is noise, not distortion. Without it, true. |
| `--overwrite` | `overwrite` | Writes over a file at out. Without it, a file there stops the command. |

It gives back:

| Field | What it is |
|---|---|
| `out` | The new file. Its fields are those `info` gives. |
| `clipped_samples` | Samples beyond full scale, which integers clip, and floats keep. |
| `dithered` | Whether the samples were dithered. |
| `exact` | Whether the new file's samples read back as the file's, sample for sample: so when only the format changed, the samples got wider, or a mono file's channel was copied to two. |

For example,

```sh
musicable audio convert tone.wav --rate 48000 --encoding int16 --out tone-48k.flac
```

prints

```text
out:
  file: /music/tone-48k.flac
  format: flac
  encoding: int16
  channels: 2
  sample_rate: 48000
  frames: 96000
  seconds: 2.0
  peak_dbfs: -6.02
  acid_bpm: none
  audio_md5: 3c97d96071dac9dd480730b912fae105
clipped_samples: 0
dithered: true
exact: false
```

### cut

**Cut Audio File**: `musicable audio cut FILE [options]`, or the tool `audio_cut`.

Copies a stretch of a file to a new file, its frames unchanged, stored as the file stores them, and reads the new file back to check them. The stretch starts at from or from_frame, and ends at to or to_frame, or after length or length_frames: seconds, or frames for a cut to the frame.

| Option | Argument | What it's for |
|---|---|---|
| `FILE` | `file` | The audio file to read, by its path: a WAV, AIFF or FLAC file. Required. |
| `--out <path>` | `out` | Where to write the new file, by its path. Its extension says the format: .wav, .aif or .flac. Required. |
| `--from <n>` | `from` | Where the stretch starts, in seconds from the start of the file. Without it or from_frame, it starts where the file does. |
| `--to <n>` | `to` | Where the stretch ends, in seconds from the start of the file. Without it, to_frame or a length, it ends where the file does. |
| `--from-frame <n>` | `from_frame` | Where the stretch starts, in frames from the start of the file, instead of from. |
| `--to-frame <n>` | `to_frame` | Where the stretch ends, in frames from the start of the file, instead of to. |
| `--length <n>` | `length` | How long the stretch is, in seconds, instead of where it ends. |
| `--length-frames <n>` | `length_frames` | How long the stretch is, in frames, instead of where it ends. |
| `--fade-ms <n>` | `fade_ms` | Fades the stretch in and out over this many milliseconds each, with a raised cosine, so its ends don't click. Without it, every frame is copied unchanged. |
| `--overwrite` | `overwrite` | Writes over a file at out. Without it, a file there stops the command. |

It gives back:

| Field | What it is |
|---|---|
| `from` | Where the stretch starts in the file. Its `seconds` and `frame`. |
| `to` | Where the stretch ends: the frame after its last. Its `seconds` and `frame`. |
| `out` | The new file. Its fields are those `info` gives. |
| `exact` | Whether the new file's frames read back as the stretch's, sample for sample, but for its fades. |

For example,

```sh
musicable audio cut tone.wav --from 0.5 --length 1 --out cut.wav
```

prints

```text
from: 0.5 s, frame 22050
to: 1.5 s, frame 66150
out:
  file: /music/cut.wav
  format: wav
  encoding: int24
  channels: 2
  sample_rate: 44100
  frames: 44100
  seconds: 1.0
  peak_dbfs: -12.04
  acid_bpm: none
  audio_md5: b85a231fc0dc9b21890feb3d97cc4bf7
exact: true
```

### find

**Find Stretches of Audio**: `musicable audio find FILE [options]`, or the tool `audio_find`.

Finds stretches of a file by how loud they are: the loudest or quietest stretches of a length that don't overlap, loudest or quietest first, or the stretches of sound, or of silence, in order. It says where the sound starts and ends too, to the frame. The quietest are looked for between the first sound and the last. With from or to, it looks in that stretch of the file.

| Option | Argument | What it's for |
|---|---|---|
| `FILE` | `file` | The audio file to read, by its path: a WAV, AIFF or FLAC file. Required. |
| `--what <loudest\|quietest\|sound\|silence>` | `what` | What to find: the loudest or quietest stretches of a length, or each stretch of sound, or of silence between sounds. Required. |
| `--length <n>` | `length` | How long the loudest or quietest stretches are, in seconds. Without it, 10, or the whole file if it's shorter. |
| `--count <n>` | `count` | How many stretches to give at most, from 1 to 200. Without it, the loudest or quietest, or the first 200 sounds or silences. |
| `--threshold-dbfs <n>` | `threshold_dbfs` | How loud a sample must be to be sound, in dBFS. Without it, -60. |
| `--min-silence <n>` | `min_silence` | The shortest silence, in seconds: shorter ones are part of the sound around them. Without it, 0.5. |
| `--from <n>` | `from` | Where the stretch starts, in seconds from the start of the file. Without it or from_frame, it starts where the file does. |
| `--to <n>` | `to` | Where the stretch ends, in seconds from the start of the file. Without it or to_frame, it ends where the file does. |
| `--from-frame <n>` | `from_frame` | Where the stretch starts, in frames from the start of the file, instead of from. |
| `--to-frame <n>` | `to_frame` | Where the stretch ends, in frames from the start of the file, instead of to. |

It gives back:

| Field | What it is |
|---|---|
| `what` | What was looked for. |
| `stretches` | The stretches found: the loudest or quietest first, or sounds or silences in order. |
| `stretches[].from` | Where it starts. Its `seconds` and `frame`. |
| `stretches[].to` | Where it ends: the frame after its last. Its `seconds` and `frame`. |
| `stretches[].seconds` | How long it is. |
| `stretches[].loudness_lufs` | Its loudness, in LUFS: the mean power of its K-weighted samples, as BS.1770-4 weights them, ungated. |
| `stretches[].peak_dbfs` | Its loudest sample, in dBFS. |
| `found` | How many there are in all, for sounds and silences: those after the first count, or 200, aren't listed. |
| `sound` | Where the first sound starts and the last ends, to the frame: null if it's silent throughout. |
| `sound.from` | Where the first sound starts. Its `seconds` and `frame`. |
| `sound.to` | Where the last sound ends: the frame after it. Its `seconds` and `frame`. |

For example,

```sh
musicable audio find tone.wav --what loudest --length 0.5
```

prints

```text
what: loudest
stretches:
  - from: 1.5 s, frame 66150
    to: 2.0 s, frame 88200
    seconds: 0.5
    loudness_lufs: -8.42
    peak_dbfs: -6.02
found: 1
sound:
  from: 0.500023 s, frame 22051
  to: 2.0 s, frame 88200
```

### gain

**Change Audio Level**: `musicable audio gain FILE [options]`, or the tool `audio_gain`.

Writes a file again louder or quieter: by a number of decibels with db, or to an integrated loudness, a sample peak or a true peak with to_lufs, to_peak_dbfs or to_true_peak_dbtp, or to another file's loudness with like. Give one of them. It measures the new file, and says how many samples went beyond full scale, which integers clip.

| Option | Argument | What it's for |
|---|---|---|
| `FILE` | `file` | The audio file to read, by its path: a WAV, AIFF or FLAC file. Required. |
| `--out <path>` | `out` | Where to write the new file, by its path. Its extension says the format: .wav, .aif or .flac. Required. |
| `--db <n>` | `db` | The gain, in decibels: 6 doubles the level, and -6 halves it. |
| `--to-lufs <n>` | `to_lufs` | The integrated loudness to bring the file to, in LUFS, such as -14 or -23. |
| `--to-peak-dbfs <n>` | `to_peak_dbfs` | The sample peak to bring the file to, in dBFS, such as -1. |
| `--to-true-peak-dbtp <n>` | `to_true_peak_dbtp` | The true peak to bring the file to, in dBTP, such as -1. |
| `--like <path>` | `like` | Another audio file, by its path, whose integrated loudness to bring the file to. |
| `--dither` | `dither` | Dithers samples written as integers, so what rounding loses is noise, not distortion. Without it, true. |
| `--overwrite` | `overwrite` | Writes over a file at out. Without it, a file there stops the command. |

It gives back:

| Field | What it is |
|---|---|
| `gain_db` | The gain, in decibels, to 2 places. |
| `integrated_lufs` | The new file's integrated loudness, in LUFS. |
| `sample_peak_dbfs` | The new file's loudest sample, in dBFS. |
| `true_peak_dbtp` | The new file's true peak, between its samples too, in dBTP. |
| `clipped_samples` | Samples the gain took beyond full scale, which integers clip, and floats keep. |
| `out` | The new file. Its fields are those `info` gives. |

For example,

```sh
musicable audio gain voice.aif --to-lufs -23 --out voice-23.wav
```

prints

```text
gain_db: -9.05
integrated_lufs: -23.0
sample_peak_dbfs: -13.5
true_peak_dbtp: -13.5
clipped_samples: 0
out:
  file: /music/voice-23.wav
  format: wav
  encoding: float32
  channels: 1
  sample_rate: 48000
  frames: 72000
  seconds: 1.5
  peak_dbfs: -13.5
  acid_bpm: none
  audio_md5: 1dca2c0be141df77597125cfa7809651
```

### info

**Describe Audio Files**: `musicable audio info FILE... [options]`, or the tool `audio_info`.

Says what each file holds: its format, how it stores its samples, its channels, sample rate and length, its peak, the tempo its ACID chunk gives, and the MD5 of its samples.

| Option | Argument | What it's for |
|---|---|---|
| `FILE...` | `files` | The audio files to read, by their paths: WAV, AIFF or FLAC files, 200 at most. Required. |

It gives back:

| Field | What it is |
|---|---|
| `files` | Each file, in the order given. One the command couldn't read or write has only its `file` and an `error`, which says why. |
| `files[].file` | Its whole path. |
| `files[].format` | Its format. |
| `files[].encoding` | How it stores its samples: int8 to int32 for integers of that many bits, or float32 or float64. |
| `files[].channels` | Its channels. |
| `files[].sample_rate` | Its sample rate, in hertz. |
| `files[].frames` | Its length in frames, a sample of each channel. |
| `files[].seconds` | Its length in seconds, to 6 places. |
| `files[].peak_dbfs` | Its loudest sample, in decibels from full scale, to 2 places: null if it's silent. |
| `files[].acid_bpm` | The tempo its ACID chunk gives, as loops from sample libraries have, in beats a minute: null without one. |
| `files[].audio_md5` | The MD5 of its samples, as a FLAC file keeps it: files of the same samples hash the same, whatever their format. |

For example,

```sh
musicable audio info tone.wav loop.flac voice.aif
```

prints

```text
files:
  - file: /music/tone.wav
    format: wav
    encoding: int24
    channels: 2
    sample_rate: 44100
    frames: 88200
    seconds: 2.0
    peak_dbfs: -6.02
    acid_bpm: none
    audio_md5: 56f9fa874015d4bcfedceddff98ed85a
  - file: /music/loop.flac
    format: flac
    encoding: int16
    channels: 2
    sample_rate: 44100
    frames: 44100
    seconds: 1.0
    peak_dbfs: -3.14
    acid_bpm: none
    audio_md5: 6a35c09835843c9b43cd8690bbba6a7a
  - file: /music/voice.aif
    format: aiff
    encoding: float32
    channels: 1
    sample_rate: 48000
    frames: 72000
    seconds: 1.5
    peak_dbfs: -4.45
    acid_bpm: none
    audio_md5: d6e79aa3c7bfb666ab1ef3c0b793fb0d
```

### join

**Join Audio Files**: `musicable audio join FILE... [options]`, or the tool `audio_join`.

Joins files end to end in a new file, or plays them over and over, as for looping a drum break: with silence between them, or each crossfading into the next. They must have the same sample rate and channels. The new file stores samples as the widest of them do, so frames outside the crossfades are copied unchanged, which it reads back to check.

| Option | Argument | What it's for |
|---|---|---|
| `FILE...` | `files` | The audio files to join, in order, by their paths: WAV, AIFF or FLAC files, 200 at most. A file can be there more than once. Required. |
| `--out <path>` | `out` | Where to write the new file, by its path. Its extension says the format: .wav, .aif or .flac. Required. |
| `--repeat <n>` | `repeat` | How many times to play the files, one after another: 4 plays them four times. Without it, once. |
| `--gap <n>` | `gap` | Seconds of silence between one file and the next. Without it, none. |
| `--crossfade-ms <n>` | `crossfade_ms` | Crossfades each file into the next over this many milliseconds, keeping their power the same, instead of a gap. They overlap by as much, so the new file is shorter. |
| `--overwrite` | `overwrite` | Writes over a file at out. Without it, a file there stops the command. |

It gives back:

| Field | What it is |
|---|---|
| `parts` | Each file's place in the new file, in order: the first 200 if there are more. |
| `parts[].file` | The file's whole path. |
| `parts[].from` | Where it starts in the new file. Its `seconds` and `frame`. |
| `parts[].to` | Where it ends in the new file, the frame after its last: it overlaps the next by a crossfade. Its `seconds` and `frame`. |
| `count` | How many parts there are: the files, times repeat. |
| `out` | The new file. Its fields are those `info` gives. |
| `exact` | Whether each part's frames read back as its file's, sample for sample, but for crossfades. |

For example,

```sh
musicable audio join loop.flac tone.wav --repeat 2 --crossfade-ms 10 --out joined.flac
```

prints

```text
parts:
  - file: /music/loop.flac
    from: 0.0 s, frame 0
    to: 1.0 s, frame 44100
  - file: /music/tone.wav
    from: 0.99 s, frame 43659
    to: 2.99 s, frame 131859
  - file: /music/loop.flac
    from: 2.98 s, frame 131418
    to: 3.98 s, frame 175518
  - file: /music/tone.wav
    from: 3.97 s, frame 175077
    to: 5.97 s, frame 263277
count: 4
out:
  file: /music/joined.flac
  format: flac
  encoding: int24
  channels: 2
  sample_rate: 44100
  frames: 263277
  seconds: 5.97
  peak_dbfs: -1.79
  acid_bpm: none
  audio_md5: ce078890d12391364f4a13d9259dcc20
exact: true
```

### loudness

**Measure Loudness**: `musicable audio loudness FILE... [options]`, or the tool `audio_loudness`.

Measures each file's loudness as ITU-R BS.1770-4 and EBU R 128 do: its integrated loudness and loudness range, and its loudest momentary and short-term loudness, with its sample peak, true peak, RMS level, DC offset and clipped samples. With from or to, it measures that stretch of each file.

| Option | Argument | What it's for |
|---|---|---|
| `FILE...` | `files` | The audio files to read, by their paths: WAV, AIFF or FLAC files, 200 at most. Required. |
| `--from <n>` | `from` | Where the stretch starts, in seconds from the start of the file. Without it or from_frame, it starts where the file does. |
| `--to <n>` | `to` | Where the stretch ends, in seconds from the start of the file. Without it or to_frame, it ends where the file does. |
| `--from-frame <n>` | `from_frame` | Where the stretch starts, in frames from the start of the file, instead of from. |
| `--to-frame <n>` | `to_frame` | Where the stretch ends, in frames from the start of the file, instead of to. |

It gives back:

| Field | What it is |
|---|---|
| `files` | Each file's levels, in the order given. One the command couldn't read or write has only its `file` and an `error`, which says why. |
| `files[].file` | Its whole path. |
| `files[].seconds` | How long the stretch measured is, in seconds. |
| `files[].integrated_lufs` | Its integrated loudness, in LUFS, gated as BS.1770-4 gates it: null without 0.4 s louder than -70 LUFS. |
| `files[].range_lu` | Its loudness range, in LU, as EBU Tech 3342 measures it: null without 3 s louder than -70 LUFS. |
| `files[].momentary_max_lufs` | Its loudest momentary loudness, over 0.4 s, in LUFS. |
| `files[].short_term_max_lufs` | Its loudest short-term loudness, over 3 s, in LUFS. |
| `files[].sample_peak_dbfs` | Its loudest sample, in dBFS. |
| `files[].true_peak_dbtp` | Its true peak, between its samples too, in dBTP: oversampled 4 times, as BS.1770-4 has it. |
| `files[].rms_dbfs` | The RMS level of its samples, in dBFS: a full-scale sine reads -3.01. |
| `files[].dc_offset` | The mean of the channel whose mean is furthest from 0, to 6 places: 0 without a DC offset. |
| `files[].clipped_samples` | Its samples at full scale or beyond, either way. |

For example,

```sh
musicable audio loudness tone.wav voice.aif
```

prints

```text
files:
  - file: /music/tone.wav
    seconds: 2.0
    integrated_lufs: -12.4
    range_lu: none
    momentary_max_lufs: -8.42
    short_term_max_lufs: none
    sample_peak_dbfs: -6.02
    true_peak_dbtp: -6.02
    rms_dbfs: -15.05
    dc_offset: 0.000142
    clipped_samples: 0
  - file: /music/voice.aif
    seconds: 1.5
    integrated_lufs: -13.95
    range_lu: none
    momentary_max_lufs: -11.27
    short_term_max_lufs: none
    sample_peak_dbfs: -4.45
    true_peak_dbtp: -4.45
    rms_dbfs: -13.98
    dc_offset: 0.0
    clipped_samples: 0
```
