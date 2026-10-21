# Audio devices

How `crates/audio` gets the engine's output to a device: the backends, opening
a stream, periods and latency, counting dropouts, and what happens when a
device goes away.

## Backends

| Platform | Backend | Modes |
|---|---|---|
| Windows | Our own WASAPI backend | Shared (the default) and exclusive |
| macOS | cpal, CoreAudio | Shared only. Builds and plays, not tuned |
| Linux | cpal, ALSA | Shared only. Builds and plays, not tuned |

Windows doesn't go through cpal. On an RME interface, a spike measured cpal's
WASAPI host at about 50 ms round trip, and under load it dropped audio without
reporting it. A plain WASAPI client of our own on the same 10 ms period took
28 ms and stayed sample-exact under load, and exclusive mode took 15 to 18 ms.
cpal also has neither exclusive mode nor IAudioClient3, which gets onboard
audio down to periods of a few milliseconds in shared mode.

**Shared mode** mixes our audio with other apps'. Where the driver offers
periods below the audio engine's default, the stream sets one up through
IAudioClient3: the smallest by default, or the requested one rounded up to a
multiple of the driver's fundamental period. When another app already runs the
engine at a period, the stream joins in at that one. Drivers that offer
nothing smaller get the classic shared stream at the default period, usually
10 ms. A shared stream runs at the rate the device is set to in the Windows
sound settings, and keeps two periods queued, or the whole buffer if it holds
less.

**Exclusive mode** has the device to itself: other apps go silent, and the
stream picks the rate and sample format (24 bits in 32 first, then float, 32,
packed 24 and 16 bits). It's event-driven, with two buffers the device
alternates between, each one period long. Some drivers keep to their own
buffer setting whatever the stream sets up: the RME HDSPe AIO, asked for 132
frames, wakes the stream every 256. Writing less than such a driver plays
breaks the audio up, so the stream checks first: it plays silence for 24
wake-ups, and if the median interval is more than 1.3 times the period it set
up, it opens again at the period the driver delivers. A driver that still
doesn't match fails with `Error::PeriodMismatch`. With such a driver, set the
buffer size in its own settings panel.

## Opening a stream

Opening takes two steps, because the engine is built for what the device
actually does, which isn't always what was asked for.

```rust
let open = musicable_audio::open(&StreamRequest::default())?;
let config = open.info().engine_config();
let (engine, mut handle) = Engine::new(config);
handle.swap_graph(graph.prepare(&config)?)?;
let stream = open.start(engine)?;
// ... later, or once `stream.has_ended()`:
let Stopped { engine, error } = stream.stop();
```

- `open` opens the device and says what it got in `StreamInfo`: sample rate,
  channels, sample format, period, buffer and latency, and whether the audio
  thread runs at real-time priority.
- `StreamInfo::engine_config` has the stream's rate, and its period as
  `max_block`. The engine must run at the stream's rate: `start` hands back an
  engine that doesn't in `StartError`, with `Error::EngineRate`.
- `StreamRequest` takes a device id (`Device::id` from `output_devices`, which
  stays the same across runs), the mode, a sample rate and a period. `None`
  picks the default output, the device's rate and the smallest period.
  `capabilities` says what a device supports before opening it.
- Dropping an `OpenStream` closes the device. Dropping a `Stream` stops it and
  drops the engine on the dropping thread.

## Periods and latency

- `StreamInfo::period` is the number of frames the device asks for at each
  wake-up: the period it really delivers.
- `StreamInfo::buffer` is the device buffer: in shared mode the most the
  stream can queue, in exclusive mode each of the two halves.
- `StreamInfo::latency` is the output latency estimated from the buffer sizes:
  how far ahead of the play position a punctual wake-up starts writing, plus
  the delay the system reports beyond that (`GetStreamLatency` in shared
  mode). In exclusive mode that's one buffer; in shared mode, what's left of
  the two queued periods plus the audio engine's delay. It leaves out
  converter delay.
- `StreamStats::queued` measures the same thing while the stream runs: the
  frames written but not yet played, by the device's clock, at the last
  wake-up before it wrote.
- There are no input streams yet, so no input latency either. They come with
  recording.

## Dropouts

A dropout is the device running out of audio. The Windows backend reads the
device clock (`IAudioClock::GetPosition`, with the QPC time of the reading) at
each wake-up, before writing, and looks for two signs:

- **The device clock fell behind the wall clock** by more than half a period
  since the last wake-up. A device with nothing of ours to play stops counting
  our position, and so do some exclusive-mode drivers. In shared mode the gap
  only counts if the buffer had run low (less than a period queued) since the
  wake-up before last, at a wake-up or as a write landed. That's where a
  render that took too long shows, and with audio still queued the gap is
  jitter in the clock readings.
- **The device played past what was written.** Most exclusive-mode drivers
  keep counting while they replay whatever the buffer held, so their clock
  never falls behind.

A dropout takes a few wake-ups to recover from while the buffer refills, so
signs in the 10 wake-ups after one count towards the same dropout.
`src/xrun.rs` has the details, and tests that simulate each kind of clock.

Each dropout counts once in `StreamStats::underruns` and once in the engine's
`Stats::xruns` (`Engine::record_xrun`). `StreamStats::lost_frames` adds up
the audio they lost. `late_wakeups`, `max_interval` and `max_busy` show how
close the stream came: wake-ups more than one and a half periods apart eat into
the queued audio, but aren't dropouts unless it runs out. `busy` adds up the
time all wake-ups took, so its growth over a stretch of time, as a share of
it, is the load a CPU meter shows.

On macOS and Linux, a dropout is whatever cpal reports as an xrun: ALSA
underruns and CoreAudio overloads. cpal doesn't say how much audio they lost.

## When the device goes away

When the device is unplugged or disabled, its format is changed in the sound
settings, or the audio service restarts, the stream ends by itself:
`Stream::has_ended` turns true, and `Stream::stop` hands back the engine with
the reason, such as `Error::DeviceLost`. A device that stops waking the stream
without reporting anything ends it with `Error::Timeout` after two seconds.

To carry on, open the same `StreamRequest` again until it works, as the tone
example does once a second, and the app every two seconds
([app.md](app.md#audio)). Reuse the engine if the new stream runs at the same
rate. Otherwise create a new engine for the new `engine_config`, and set it up
as [engine.md](engine.md#notes-for-the-next-milestones) describes: tempo map,
loop, and a locate to where the old engine was.

A stream on the default output stays on the device it opened when the default
changes, until that device goes away. Following the default is still to do.

## The audio thread

Each Windows stream has its own thread, named `musicable-audio`. It joins COM's
multithreaded apartment and the MMCSS "Pro Audio" task at critical priority,
and does all its WASAPI calls itself, from opening the device to closing it.
The engine reaches it through a channel at `start` and comes back with the
thread at `stop`, so it's created and dropped on the control thread. The rest
of each wake-up, from the clock reading to releasing the buffer, runs inside
`rt::scope` and follows the [real-time rules](engine.md#real-time-rules):
errors are carried out of the scope before anything formats a message.

The engine renders straight into the device buffer when it takes 32-bit float,
and into a buffer of its own to convert from when it takes integers.

## Trying it

The tone example plays a sine wave and prints the stream's counters as it goes:

```sh
cargo run --release -p musicable-audio --example tone -- --list
cargo run --release -p musicable-audio --example tone -- --period 128 --seconds 3600
cargo run --release -p musicable-audio --example tone -- --exclusive --period 128 --seconds 3600
```

It exits with an error if there was a dropout. `--stall-every 5` holds up the
audio thread for five periods every five seconds, to see that each one is
counted. `--device` takes an id or part of a device name, and `--help` lists
the rest. When the device goes away mid-run, it keeps trying to open it again.

The tests in `crates/audio/tests/device.rs` that play need an output device,
so they're ignored by default:

```sh
cargo test -p musicable-audio --test device -- --ignored --test-threads 1
```

On Linux, building needs the ALSA development files (`libasound2-dev` on
Debian and Ubuntu).

## Not done yet

- ASIO, as an opt-in GPL build feature. It moves to Tier 1, which the plan
  allows.
- Input streams, and the input latency that comes with them.
- Following the default output when it changes.
- Tuning on macOS and Linux.
