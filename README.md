# tsm-lab

A research-spike time-stretch engine for music, written clean-room from published papers.

The question it is built to answer: can a stretcher that analyses the whole file offline
(source separation, known onsets, no latency budget) sound better than the best commercial
real-time stretchers on music? It can't be judged by ear here, so the engine ships with
objective self-checks (`tsm-lab check`, `cargo test --release`). A human will run the blind
A/B tests.

Status: builds on stable Rust (edition 2024), `cargo test --release` passes, clippy is clean
(with `needless_range_loop` allowed crate-wide), and every limited row of the full check passes
(see [Self-checks](#self-checks)).

## Usage

```text
cargo build --release
target/release/tsm-lab --in a.wav --out b.wav --ratio R [--pitch SEMITONES] [--onsets o.json] [--variant NAME]
target/release/tsm-lab check [--material DIR | --no-material] [--quick]
```

* `--ratio R`: output length / input length (2.0 = twice as long). Output length is exactly
  `round(N*R)` frames, written as 32-bit float WAV with the input's sample rate and channels.
* `--onsets o.json`: optional `[{"frame": u64, "strength": f32, "percussiveness": f32}, ...]`,
  where `frame` is the input sample where the attack starts. An attack at input frame t comes
  out at `round(t*R)`. The output has no constant latency (tested to within 2 samples for
  clicks; attack-envelope timing is within 0.1 ms).
* `--pitch S`: semitones. The engine stretches by `R*2^(S/12)`, then resamples with a
  Kaiser-windowed sinc (32 zero crossings, beta = 9) back to exactly `round(N*R)` frames.
* `--variant`: `full` (default), `full-2way`, `pv`, `hptsm` (see [Variants](#variants)).
* `check`: runs the self-checks and prints a Markdown table. By default it uses the material in
  `/home/user/stretch-material`. It picks a file named "Master", then "Mix", then "Moon" for the
  real-mix rows.

Examples (diagnostics on real files, not part of the product):
`cargo run --release --example levels -- file.wav R onsets.json` (level, peak and spectrum
change per variant) and `--example bands` (1/3-octave change per variant and per part).

## Design (`full`)

```text
 input ─► 3-way median split (soft masks, shared by all channels)
            │  energy-normalised parts for R != 1, merged H+P grouping outside attacks
            ├─ harmonic H  ─► PGHI phase vocoder (L 4096, hop 512), onset resets ─┐
            ├─ percussive P ─► onset-anchored OLA/WSOLA (512 / 256)              ├─► sum
            └─ residual R  ─► stretched cross-spectral envelope, random phases ─┘
```

### 1. Separation (`split.rs`, behind the `Separator` trait)

* The split is a median-filtering harmonic/percussive/residual separation (Fitzgerald 2010).
  The 3-way version uses the separation factor β and a residual
  (Driedger, Müller, Disch 2014).
  * STFT: Hann 2048, hop 512.
  * Filters: ~200 ms time median (17 frames) for H and ~500 Hz frequency median (23 bins)
    for P.
  * Soft Wiener-like masks: `M_H = H^2/(H^2 + β^2 P^2)` and `M_P` likewise, with β = 2;
    `M_R = 1 - M_H - M_P`.
* **Shared masks.** All channels use one set of masks, computed on the channel energy sum, so
  a source is never routed differently in L and R.
* **Main-lobe protection.** Around every harmonic peak with `M_H >= 0.7`, a neighbour's
  harmonic mask is raised to the share of its magnitude that the peak's Hann main lobe can
  explain. Without this, the skirts of strong partials leak into the residual and come back as
  a noise halo.
* **Low tonal peaks** (below 250 Hz) hand their main lobe's residual share to H. These are
  peaks 12 dB above the bins 3–4 away.
  * Walking bass lines fail the 200 ms time median, and random-phase resynthesis at 21.5 Hz
    bin spacing smeared them: +3 dB at 126 Hz and −2 dB at 63 Hz on a mix.
* **Energy-normalised parts** (my own fix, not from a paper).
  * Soft-mask parts are coherent fractions of the same STFT coefficient. Once they go through
    different stretchers they add up incoherently and lose the cross terms: white noise came
    out about 4 dB too quiet.
  * For R ≠ 1 the masks are therefore normalised per bin over the groups that will be
    stretched incoherently, so that Σ over groups of (Σ of the group's masks)² = 1.
  * Outside the attack windows the percussive path is not used. There P is merged into H
    using the matching {H+P}, {R} normalisation, not by adding the energy parts: H and P are
    coherent, so adding them overshot by 0.5–2 dB on a dense mix.
  * At R = 1 the plain amplitude parts are used, so H + P + R = input to float precision.
* A neural separator can replace the median split later. It only has to return time-domain
  parts; the energy normalisation is the only median-split-specific extra (`Parts::merged_hp`).

### 2. Onset-driven reshaping of the parts

* **`percussive_attacks_only`.** With onsets known, P keeps only `[t0 − 256, t0 + 768]`
  around each attack, with raised-cosine fades. The rest goes to the PV.
  * In the grain path, longer drum decays were re-read by overlapping grains: a buzz at the
    grain rate, ±3–6 dB at R = 4.
  * Without onsets, all of P goes to the PV. Grains without placement only re-read attacks:
    17 of 17 clicks were doubled at R = 2, with copies up to 10 ms early.
* **`gate_before_onsets`.** For onsets with strength ≥ 0.25, the region `[t0 − D, t0)` is
  handed entirely to H, with D up to the split window.
  * A time-varying STFT mask spreads energy up to one window before the attack. At R = 1 these
    smears cancel between the parts; after different stretchers they become pre-echo.
  * H's phase vocoder gates new components at the onset, so the hand-over removes that
    pre-echo.

### 3. Harmonic part: PGHI phase vocoder (`pv.rs`)

* **Phase gradient heap integration.** PGHI (Průša, Balazs, Søndergaard 2017) is used in its
  phase-vocoder form, "Phase Vocoder Done Right" (PVDR; Průša & Holighaus).
  * Hann window L = 4096, FFT 8192 (zero-padded ×2), synthesis hop 512 = L/8.
  * Magnitude tolerance 1e-6 of the block maximum. Below it, bins are not integrated and
    islands start with the analysis phase.
* **Correction-field formulation.** The engine integrates `ψ = φ_out − arg X` rather than the
  absolute phase. Its gradient is `dψ/dn = 2π f (Δy − Δc)` in time and
  `dψ/dk = −2π (s − 1) t_off / M` in frequency (trapezoidal rule).
  * At R = 1, ψ is constant, so the output is the input bit for bit (up to float error).
  * For impulses and chirps the field is curl-free (PVDR's `b_s = α b_a`).
* **Gradients by reassignment** (Auger & Flandrin 1995), not finite phase differences.
  * Instantaneous frequency: `f = k/M − Im(X_dw X*)/(2π|X|^2)`.
  * Energy time offset inside the frame: `t_off = Re(X_tw X*)/|X|^2`.
  * Both are energy-weighted over channels.
* **Stereo.** Every channel uses the same ψ (`Y_ch = X_ch e^{iψ}`), so the analysed
  inter-channel phase and level differences are kept exactly. The image cannot wander through
  the phase vocoder.
* **Adaptive time-offset scaling.** `s = 1 + (R − 1)·c`.
  * `c = exp(−var(t_off over ±8 bins)/(2 (L/32)^2))` is 1 for impulse-like bins (all
    neighbours agree on the event time) and 0 for noise.
  * Scaling noise bins pushed their energy out of the window: −2 dB on noise at R = 4.
  * Placement gain `w(s t_off)/w(t_off)` (capped at 4) keeps the coherent overlap-add
    normalisation exact.
* **Heap.** A bucket queue on log-magnitude (0.25 dB buckets) replaces a binary heap. The
  heap runs over blocks of 64 frames plus one carried frame, so integration is bidirectional
  inside a block.
* **Onset model** (Röbel 2003; Duxbury, Davies, Sandler 2002): phase resets and exact placement
  only; no transient is cut out and re-inserted.
  * A synthesis frame is inserted exactly at `y* = round(R t0)` and analysed exactly at `t0`.
  * **New bins** are those whose energy (±1 bin, all channels) at `t0 + L/4` exceeds 2× the
    energy at `t0 − L/2`. Their weight is ν = 1 − pre/post.
  * New bins are reset to ψ = 0 at the onset frame. The heap then integrates from these seeds
    in both time directions, so neighbouring frames stay coherent with the reset.
  * In frames whose analysis or synthesis window contains the onset, new bins are kept only
    for `t ≥ y*` and dropped from frames that end before `y*`.
    * The fade-in is 16 samples for percussive onsets. For soft onsets it is the input's own
      measured rise time × (1 − percussiveness), up to 2048 samples.
    * A 16-sample gate on sung onsets produced a low-frequency thump.
  * **Exact level for truncated components** (my own derivation).
    * New bins keep their analysis waveform (s = 1, ψ constant across the lobe), so a frame
      that saw the new note in only part of its window synthesises that truncated note.
    * Their new share (ν) is accumulated per onset and scaled by `W/T`, where W is the frames'
      Σw² and T is the same sum cut at each frame's truncation point; the gain is capped at 4.
    * This is exact for a stationary note starting at t0, at every ratio.
    * A per-frame boost by the analysed window fraction, tried first, made the first 25 ms
      of every attack 3.5–4.7 dB too loud.
    * Tone bursts now come out within +0.0..+0.7 dB.
  * Onsets with strength < 0.25 or percussiveness < 0.3 get no reset or gating.
    * On sung onsets the "new" bins also carry legitimate pre-onset energy (glides, vibrato).
      Gating it cost up to 4.5 dB in a vocal's fundamental band.

### 4. Percussive part: placed OLA / WSOLA (`ola.rs`, `timemap.rs`)

* Hann 512, hop 256 (Driedger, Müller, Ewert 2014 use OLA for the percussive part).
* **Grain placement** follows an onset-anchored piecewise-linear time map.
  * Slope 1 over ±256 output samples around `y*`, with `t(y*) = t0` exactly. Every grain that
    overlaps the attack reads it from the same relative position, so all copies land on `y*`.
  * The map bends back to `t = y/R` over `K·256` samples, with K = max(3, 2R − 1).
* **WSOLA search** (Verhelst & Roelands 1993) outside the cores, ±448 samples.
  * The template is the natural continuation of the previous grain, rectangular so that a
    zero shift scores exactly 1.
  * Normalised cross-correlation over the stacked channels via FFT; one shift is shared by all
    channels.
  * A shift is accepted only above correlation 0.6, with a penalty of 0.1·|d|/tol, and only if
    the candidate's energy is within ±1 dB of the mapped position's.
  * A shift may never move an onset across a grain boundary, and the search is off inside the
    cores.
  * Plain OLA comb-filtered kick bodies by 8–14 dB.

### 5. Residual: noise resynthesis (`noise.rs`)

* For each synthesis frame (Hann 2048, hop 512) the per-bin channel covariance matrix is
  averaged over analysis frames (hop 256) within ±max(L/2, hop/R) of `c = y/R`, with
  triangular weights.
* Output: `Y = chol(C)·z`, where z is unit-modulus with seeded uniform phases (SplitMix64).
  * This keeps each bin's expected power, inter-channel level and phase difference, and
    coherence. The texture's fine structure is new, never frozen.
* Overlap-add is normalised by `sqrt(Σw²)` (incoherent sum).
* Only analysis frames on the same side of every onset as `c`, not straddling one, are used.
  Frames built from post-onset material start at `y*`, so there is no pre-echo.
* R = 1 is an identity.

### 6. Pitch

Stretch by `R·p` (`p = 2^(S/12)`), then read the result at `j·p` with a Kaiser-windowed sinc
(β = 9, 32 zero crossings, 512-point table, cut-off 0.96·min(1, 1/p)). The output length is
exactly `round(N·R)`.

## Variants

| name | what | why |
|---|---|---|
| `full` (default) | everything above | the candidate |
| `full-2way` | same, but the residual goes through the PV together with H (energy-normalised for the {H+R}, {P} grouping) | tells whether the noise path helps |
| `pv` | the PGHI phase vocoder alone on the whole signal; no split, no onsets | isolates PGHI |
| `hptsm` | classic HPTSM (Driedger, Müller, Ewert 2014, settings after libtsm): binary 2-way median split per channel (2048, 11×11 median), Laroche–Dolson phase-locked PV (4096 / hop 1024) for H, plain OLA (512 / 256) for P, no noise path, no onset handling | published baseline |

`hptsm` processes each channel independently, as the reference implementation does. Its
stereo numbers below show why the other variants do not.

## Papers and what was taken from each

| paper | used for |
|---|---|
| D. Fitzgerald, "Harmonic/percussive separation using median filtering", DAFx 2010 | time/frequency median filters, soft masks |
| J. Driedger, M. Müller, S. Disch, "Extending harmonic-percussive separation of audio signals", ISMIR 2014 | separation factor β and the residual class (3-way split) |
| J. Driedger, M. Müller, S. Ewert, "Improving time-scale modification of music signals using harmonic-percussive separation", IEEE SPL 21(1), 2014 | the HPTSM architecture (PV for H, OLA for P) and the `hptsm` baseline |
| Z. Průša, P. Balazs, P. L. Søndergaard, "A noniterative method for reconstruction of phase from STFT magnitude" (PGHI), IEEE/ACM TASLP 25(5), 2017 | heap integration of the phase gradient, magnitude tolerance |
| Z. Průša, N. Holighaus, "Phase vocoder done right", EUSIPCO 2017 (arXiv 2202.07382) | PGHI in the phase vocoder; time-direction and frequency-direction gradients, `b_s = α b_a` scaling of the time offset |
| F. Auger, P. Flandrin, "Improving the readability of time-frequency and time-scale representations by the reassignment method", IEEE TSP 43(5), 1995 | instantaneous frequency and time offset from derivative- and time-weighted windows |
| A. Röbel, "A new approach to transient processing in the phase vocoder", DAFx 2003 | detecting onset bins and resetting their phase in the PV |
| C. Duxbury, M. Davies, M. Sandler, "Improved time-scaling of musical audio using phase locking at transients", AES 112th Convention, 2002 | phase locking/reset at transients, keeping steady parts stretched |
| W. Verhelst, M. Roelands, "An overlap-add technique based on waveform similarity (WSOLA)...", ICASSP 1993 | WSOLA search for the percussive grains |
| J. Laroche, M. Dolson, "Improved phase vocoder time-scale modification of audio", IEEE TSAP 7(3), 1999 | identity phase locking in the `hptsm` baseline PV |
| G. Steele, D. Lea, C. Flood, "Fast splittable pseudorandom number generators", OOPSLA 2014 | SplitMix64 seeded RNG |
| J. F. Kaiser, "Nonrecursive digital filter design using the I0-sinh window function", ISCAS 1974 (general DSP knowledge) | Kaiser-windowed sinc resampler |
| libtsm (AudioLabs Erlangen, MIT) `tsm.py` | default parameters of OLA / PV / HPS-TSM for the baseline |

Mine (not from a paper, as far as I know):
* the correction-field form of PVDR;
* energy-normalised soft-mask parts with the merged grouping;
* main-lobe and low-peak protection;
* the W/T normalisation of truncated onset components;
* the rise-time gate;
* the covariance-preserving noise path's onset rules;
* the WSOLA safety rules around onsets.

## Self-checks

`tsm-lab check` builds test signals, runs the engine and measures; `cargo test --release`
asserts the same limits (`tests/checks.rs`), plus a click-position test at sample accuracy, a
pitch test and a WAV round-trip. Full table of the final run (release build, single thread):

Summary (worst case over the cases of each row; the full output follows):

| check | cases | limit | `full` | `pv` | `hptsm` |
|---|---|---|---|---|---|
| exact output length | R 0.5, 0.8, 1, 1.25, 2, 3.7 and R 1.5 + 3 st pitch | = 0 frames | 0 (also `full-2way`) | 0 | 0 |
| split H+P+R vs input | real mix, 10 s | > 100 dB SNR | 161.6 dB | | |
| R = 1 reconstruction | real mix, 10 s | > 40 dB SNR | 140.1 dB | 138.4 dB | |
| sine frequency error | 440 Hz and 2637 Hz, R 0.5 / 0.8 / 1.25 / 2 | < 1 cent | ≤ 0.001 | ≤ 0.001 | |
| sine AM ripple | same | < 0.5 dB | ≤ 0.008 dB | ≤ 0.002 dB | |
| sine max spurious | same | < −50 dB | ≤ −66.3 dB | ≤ −65.1 dB | |
| pitch shift | 440 Hz +12 and −7 st at R 1, +5 st at R 1.5 | < 1 cent, < −50 dB | 0.000 cents, ≤ −67.5 dB | | |
| attack timing, with onsets | clicks and drums, R 0.5 / 0.8 / 1.25 / 2 / 4 | < 1 ms | ≤ 0.068 ms | | |
| doubled attacks, with onsets | same | = 0 | 0 | | |
| click pre-echo, with onsets | same | info | −178.6 dB re peak | | |
| clicks without onsets, R 2 | timing / doubled / pre-echo | info | 0.023 ms / 0 / −29.6 dB | 0.023 ms / 0 / −29.6 dB | 1.77 ms / 16 / −7.6 dB |
| drums without onsets, R 2 | timing / doubled | info | 1.98 ms / 0 | 2.05 ms / 0 | 2.66 ms / 0 |
| tonal attacks, with onsets | plucked notes, R 0.5 / 2 / 4: timing; pre-echo excess | info | 3.6 / 4.9 / 4.1 ms; 5.8 / 7.2 / 6.5 dB | | |
| tonal attacks, without onsets | R 2 / 4: timing; pre-echo excess | info | 10.1 / 12.5 ms; 41.7 / 41.6 dB | 11.0 / 15.5 ms; 37.7 / 41.9 dB | 15.1 / 11.6 ms; 50.1 / 47.6 dB |
| white noise, 1/3-octave level | R 0.5 / 2 / 4 | < 1 dB (`full`) | 0.92 / 0.45 / 0.49 dB | 1.43 / 0.62 / 1.23 dB | 5.34 dB (R 2) |
| stereo correlation, synthetic | R 0.8 / 2 | < 0.05 (`full`) | 0.012 / 0.026 | 0.040 (R 2) | 1.79 (R 2) |
| stereo L/R level, synthetic | R 0.8 / 2 | < 0.5 dB (`full`) | 0.13 / 0.24 dB | 0.39 dB (R 2) | 1.16 dB (R 2) |
| stereo correlation, real mix | "Dream at Night - Master", R 0.8 / 2 | < 0.05 (`full`) | 0.014 / 0.022 | | 0.93 (R 2) |
| stereo L/R level, real mix | same | < 0.5 dB (`full`) | 0.13 / 0.26 dB | | 0.16 dB (R 2) |
| CPU s per output minute | stereo 44.1 kHz real mix, single thread, R 0.5 / 2 | info | 12.8 / 6.8 | 3.4 / 3.9 | 8.4 / 2.8 |

Level smoke test on the same mix (`examples/levels`; RMS change, peak, max 1/3-octave change):

| R | `full` | `full-2way` | `pv` | `hptsm` |
|---|---|---|---|---|
| 0.8 | −0.34 dB, peak 0.72 (in 0.61), 1.4 dB | −0.03 dB, 0.70, 2.0 dB | −0.05 dB, 0.61, 0.3 dB | −0.83 dB, 0.62, 2.9 dB |
| 2 | −0.83 dB, peak 0.73, 2.1 dB | −0.63 dB, 0.71, 2.0 dB | −0.77 dB, 0.62, 2.7 dB | −1.10 dB, 0.64, 11.0 dB |

<details>
<summary>Full output of <code>tsm-lab check</code> (162 checks, 91 with limits, 0 failed)</summary>

| check | variant | case | value | unit | limit | pass |
|---|---|---|---:|---|---|---|
| exact length (R 0.5..3.7, +pitch) | full | max |len err| | 0.000 | frames | = 0 | ok |
| exact length (R 0.5..3.7, +pitch) | full-2way | max |len err| | 0.000 | frames | = 0 | ok |
| exact length (R 0.5..3.7, +pitch) | pv | max |len err| | 0.000 | frames | = 0 | ok |
| exact length (R 0.5..3.7, +pitch) | hptsm | max |len err| | 0.000 | frames | = 0 | ok |
| split H+P+R == input | full | real mix 10 s | 161.598 | dB SNR | > 100 | ok |
| R=1 reconstruction | full | real mix 10 s | 140.052 | dB SNR | > 40 | ok |
| R=1 reconstruction | pv | real mix 10 s | 138.441 | dB SNR |  | info |
| sine freq error | full | 440 Hz R=0.5 | -0.001 | cents | |x| < 1 | ok |
| sine AM ripple | full | 440 Hz R=0.5 | 0.005 | dB | < 0.5 | ok |
| sine max spurious | full | 440 Hz R=0.5 | -69.548 | dB | < -50 | ok |
| sine total non-fund. | full | 440 Hz R=0.5 | -61.444 | dB |  | info |
| sine freq error | full | 440 Hz R=0.8 | -0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 440 Hz R=0.8 | 0.007 | dB | < 0.5 | ok |
| sine max spurious | full | 440 Hz R=0.8 | -67.682 | dB | < -50 | ok |
| sine total non-fund. | full | 440 Hz R=0.8 | -59.041 | dB |  | info |
| sine freq error | full | 440 Hz R=1.25 | -0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 440 Hz R=1.25 | 0.008 | dB | < 0.5 | ok |
| sine max spurious | full | 440 Hz R=1.25 | -70.204 | dB | < -50 | ok |
| sine total non-fund. | full | 440 Hz R=1.25 | -59.185 | dB |  | info |
| sine freq error | full | 440 Hz R=2 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 440 Hz R=2 | 0.008 | dB | < 0.5 | ok |
| sine max spurious | full | 440 Hz R=2 | -73.286 | dB | < -50 | ok |
| sine total non-fund. | full | 440 Hz R=2 | -58.555 | dB |  | info |
| sine freq error | full | 2637 Hz R=0.5 | -0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 2637 Hz R=0.5 | 0.005 | dB | < 0.5 | ok |
| sine max spurious | full | 2637 Hz R=0.5 | -66.301 | dB | < -50 | ok |
| sine total non-fund. | full | 2637 Hz R=0.5 | -58.927 | dB |  | info |
| sine freq error | full | 2637 Hz R=0.8 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 2637 Hz R=0.8 | 0.006 | dB | < 0.5 | ok |
| sine max spurious | full | 2637 Hz R=0.8 | -69.730 | dB | < -50 | ok |
| sine total non-fund. | full | 2637 Hz R=0.8 | -58.573 | dB |  | info |
| sine freq error | full | 2637 Hz R=1.25 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 2637 Hz R=1.25 | 0.008 | dB | < 0.5 | ok |
| sine max spurious | full | 2637 Hz R=1.25 | -71.604 | dB | < -50 | ok |
| sine total non-fund. | full | 2637 Hz R=1.25 | -58.296 | dB |  | info |
| sine freq error | full | 2637 Hz R=2 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | full | 2637 Hz R=2 | 0.008 | dB | < 0.5 | ok |
| sine max spurious | full | 2637 Hz R=2 | -68.391 | dB | < -50 | ok |
| sine total non-fund. | full | 2637 Hz R=2 | -57.749 | dB |  | info |
| sine freq error | pv | 440 Hz R=0.5 | -0.001 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 440 Hz R=0.5 | 0.001 | dB | < 0.5 | ok |
| sine max spurious | pv | 440 Hz R=0.5 | -75.143 | dB | < -50 | ok |
| sine total non-fund. | pv | 440 Hz R=0.5 | -73.302 | dB |  | info |
| sine freq error | pv | 440 Hz R=0.8 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 440 Hz R=0.8 | 0.001 | dB | < 0.5 | ok |
| sine max spurious | pv | 440 Hz R=0.8 | -85.326 | dB | < -50 | ok |
| sine total non-fund. | pv | 440 Hz R=0.8 | -84.471 | dB |  | info |
| sine freq error | pv | 440 Hz R=1.25 | -0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 440 Hz R=1.25 | 0.001 | dB | < 0.5 | ok |
| sine max spurious | pv | 440 Hz R=1.25 | -81.173 | dB | < -50 | ok |
| sine total non-fund. | pv | 440 Hz R=1.25 | -78.612 | dB |  | info |
| sine freq error | pv | 440 Hz R=2 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 440 Hz R=2 | 0.002 | dB | < 0.5 | ok |
| sine max spurious | pv | 440 Hz R=2 | -65.077 | dB | < -50 | ok |
| sine total non-fund. | pv | 440 Hz R=2 | -64.505 | dB |  | info |
| sine freq error | pv | 2637 Hz R=0.5 | -0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 2637 Hz R=0.5 | 0.000 | dB | < 0.5 | ok |
| sine max spurious | pv | 2637 Hz R=0.5 | -76.112 | dB | < -50 | ok |
| sine total non-fund. | pv | 2637 Hz R=0.5 | -73.724 | dB |  | info |
| sine freq error | pv | 2637 Hz R=0.8 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 2637 Hz R=0.8 | 0.000 | dB | < 0.5 | ok |
| sine max spurious | pv | 2637 Hz R=0.8 | -86.801 | dB | < -50 | ok |
| sine total non-fund. | pv | 2637 Hz R=0.8 | -85.446 | dB |  | info |
| sine freq error | pv | 2637 Hz R=1.25 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 2637 Hz R=1.25 | 0.000 | dB | < 0.5 | ok |
| sine max spurious | pv | 2637 Hz R=1.25 | -80.460 | dB | < -50 | ok |
| sine total non-fund. | pv | 2637 Hz R=1.25 | -77.853 | dB |  | info |
| sine freq error | pv | 2637 Hz R=2 | 0.000 | cents | |x| < 1 | ok |
| sine AM ripple | pv | 2637 Hz R=2 | 0.001 | dB | < 0.5 | ok |
| sine max spurious | pv | 2637 Hz R=2 | -67.055 | dB | < -50 | ok |
| sine total non-fund. | pv | 2637 Hz R=2 | -65.643 | dB |  | info |
| pitch shift freq error | full | 440 Hz +12 st R=1 | 0.000 | cents | |x| < 1 | ok |
| pitch shift max spurious | full | 440 Hz +12 st R=1 | -72.298 | dB | < -50 | ok |
| pitch shift freq error | full | 440 Hz -7 st R=1 | -0.000 | cents | |x| < 1 | ok |
| pitch shift max spurious | full | 440 Hz -7 st R=1 | -67.466 | dB | < -50 | ok |
| pitch shift freq error | full | 440 Hz +5 st R=1.5 | -0.000 | cents | |x| < 1 | ok |
| pitch shift max spurious | full | 440 Hz +5 st R=1.5 | -71.843 | dB | < -50 | ok |
| attack timing max |err| | full | clicks R=0.5 | 0.000 | ms | < 1 | ok |
| doubled attacks | full | clicks R=0.5 | 0.000 | count | = 0 | ok |
| pre-echo (clicks, worst) | full | clicks R=0.5 | -178.617 | dB re peak |  | info |
| attack timing max |err| | full | clicks R=0.8 | 0.000 | ms | < 1 | ok |
| doubled attacks | full | clicks R=0.8 | 0.000 | count | = 0 | ok |
| pre-echo (clicks, worst) | full | clicks R=0.8 | -178.617 | dB re peak |  | info |
| attack timing max |err| | full | clicks R=1.25 | 0.000 | ms | < 1 | ok |
| doubled attacks | full | clicks R=1.25 | 0.000 | count | = 0 | ok |
| pre-echo (clicks, worst) | full | clicks R=1.25 | -178.617 | dB re peak |  | info |
| attack timing max |err| | full | clicks R=2 | 0.000 | ms | < 1 | ok |
| doubled attacks | full | clicks R=2 | 0.000 | count | = 0 | ok |
| pre-echo (clicks, worst) | full | clicks R=2 | -178.617 | dB re peak |  | info |
| attack timing max |err| | full | clicks R=4 | 0.000 | ms | < 1 | ok |
| doubled attacks | full | clicks R=4 | 0.000 | count | = 0 | ok |
| pre-echo (clicks, worst) | full | clicks R=4 | -178.617 | dB re peak |  | info |
| attack timing max |err| | full | drums R=0.5 | 0.045 | ms | < 1 | ok |
| doubled attacks | full | drums R=0.5 | 0.000 | count | = 0 | ok |
| attack timing max |err| | full | drums R=0.8 | 0.068 | ms | < 1 | ok |
| doubled attacks | full | drums R=0.8 | 0.000 | count | = 0 | ok |
| attack timing max |err| | full | drums R=1.25 | 0.045 | ms | < 1 | ok |
| doubled attacks | full | drums R=1.25 | 0.000 | count | = 0 | ok |
| attack timing max |err| | full | drums R=2 | 0.068 | ms | < 1 | ok |
| doubled attacks | full | drums R=2 | 0.000 | count | = 0 | ok |
| attack timing max |err| | full | drums R=4 | 0.068 | ms | < 1 | ok |
| doubled attacks | full | drums R=4 | 0.000 | count | = 0 | ok |
| attack timing max |err| | full | clicks R=2 no onsets | 0.023 | ms |  | info |
| doubled attacks | full | clicks R=2 no onsets | 0.000 | count |  | info |
| pre-echo (clicks, worst) | full | clicks R=2 no onsets | -29.557 | dB re peak |  | info |
| attack timing max |err| | full | drums R=2 no onsets | 1.977 | ms |  | info |
| doubled attacks | full | drums R=2 no onsets | 0.000 | count |  | info |
| attack timing max |err| | pv | clicks R=2 no onsets | 0.023 | ms |  | info |
| doubled attacks | pv | clicks R=2 no onsets | 0.000 | count |  | info |
| pre-echo (clicks, worst) | pv | clicks R=2 no onsets | -29.604 | dB re peak |  | info |
| attack timing max |err| | pv | drums R=2 no onsets | 2.045 | ms |  | info |
| doubled attacks | pv | drums R=2 no onsets | 0.000 | count |  | info |
| attack timing max |err| | hptsm | clicks R=2 no onsets | 1.773 | ms |  | info |
| doubled attacks | hptsm | clicks R=2 no onsets | 16.000 | count |  | info |
| pre-echo (clicks, worst) | hptsm | clicks R=2 no onsets | -7.585 | dB re peak |  | info |
| attack timing max |err| | hptsm | drums R=2 no onsets | 2.659 | ms |  | info |
| doubled attacks | hptsm | drums R=2 no onsets | 0.000 | count |  | info |
| tonal attack timing max |err| | full | plucked R=0.5 | 3.568 | ms |  | info |
| tonal pre-echo excess (worst) | full | plucked R=0.5 | 5.785 | dB |  | info |
| tonal attack timing max |err| | full | plucked R=2 | 4.932 | ms |  | info |
| tonal pre-echo excess (worst) | full | plucked R=2 | 7.173 | dB |  | info |
| tonal attack timing max |err| | full | plucked R=4 | 4.068 | ms |  | info |
| tonal pre-echo excess (worst) | full | plucked R=4 | 6.500 | dB |  | info |
| tonal attack timing max |err| | full | plucked R=2 no onsets | 10.114 | ms |  | info |
| tonal pre-echo excess (worst) | full | plucked R=2 no onsets | 41.711 | dB |  | info |
| tonal attack timing max |err| | full | plucked R=4 no onsets | 12.545 | ms |  | info |
| tonal pre-echo excess (worst) | full | plucked R=4 no onsets | 41.601 | dB |  | info |
| tonal attack timing max |err| | pv | plucked R=2 no onsets | 10.955 | ms |  | info |
| tonal pre-echo excess (worst) | pv | plucked R=2 no onsets | 37.717 | dB |  | info |
| tonal attack timing max |err| | pv | plucked R=4 no onsets | 15.455 | ms |  | info |
| tonal pre-echo excess (worst) | pv | plucked R=4 no onsets | 41.864 | dB |  | info |
| tonal attack timing max |err| | hptsm | plucked R=2 no onsets | 15.136 | ms |  | info |
| tonal pre-echo excess (worst) | hptsm | plucked R=2 no onsets | 50.125 | dB |  | info |
| tonal attack timing max |err| | hptsm | plucked R=4 no onsets | 11.636 | ms |  | info |
| tonal pre-echo excess (worst) | hptsm | plucked R=4 no onsets | 47.554 | dB |  | info |
| white noise 1/3-oct spectrum | full | R=0.5 | 0.917 | dB max dev | < 1 | ok |
| white noise 1/3-oct spectrum | full | R=2 | 0.447 | dB max dev | < 1 | ok |
| white noise 1/3-oct spectrum | full | R=4 | 0.488 | dB max dev | < 1 | ok |
| white noise 1/3-oct spectrum | pv | R=0.5 | 1.426 | dB max dev |  | info |
| white noise 1/3-oct spectrum | pv | R=2 | 0.624 | dB max dev |  | info |
| white noise 1/3-oct spectrum | pv | R=4 | 1.228 | dB max dev |  | info |
| white noise 1/3-oct spectrum | hptsm | R=2 | 5.339 | dB max dev |  | info |
| stereo corr (synthetic wide) | full | R=0.8 | 0.012 | max |dcorr| | < 0.05 | ok |
| stereo L/R balance (synth.) | full | R=0.8 | 0.125 | dB max dev | < 0.5 | ok |
| stereo corr (synthetic wide) | full | R=2 | 0.026 | max |dcorr| | < 0.05 | ok |
| stereo L/R balance (synth.) | full | R=2 | 0.243 | dB max dev | < 0.5 | ok |
| stereo corr (real mix) | full | Dream at Night - Master  R=0.8 | 0.014 | max |dcorr| | < 0.05 | ok |
| stereo L/R balance (real mix) | full | Dream at Night - Master  R=0.8 | 0.132 | dB max dev | < 0.5 | ok |
| stereo corr (real mix) | full | Dream at Night - Master  R=2 | 0.022 | max |dcorr| | < 0.05 | ok |
| stereo L/R balance (real mix) | full | Dream at Night - Master  R=2 | 0.264 | dB max dev | < 0.5 | ok |
| stereo corr (synthetic wide) | pv | R=2 | 0.040 | max |dcorr| |  | info |
| stereo L/R balance (synth.) | pv | R=2 | 0.390 | dB max dev |  | info |
| stereo corr (synthetic wide) | hptsm | R=2 | 1.791 | max |dcorr| |  | info |
| stereo L/R balance (synth.) | hptsm | R=2 | 1.156 | dB max dev |  | info |
| stereo corr (real mix) | hptsm | Dream at Night - Master  R=2 | 0.927 | max |dcorr| |  | info |
| stereo L/R balance (real mix) | hptsm | Dream at Night - Master  R=2 | 0.156 | dB max dev |  | info |
| CPU s per output minute | full | stereo R=0.5 | 12.840 | s |  | info |
| CPU s per output minute | full | stereo R=2 | 6.750 | s |  | info |
| CPU s per output minute | pv | stereo R=0.5 | 3.420 | s |  | info |
| CPU s per output minute | pv | stereo R=2 | 3.915 | s |  | info |
| CPU s per output minute | hptsm | stereo R=0.5 | 8.400 | s |  | info |
| CPU s per output minute | hptsm | stereo R=2 | 2.835 | s |  | info |

162 checks, 91 with limits, 0 failed

</details>

Notes on the measurements:
* Sine: 440 Hz and 2637 Hz, 5 s. Frequency from a 4-term Blackman-Harris spectrum peak;
  AM ripple from a 20 ms demodulated envelope; "spurious" = largest component outside
  ±20 Hz.
* Attacks: single-sample clicks and synthetic drum hits (stereo). Timing = 50 % point of the
  rise of a 0.5 ms RMS envelope, output vs input. "Doubled" = a second envelope peak
  (2 ms RMS) within 50 ms, above −20 dB re the attack and 6 dB prominent, that the input does
  not have.
* White noise: 12 s, frame-averaged 1/3-octave spectrum 50 Hz–16 kHz, max |deviation|.
* Stereo: octave bands 63 Hz–16 kHz, max |Δ correlation| and max |Δ L/R level|. Bands with
  < 1e-5 of the energy are skipped. The synthetic signal is 30 s: common noise with a 0.4 ms
  inter-channel delay plus independent noise and panned tones. The per-band correlation of a
  noise-like signal is itself an estimate; at 10 s its standard error in the 63 Hz octave was
  ~0.035 for the difference of two realisations.
* Tonal attacks (informational): plucked 8-harmonic notes ringing into each other. Timing
  uses a 6 ms envelope, which resolves only ~±3 ms on beating tones. "Pre-echo excess" is the
  output envelope over the time-mapped input envelope, in the 50–4 ms before each attack.
* CPU = process CPU time (`/proc/self/stat`) per minute of output, stereo 44.1 kHz, single
  thread, on the real mix if present.

## Known weaknesses

* **Without an onset list, `full` has no attack placement.** P then goes to the PV, so `full`
  behaves like `pv` on attacks.
  * Clicks are not doubled but are smeared: pre-echo about −30 dB re peak at R = 2. Drums
    land within 2 ms.
  * Tonal attacks show a ~42 dB pre-echo excess.
  * There is no internal onset detector yet. `hptsm` doubles 16 of 16 clicks at R = 2.
* **Harmonic attacks are still soft compared with percussive ones.**
  * With onsets: tonal attack timing 3.6–4.9 ms; pre-echo 6–7 dB above the input in the 50 ms
    before the attack.
  * Without onsets: 38–50 dB, as in every PV.
  * The 4096 window is long for attacks; a multi-resolution PV near onsets is the obvious next
    step.
* **Soft onsets are left to the PV.** Onsets weaker than 0.25 or less percussive than 0.3
  (most sung onsets) get no reset or gating, so they smear like in any PV: a pre-swell of up
  to R·L/2 before the note.
* **Short transients inside H** (e.g. vocal plosives) come out 2–3 dB hot at R < 1. The W/T
  normalisation assumes a component that continues after the onset.
* **The PV loses energy on noise-like content** as R moves away from 1: about −0.7 dB at
  R = 2 and up to −2..−4 dB in sparse low bands of vocal stems at R = 2.5. The OLA
  normalisation is exact only for coherent frames. The `full` noise path avoids this for
  material classified as residual; `full` on a dense mix is −0.3 dB at R = 0.8 and −0.8 dB at
  R = 2 overall.
* **Peaks rise on limited masters.** Phase changes destroy a limiter's peak shaping: the
  "Master" mix peaks 0.61 → 0.73 at R = 2. Output is float, so nothing clips in the file.
* **Decorrelated stereo** is handled statistically (shared ψ in the PV, covariance in the noise
  path, shared WSOLA shifts). Per-band correlation stays within 0.05 in all checks, but there
  is no explicit covariance correction. The worst `full` case is 0.026 (synthetic, R = 2).
  * A per-channel phase-drift correction in the PV was tried and made it worse, so it was
    reverted.
* **R = 1 special cases.** At R = 1 the noise path and WSOLA search are switched off and the
  plain parts are used, so `R = 1` is near-exact (> 140 dB). Ratios very close to 1 take the
  general path.
* **The split is a median filter.** Vocals with vibrato and dense mixes are separated coarsely.
  Every downstream path is built to degrade gracefully, so a bin in the "wrong" path is still
  stretched reasonably.
* **`full-2way`** has seen less tuning (its levels were checked on two mixes only).
* **Tuned on few signals.** The thresholds were each set against a measured failure on a
  handful of synthetic signals and five or six of the material files:
  * β, 0.7 lobe threshold, 12 dB low-peak rule;
  * reset ratio 2, strength 0.25, percussiveness 0.3;
  * WSOLA 0.6 / ±1 dB.

  They are not cross-validated, and the listening test is the real judge.
* **Speed** is research-grade.
  * `full`: ~7 CPU-s per output minute at R = 2 and ~13 at R = 0.5.
  * The median split and the PV dominate. Nothing is parallelised and nothing is SIMD-tuned.

## Next steps

1. Internal onset detector (spectral-flux / phase-deviation), so `full` does not depend on the
   external list.
2. Multi-resolution PV near onsets (e.g. 1024-sample frames for new bins) to sharpen harmonic
   attacks.
3. A learned separator behind `Separator`, e.g. a 3-stem harmonic / percussive / noise model.
   The energy normalisation would then need a re-analysis of the stems, or the model's masks.
4. An energy-preserving correction for the PV on noise-like bins, e.g. a coherence-dependent
   gain from a cheap re-analysis of the output.
5. Optional explicit per-band covariance matching of the output to the stretched input
   covariance, if listening shows image drift.
6. Speed: parallel channels and blocks, a cheaper median, and the split hop.

## Clean-room record

### Sources read during this work

* https://arxiv.org/abs/2202.07382: abstract page of Průša & Holighaus, "Phase vocoder done
  right".
* https://arxiv.org/html/2202.07382v1: HTML full text of the same paper.
* https://arxiv.org/pdf/2202.07382: fetched, but the PDF was not readable as text.
* https://raw.githubusercontent.com/meinardmueller/libtsm/master/libtsm/tsm.py: libtsm (MIT,
  AudioLabs Erlangen), the Python reference of OLA / WSOLA / PV / HPS-TSM. Used only for the
  baseline's parameters.
* All other papers in the table above were used from prior knowledge; nothing else was
  fetched.

### Statement

No Rubber Band, zplane élastique, or Bungee material was read. That covers source code,
documentation and technical notes, and no search result leading there was opened. The
Signalsmith Stretch blog post was not read either. Nothing under `/home/user/Musicable`,
`/tmp/claude`, `/mnt/project-files` or `/mnt/user-data` was read. All code in this crate was
written for it. The input material in `/home/user/stretch-material` is copyrighted and is not
part of the repository: `*.wav` is git-ignored.

## Layout

| path | content |
|---|---|
| `src/lib.rs` | variants, options, `process` (stretch + pitch) |
| `src/split.rs` | `Separator` trait, median HPR split, energy parts, onset reshaping |
| `src/pv.rs` | PGHI phase vocoder with onset model |
| `src/ola.rs`, `src/timemap.rs` | placed OLA / WSOLA, onset-anchored time map |
| `src/noise.rs` | residual resynthesis |
| `src/classic_pv.rs` | phase-locked PV for the `hptsm` baseline |
| `src/resample.rs` | Kaiser-windowed sinc resampler |
| `src/checks.rs` | test signals, metrics, the check table |
| `src/main.rs` | CLI |
| `tests/checks.rs` | integration tests asserting the check limits |
| `examples/` | `levels`, `bands`: diagnostics on real files |
