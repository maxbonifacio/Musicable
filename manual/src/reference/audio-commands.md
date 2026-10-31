# Audio commands

`musicable audio` works on audio files without opening Musicable: it cuts
a stretch out of a file, joins files or loops one, converts a file to
another sample rate, format or bit depth, makes it louder or quieter, and
measures its loudness and peaks, finds where its sound starts and its
loudest moment, or compares two files: a take with the one before, or a
recording with an export. It reads and writes WAV, AIFF and FLAC files.

```sh
musicable audio cut take.wav --from 12.5 --length 4 --out loop.wav
musicable audio loudness mix.wav master.flac
```

Agents, such as Claude Code, have each of these commands as a tool too:
`audio_cut`, `audio_loudness` and so on. `musicable mcp` runs them itself,
so they work while Musicable is closed, and they never touch the project
you have open ([Agents](command-line.md#agents)).

## On the command line

Each command takes its file, or its files, first, then its options.
`compare` takes two, A and then B:

- An option is `--name value`, such as `--fade-ms 10`, or `--name=value`.
- An option that's true or false takes `--name` for true, and `--no-name`
  for false: `--overwrite`, or `--no-dither`.
- Times are in seconds, such as `--from 1.5`. The options that end in
  `-frame` or `-frames` take frames instead, for cutting to the sample: a
  frame is a sample of each channel.
- Files are in the folder you run the command in, unless you give their
  whole paths.

`musicable audio` alone lists the commands, and `musicable audio cut
--help` says what `cut` takes.

A command prints what it did, a field on each line. With `--json`, it
prints it as JSON instead, for scripts, with the fields named as below. If
the command fails, it says why and exits with 1, and with `--json` it
prints `{"error": "..."}` instead. A command given several files reads the
others when one fails, says why that one failed, and exits with 1.

## Files are safe

- A command never writes over a file unless you give `--overwrite`, and
  never writes to a file it reads, even with `--overwrite`.
- It writes the new file under another name first, the file's name with
  `.part` after it, and renames it once it's whole, so a file that's there
  is whole. Stopping a command leaves nothing behind.
- `cut` and `join` copy samples exactly as the file stores them, and so
  does `convert` when it only changes the format, widens the samples or
  makes a mono file stereo.
  They read the new file back, and `exact` says whether every sample came
  out as it went in.

## What they give back

- Levels are in decibels from full scale (dBFS), true peaks in dBTP, and
  loudness in LUFS, as ITU-R BS.1770-4 and EBU R 128 measure it. Silence
  has no level, which prints as `none`, and is `null` in JSON.
- A position in a file has its time in seconds, to 6 places, and its
  frame, counting from 0 at the start. A stretch ends at the frame after
  its last.
- The files a command writes are described as `info` describes files:
  their path, format, encoding, channels, sample rate, length, peak, the
  tempo of their ACID chunk if they have one, and the MD5 of their
  samples. Files of the same samples hash the same, whatever their format.

## The commands

{{#include ../generated-audio/commands.md}}
