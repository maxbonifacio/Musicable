# Audio test files

FLAC files from another encoder than ours, for `src/audio/flac.rs`'s tests.
ffmpeg 6.1 made them, with its default settings but for the block size, from
the signal `ffmpeg_signal` in those tests computes, written as raw PCM:

```sh
ffmpeg -f s16le -ar 44100 -ac 2 -i stereo.raw -frame_size 1152 ffmpeg-stereo-16.flac
ffmpeg -f s24le -ar 96000 -ac 1 -i mono.raw -frame_size 4096 ffmpeg-mono-24.flac
```

- `ffmpeg-stereo-16.flac`: 10,000 frames, 16-bit stereo at 44.1 kHz, in nine
  frames of 1,152 samples, the last one shorter. A few runs of bytes in it
  look like frame headers.
- `ffmpeg-mono-24.flac`: 5,000 frames, 24-bit mono at 96 kHz, in two frames.

Both have a Vorbis comment and 8 KiB of padding after STREAMINFO, as ffmpeg
writes them.
