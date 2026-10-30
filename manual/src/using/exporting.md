# Exporting audio

**File > Export Audio...** (Ctrl+Shift+R, as in Live) opens the export
window, which renders the song to audio files faster than real time, while
Musicable plays on:

| Setting | What it does |
|---|---|
| Render | The song, from its start to the end of its last clip; the loop region; or the selection: the time selection, or else from the first selected clip to the end of the last. It starts on the selection when there is one. |
| Files | The mix, as you hear it, mute and solo too; and each track ticked on its own (stems): the track after its fader, pan and devices, as it feeds its bus, before mute and solo. A return track's file is its return; a group's, what its unmuted tracks feed it. Sends aren't in a track's file, and the master's devices aren't in any but the mix's. |
| Format | WAV, AIFF or FLAC, 16-bit or 24-bit, or 32-bit float in WAV and AIFF, which keeps what goes over full scale. FLAC files carry the samples' MD5 checksum. |
| Sample rate | The one Musicable plays at, or another: the song then renders at that rate, its plugins too, and audio files at other rates are converted as they play. |
| Dither | Adds triangular noise of one step as the samples are rounded to 16 or 24 bits, so quiet sounds fade out smoothly. Exact silence stays silent, and every export of the same song is the same, bit for bit. |
| Normalise | Raises or lowers each file so its loudest sample is at 0 dBFS. |
| Render the tail | Renders on after the end until everything has been quiet, below -96 dBFS, for two seconds, for 30 seconds at most, and leaves the quiet end off, so reverbs and releases ring out. |

**Export...** asks where to save the mix, and the stems go next to it, named
after it and their track's number: `Song.wav`, `Song 01 Drums.wav`,
`Song 02 Bass.wav`. The window then shows how far along the export is and
how many times faster than real time it renders, and **Cancel** stops it.
Closing the window leaves it running, and the status bar says when it's
done. The window then lists each file with its peak, and warns about samples
over full scale, which clip in 16- and 24-bit files. The window keeps its
settings for the next export.

## What it renders

An export renders exactly what Musicable plays, with a copy of the project
as it is, plugins and all. The copy plays without the metronome, looping or
punching in, from the start of the range, and stops at its end, to the
sample. Each file starts with the range, lined up to the sample however late
the plugins make it, and the stems line up with the mix.

An export renders on all the cores, even a long chain of plugins on one
track or the master, which it splits into parts that run down the chain at
once: each plugin renders one part while the plugin after it renders the
part before.

Stems render with solo cleared, so a group's stem has all its unmuted
tracks; when solo is on and the mix is wanted too, the mix renders as it's
heard.

A cancelled or failed export leaves no half-written files.

## From the command line

`musicable export` exports a project without opening the window, with the
plugins Musicable's last plugin scan found
([The command line](../reference/command-line.md#exporting)).
