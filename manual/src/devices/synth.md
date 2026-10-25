# Synth

Two oscillators and noise, through a filter with its own envelope, into an
amp envelope. Each voice mixes **Osc 1**, a choice of octave, **Osc 2**, in
semitones and cents from it, and white noise, each at its own level.

The filter's **Cutoff** moves by the **Envelope** amount, in semitones, at
the filter envelope's peak, and follows the notes by the **Key Track**
amount: at 100% it moves with the note's pitch. **Resonance** goes from
none to just short of self-oscillation.

**Velocity** sets how far a note's velocity turns its level down from full,
notes slide from the last note's pitch over the **Glide** time, and a note
past the number of **Voices** takes the place of the voice let go longest
ago. Each note's pitch bend moves its oscillators.

## Controls

{{#include ../generated/devices/synth.md}}
