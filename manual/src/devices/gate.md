# Gate

Turns the signal down to the **Floor** while it's quieter than the
**Threshold**. It opens as soon as the louder channel's peaks reach the
threshold, and closes once they've stayed more than **Return** decibels
below it for the **Hold** time, so it doesn't chatter on a level that
hovers around the threshold.

It fades open over the **Attack** and closed over the **Release**. A floor
of -80 dB is all but silent, and a higher one lets some of the signal
through.

## Controls

{{#include ../generated/devices/gate.md}}
