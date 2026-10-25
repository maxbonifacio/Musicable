# Signalsmith Stretch and Signalsmith Linear

The time-stretch and pitch-shift that the Spectral warp mode renders with,
by Geraint Luff and Signalsmith Audio Ltd, under the MIT licence: each
folder has its `LICENSE.txt`. Both are header-only C++ libraries, copied
here as they were released, unchanged:

- `signalsmith-stretch/signalsmith-stretch.h`: Signalsmith Stretch 1.4.0,
  the release tagged `1.4.0` at
  <https://github.com/Signalsmith-Audio/signalsmith-stretch>. Its header
  still calls itself 1.3.2.
- `signalsmith-linear/`: the FFT and STFT it uses (`fft.h` and `stft.h`),
  from Signalsmith Linear 0.6.4, the release Stretch 1.4.0 builds with,
  tagged `0.6.4` at <https://github.com/Signalsmith-Audio/linear>, without
  the rest of the library, which these don't include, or its optional back
  ends, which Musicable doesn't turn on.

Musicable's own C interface to them is `../src/stretch.cpp`, and its Rust
bindings `../src/lib.rs`, written by hand so the build needs no LLVM for
bindgen.

To update them, copy the new releases' files over these, change the
versions here, and bump `VERSION` in `../src/lib.rs`, so the render cache
renders again with the new code.
