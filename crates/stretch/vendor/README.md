# Vendored stretchers

Musicable's own C interfaces to these are `../src/stretch.cpp` and
`../src/rubber_band.cpp`, and its Rust bindings `../src/lib.rs` and
`../src/rubber_band.rs`, written by hand so the build needs no LLVM for
bindgen.

## Signalsmith Stretch and Signalsmith Linear

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

To update them, copy the new releases' files over these, change the
versions here, and bump `VERSION` in `../src/lib.rs`, so the render cache
renders again with the new code.

## Rubber Band Library

The time-stretch and pitch-shift that Spectral renders with while High
quality is on: Rubber Band Library 4.0.0 by Particular Programs Ltd, under
the GNU General Public License, version 2 or later (`rubberband/COPYING`),
which lets Musicable, under version 3 or later, include it. `rubberband/`
holds these files of the release, from
<https://breakfastquay.com/files/releases/rubberband-4.0.0.tar.bz2> (SHA-256
`af050313ee63bc18b35b2e064e5dce05b276aaf6d1aa2b8a82ced1fe2f8028e9`), which
has the same code as the tag `v4.0.0` (commit `1d95888`) at
<https://github.com/breakfastquay/rubberband>:

- `COPYING`, the licence.
- `rubberband/`, the public headers.
- `single/RubberBandSingle.cpp`, which builds the whole library as one file,
  and everything under `src/` that it includes: `src/common`, `src/faster`,
  `src/finer`, and the three files beside them.

Left out: `src/ext` (other people's code, for back ends that the single-file
build doesn't use), the Java bindings, the programs and tests, and the
build files.

Musicable changed two of them, `src/finer/R3Stretcher.h` and
`src/finer/R3Stretcher.cpp`, on 25 October 2026; each change there is marked
"Changed for Musicable, 25 October 2026". The changes are to the R3 engine,
which Musicable drives directly:

1. How finely the formant envelope follows the spectrum can be set
   (`setFormantEnvelopeScale`): the cepstral cutoff in `analyseFormant`
   scales by it, which is what the Envelope control sets.
2. Passing a key frame, the stretcher aims from where its output has got
   to, at the input the next hop is centred on, for the next key frame,
   rather than from the key frame passed: a hop may pass one most of a hop
   late, and the original then put everything after it that much late, more
   with every key frame. It marks every key frame the input has passed, not
   only the first.
3. Key frames work in real-time mode too.
4. Output is resampled at the pitch scale the hops whose frames were centred
   on it were synthesised at, rather than at the pitch scale as it is when
   it's put out, half the longest frame later. Otherwise a pitch change in
   real-time mode reached the audio from before it, and moved everything
   after it in time: up a fifth, by 15 ms.

To update it, copy the new release's files over these, apply the changes to
them again, change the version and hash here, and bump
`RUBBER_BAND_VERSION` in `../src/rubber_band.rs`, so the render cache
renders again with the new code.
