# Working on Musicable

This file is for agents, such as Claude Code, working in this repository.
[CONTRIBUTING.md](CONTRIBUTING.md) has the rules for changes, from the
manual to the checks CI runs, and [docs](docs) says how each part works.

## Audio files: use the audio toolkit

When you need to look into an audio file, cut, join or convert one,
change its level, measure its loudness, find where its sound is, or
compare two renders or a recording with an export, use
Musicable's own audio commands, rather than writing a script or reaching
for ffmpeg or sox. They read and write WAV, AIFF and FLAC files exactly,
measure loudness as EBU R 128 does, and say what they did as JSON.
`musicable audio` alone lists them.

| Command | What it does |
|---|---|
| `info` | Format, encoding, channels, sample rate, length, peak, and an MD5 of the samples |
| `cut` | Copies a stretch of a file, in seconds or frames, unchanged or with fades |
| `join` | Puts files end to end, or loops one, with gaps or crossfades |
| `convert` | Writes a file at another sample rate, encoding, format or channel count |
| `gain` | Makes a file louder or quieter, by decibels, or to a loudness or a peak |
| `loudness` | Integrated loudness, loudness range, momentary and short-term maxima, sample and true peaks, RMS |
| `find` | The loudest or quietest stretches of a length, or where sound and silence are |
| `compare` | Lines B up with A and says how it differs: offset, gain, what's left and where, loudness, bands, onset timing, clicks |

Build the program once, then run them in a shell:

```sh
cargo build --release -p musicable-app
target/release/musicable audio loudness mix.wav --json
target/release/musicable audio cut take.wav --from 12.5 --length 4 --out loop.wav
target/release/musicable audio cut --help
```

On Windows it's `target\release\musicable.exe`, and on Linux the build
needs the ALSA headers (`libasound2-dev`), as CI installs them. The
release build takes about 5 minutes from nothing on 4 cores. A debug build
runs the commands too, but 10 to 25 times slower: the release build
measures the loudness of a 3-minute song in about a second, and a debug
one in 20.

- With `--json`, each field has its unit in its name, such as
  `integrated_lufs` or `peak_dbfs`, and silence is `null`. Without it, the
  result is a field on each line.
- Two files hold the same samples when `info` gives them the same
  `audio_md5`, whatever their formats.
- A command never writes over a file without `--overwrite`, and never
  writes to a file it reads.
- The same commands are MCP tools, `audio_info`, `audio_cut` and so on, in
  `musicable mcp`, which runs them itself, with Musicable closed. Give
  them whole paths.
- `compare A B` is the check that one file plays another: `identical`
  says whether every sample is the same, and otherwise it lines B up
  (`offset_ms`), matches its level (`gain_db`) and says how loud what's
  left is (`residual_db`, against A). `--no-align --no-match-level`
  compares them as they are, as a recording against its export needs.
  Its `onsets.median_lag_ms` says how early or late B's transients are,
  and `clicks` lists the clicks one has and the other doesn't; `--at`
  measures the high frequencies at positions you give, such as a slice's
  loop turns.
- For a file's tempo, beats, bars and transients, as warping finds them,
  run `musicable analyse FILE`. Without the network's model files, which
  `scripts/fetch-models.sh` fetches, give `--fast`. Its `--json` lists
  every beat and pitch mark, which can run to hundreds of kilobytes for a
  song.

The manual's [Audio commands](manual/src/reference/audio-commands.md) page
has every option and field, with examples, and
[docs/agents.md](docs/agents.md#the-audio-tools) says how they're built.
