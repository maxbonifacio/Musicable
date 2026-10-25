# Compressor

Turns the signal down by the **Ratio** once it's louder than the
**Threshold**. It follows the louder of the two channels, so both move
together, and has a soft **Knee**, that many decibels wide around the
threshold, where the ratio comes in gradually.

**Attack** and **Release** are how long it takes to turn the signal down
and to let it back up, most of the way. The **Detector** follows the
signal's peaks (Peak), or its power over the attack time (RMS), which
responds a little more slowly. **Output** comes after, and **Mix** blends
the compressed signal with what came in, for parallel compression.

## Controls

{{#include ../generated/devices/compressor.md}}
