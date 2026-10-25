# Utility

Gain, balance, stereo width, polarity, mute and a DC filter. The signal goes
through them in this order: the **DC Filter**, a high pass at 5 Hz, well
below hearing; **Invert L** and **Invert R**, which flip each channel's
polarity; the **Width**; the **Pan**, which works as the mixer's does; and
the **Gain**.

A width of 0% is mono, 100% leaves the signal as it is, and 200% doubles
the difference between the channels. **Mute** and the switches fade, so
nothing clicks.

## Controls

{{#include ../generated/devices/utility.md}}
