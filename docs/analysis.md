# Analysis

The `analysis` crate finds what warping needs to know about a sound: where
its sounds start, its tempo, its beats and downbeats, and whether it has a
pulse at all. It finds beats with the Beat This! network, and checks them
with a classic tracker, which finds them alone in Fast analysis or when the
network's files aren't there. It's plain functions on audio, with no UI.

So far only `musicable analyse` runs it, to measure it against labelled
files and against Live's auto-warp ([Measuring it](#measuring-it)). The
app's background jobs, the cache and auto-warp come next.

## What it finds

`analyse` reads a sound twice, start to end, 16,384 frames at a time, so a
long file is never in memory at once. The first pass works out the spectral
flux and feeds the network as it goes. The second finds each onset to the
sample. The `Analysis` it returns holds:

| Field | What it is |
|---|---|
| `onsets` | Each transient: the frame its attack starts at, its strength from 0 to 1, and how percussive it is, from 0 for a soft start to 1 for a hit |
| `tempi` | The tempos the flux repeats at, likeliest first, each with how strongly it repeats there |
| `beats`, `downbeats`, `meter` | The beats, as frames; which of them start a bar; and the beats to a bar |
| `tracker`, `agreement` | Which tracker found the beats, and how well the classic tracker's agree with the network's, from 0 to 1 |
| `steady` | The straight line through the beats: its tempo, how far the beats stray from it, and how far their tempo drifts from it |
| `breaks` | How often the beat slips by part of a beat, where the tracker lost it and found it again off the beat |
| `pulse`, `events`, `riser`, `sync` | Whether it has a beat that goes on. Without one: how many separate strong events it has, 1 for a one-shot; whether it builds to a peak; and the frame that should land on the grid, the first strong attack or a riser's peak |
| `loop` | For a loop at a steady tempo that starts on a beat and lasts whole bars, up to 32: how many bars, and the exact tempo its length gives |

`VERSION` goes up when what it finds changes, so the cache can analyse a
sound again.

## Onsets

The flux is log-filtered spectral flux (Böck, Krebs and Schedl, 2012) with
SuperFlux's vibrato suppression (Böck and Widmer, 2013). Frames are 10 ms
apart, with a Hann window of 2048 samples at 44.1 kHz and the power of two
nearest 46 ms at other rates. Triangular filters, a semitone apart from
27.5 Hz to 16 kHz, sum the spectrum, which is taken as `log10(1 + x)`. Each
filter's rise is against the loudest of it and its two neighbours in the
frame before, so vibrato doesn't read as a rise.

An onset is a peak of the flux: the highest frame within three either side,
above the mean of the ten frames before and seven after by 2% of the
largest peak, and at least three frames after the last. The first frame
rises from the silence before the file, so it counts for a tenth towards
the largest.

Each one is then found again to the sample, in the band where its flux rose
most: below 200 Hz, 200 Hz to 4 kHz, or above, split by cascaded one-pole
filters. In that band, a peak envelope with a 10 ms release is read from
20 ms before the frame to 30 ms after. The onset is where the envelope's
rise over its loudest of 2 to 15 ms before is largest, moved back to where
that rise first reached half its size. On the synthetic grooves in the
tests, 99% of onsets land within a millisecond. Percussiveness is how soon
the attack peaks, from 1 within 2 ms to 0 at 40 ms, scaled down for a rise
of less than 9 dB.

## Tempo

The tempo candidates come from the flux's autocorrelation, with its moving
mean over a second taken away, between 40 and 240 BPM. Each lag's
correlation is weighted towards 120 BPM, as librosa's is, by a Gaussian of
its distance in octaves with a width of one octave, and each peak, refined
with a parabola, is a candidate. The half and double tempos stay in the list, for least stretch
to choose between. A candidate's `periodicity` is its correlation
unweighted, near 0 for a sound with no pulse.

## Beats

With the network, the beats are its beats, kept in step:

1. **In step.** The network's beats sometimes run at double time for a
   stretch, or have strays. A chain through them keeps the beats that best
   follow the beats' local period. Each beat adds 1, and each step from
   one to the next loses 30 times the square of how far it strays from a
   whole number of periods, as a fraction of a period, and 0.5 for each
   beat it skips, counting two at most, so a break with no beats costs no
   more than a short gap.
2. **Breaks.** A gap in the chain more than a quarter of a period from a
   whole number of periods is a break, where the network lost the beat and
   found it again off it. They're counted before smoothing, which would
   spread them over the beats around.
3. **Smoothed.** Each beat moves to where a parabola through the eight
   beats either side puts it, which evens out the network's jitter from
   beat to beat and follows the tempo as it moves.
4. **Snapped.** Each beat moves to the onset nearest it within 40 ms.

The downbeats are the beats the network's downbeats land on, within 40 ms,
with any bar it missed between two of them filled in, and the bars before
the first and after the last. The meter is the commonest bar. The classic
tracker then runs at the network's tempo, and the agreement is the
F-measure of its beats against the network's, within 70 ms, at the same
tempo, at twice it or at half, whichever is best.

Without the network, the classic tracker's beats stand alone, at the first
tempo candidate. It's Ellis's dynamic programming tracker (2007), with
librosa's tightness of 100, moved to onsets within 30 ms, and without the
beats it counts on into the silence before the first onset and after the
last. Its downbeats are every fourth beat from the first. As it finds beats
in anything, a sound has a pulse by it only if the flux repeats at its
tempo, with a periodicity of 0.15 or more, and half its beats or more land
on onsets.

A sound has a pulse if it has three strong events or more, and at least
four beats, 80% of them a whole number of local periods apart, spanning at
least half of it. Its tempo is steady if the beats' smooth curve drifts
less than 10 ms from the straight line through them.

## The network

[Beat This!](https://github.com/CPJKU/beat_this) (Foscarin, Schlüter and
Widmer, 2024) is a transformer that finds beats and downbeats in music of
any style. Musicable runs it through the
[beat-this](https://crates.io/crates/beat-this) crate, a port to Rust on
the pure-Rust [rten](https://crates.io/crates/rten) runtime, both under the
MIT licence. The crate runs our own model type, so the network runs on a
thread pool of its own, whose threads each run a start function first, as
the app will to lower their priority.

It needs two model files, which the repository leaves out: the log-mel
spectrogram it listens to, and the network, the full `final0` checkpoint as
ONNX, 83 MB. [`scripts/fetch-models.sh`](../scripts/fetch-models.sh), or
`fetch-models.ps1` in PowerShell, fetches them from beat-this-rs's
repository and its `model-large` release into `models/`, which git ignores,
and checks each against the SHA-256 it pins. CI caches them until the script
changes.

Analysis looks for the files in the folder `MUSICABLE_MODELS` names, or
else in a `models` folder beside the program or up to three folders above
it, where a build in the repository's `target` folder finds those the
script fetched. Without them, it's the classic tracker alone.

The checkpoints carry no licence of their own yet, which is why they're
fetched rather than committed. Before a release ships them, we ask the
authors to license them, or train the network again on data we may use.

A song of two minutes takes about 10 s on four cores, in a debug build:
the workspace builds the network's crates optimised in debug builds too,
without which it takes minutes.

## Measuring it

The tests check each part on synthetic grooves with known beats, and the
network on one of them when the model files are there. Without them, that
test skips, saying so, except in CI, where it fails.

`musicable analyse FILE...` analyses files as warping does and prints what
it found, and scores the beats against labels where a file has them: the
F-measure of its beats and its downbeats, within 70 ms, as beat tracking is
usually scored. The labels are a `.beats` file beside the audio, with a
line for each beat, its time in seconds and then its place in the bar,
where 1 starts a bar, as Sonic Visualiser exports them. Or they're the warp
markers in a Live Set warped by hand, with `--labels SET`. `--live SET`
scores Live's auto-warp of the same files too, from the warp markers Live
put in a Set, so the two can be compared on the same files. A Live Set's
bars are taken as 4/4 from its 1.1.1. The
[manual](../manual/src/reference/command-line.md#analysing-files) has the
options.
