# Setting up audio

Musicable plays through one audio device, which the Settings window's
**Audio** page picks: **Options > Audio Settings...** opens it, and so does
a click on the audio readout in the status bar, such as "48 kHz · 128
samples · 10.7 ms". Inputs for recording are set on each track
([Recording and comping](../using/recording.md#inputs)).

## The Audio page

- **Driver type**, on Windows: **WASAPI**, Windows' own audio, which plays
  through any device, or **ASIO**, for an audio interface with an ASIO
  driver. ASIO drivers talk to their hardware themselves, for the lowest
  latency, and other apps can't play through the device while Musicable
  has it.
- **Device**, or **Driver** for ASIO: **Default output**, the system's
  default output device, or another. **Refresh** lists them again after
  plugging one in.
- **Mode**, for WASAPI: **Shared**, where other apps can play at the same
  time, or **Exclusive**, which gives the device to Musicable alone, for
  the lowest latency, while other apps go silent. Not every device offers
  it.
- **Sample rate**: in shared mode, the rate the system's sound settings
  give the device. In exclusive mode and with ASIO, it's a menu of the
  rates the device can switch to. Switching an ASIO driver's rate switches
  it for everything its hardware plays.
- **Buffer size**: smaller buffers play sooner after a key press, and take
  more CPU. It starts at the smallest the device supports, or with ASIO, the
  size the driver prefers. Drivers round the size to one they take, and many
  ASIO drivers take only the size set in their own settings: **Driver
  Settings…** opens them, and when they change, Musicable opens the driver
  again with them.

**Apply** plays through what the page shows, and **Revert** goes back to
what's playing. If the new device doesn't open, the old one plays on, and
the page says why. **Now playing**, below, says what Musicable got: the
device, its rate, its channels and samples, and how many samples it asks
for at a time.

With ASIO, Musicable plays through the driver's first two outputs, the
first analog outputs on most interfaces. Only one ASIO driver can play at a
time.

## Render Ahead

**Render Ahead**, under the device's settings, lets a small buffer play a
heavy mix. Musicable renders the project ahead of the device, on all its
cores, and each buffer the device asks for is ready already. A plugin can
then take longer than a buffer lasts now and then without a dropout, as
long as the project keeps up on the whole. A long chain of plugins on one
track, as on the master, renders on several cores too: Musicable splits
what it renders ahead into parts that run down the chain at once, each
plugin rendering one part while the plugin after it renders the part
before.

- **Off**: every block renders as the device asks for it, within its
  buffer.
- **Short**, about 20 ms ahead.
- **Normal**, about 50 ms ahead, as Musicable starts.
- **Long**, about 200 ms ahead, the most room, for the heaviest mixes.

The further ahead, the later you hear what you change by hand while the
song plays, up to that long after: a knob you turn, Stop, or a jump. While
the transport stands still, Musicable keeps only about 512 samples ahead,
so Play starts soon.

A track that can monitor an audio or MIDI input, with its monitoring at
**In**, or at **Auto** while it's armed
([Monitoring](../using/recording.md#monitoring)), renders live, as the
device asks for each buffer, so what you play reaches your ears at the
buffer's latency. So does everything it plays into: the buses its sends go
to, a track or a sidechain taking it as an input, and the master, whose
knobs then change what you hear at once. The rest of the project still
renders ahead, and the live part plays on top of it, with two of
Musicable's threads kept for it besides the audio device's, so the rest
doesn't hold it up. A track taking its input from another track renders
live only if that track does. Exports aren't played through the device,
so Render Ahead makes no difference to them.

**Always Live**, in a track's right-click menu in the arrangement or the
mixer, or **Track > Always Live** for the selected tracks, has a track
render live whatever its input, with what it plays into: for a plugin
whose window has to keep time with what you hear, such as an analyser, or
knobs you want to hear at once. Each track that renders live while the
rest renders ahead has a **LIVE** mark at the end of its name, and its
tooltip says why, such as "Live: fed by the armed track Vocal". The
project keeps it, and Undo takes it back.

A plugin that says it must render live, as one that talks to hardware
does, renders live too, with what it plays into, and so does one set to
**Never** in its **Render Ahead**, for a plugin that misbehaves rendered
ahead ([Plugins](../using/plugins.md#rendering-ahead)). Its track's LIVE
mark says so, such as "Live: Outboard says it must render live".

Render Ahead changes at once, with no **Apply**, since the device needn't
open again.

## When a device goes away

- If the device in the settings isn't plugged in as Musicable starts, it
  plays through the default output instead, and says so. The settings keep
  the device for next time.
- When the device playing is unplugged, its format changes in the system's
  sound settings, or the system's audio restarts, the status bar says it
  stopped playing, and why. Musicable tries it again at once, then every
  two seconds, and says when it plays again.
- An ASIO driver whose own settings change opens again with them, and the
  status bar says what it plays with now.
- **Default output** stays on the device it opened when the system's
  default changes, until that device goes away or you apply the settings
  again.

## Latency

The status bar shows the sample rate, the buffer size, and the latency as
the device's clock measures it: how long after Musicable plays a sound you
hear it. Exclusive mode and ASIO have the least. In shared mode, Windows'
audio engine adds its own, and holds more than it says, which only the
device's clock shows.

Recording makes up for the latency, so a take lands where you played it.
Latency the driver doesn't report, such as an interface's converters', is
the recording offset's to make up for ([Where it
lands](../using/recording.md#where-it-lands)).

## The CPU meter and the dropouts

The right end of the transport bar has the CPU meter and the dropout
counter:

- **CPU** is how much of the time the audio thread spent working, rendering
  the song included, over the last quarter of a second. Near 100%, it
  starts running late, and dropouts follow. Its tooltip adds the longest it
  took to wake up and render, how many threads Musicable kept busy at once,
  and the share of a core the project took, and how much of it rendered
  ahead, which the [Performance panel](../window/performance.md) breaks
  down by device.
- The **lead gauge**, the thin bar under the CPU meter while Render Ahead
  is on, is how much of the lead Musicable aims for it kept ahead of the
  device over the last quarter of a second: full is all of it, and it
  starts again when you change Render Ahead. It turns
  amber under half, and red when the audio thread had to render blocks
  itself because the lead ran out, the step before a dropout. It dips for
  a moment as a track arms or disarms, while what renders live changes.
  The tooltip says how far ahead Musicable was, and how many blocks the
  audio thread had to render itself since the device started.
- **Dropouts** counts the times the device ran out of audio since it
  started, which you hear as clicks or gaps. Click it to count from zero
  again. Its tooltip says how much audio they lost, how many of them were
  in the system's audio engine or driver, and how often the audio thread
  woke up late.

To get rid of dropouts, try a bigger buffer, a longer Render Ahead,
exclusive mode or ASIO, and closing apps that keep the computer busy. Build
Musicable with `--release`, as [Installing Musicable](installing.md) does,
since other builds are much slower.
