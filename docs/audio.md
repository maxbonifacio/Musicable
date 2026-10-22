# Audio devices

How `crates/audio` gets the engine's output to a device: the backends, opening
a stream, periods and latency, counting dropouts, what happens when a device
goes away, and ASIO.

## Backends

| Platform | Backend | Modes |
|---|---|---|
| Windows | Our own WASAPI backend | Shared (the default) and exclusive |
| Windows, with the `asio` feature | Our own ASIO host as well ([ASIO](#asio)) | One: the driver's |
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
  stream can queue, in exclusive mode and with ASIO each of the two halves.
- `StreamInfo::latency` is the output latency estimated from the buffer sizes
  as the stream opens: how far ahead of the play position a punctual wake-up
  starts writing, plus the delay the system reports beyond that
  (`GetStreamLatency` in shared mode). It leaves out converter delay. In
  exclusive mode that's one buffer, and the device's clock agrees. In shared
  mode it's what's left of the two queued periods plus the audio engine's
  delay as the system reports it, and that can be a few periods short: the
  audio engine and the driver hold more than the system says. On onboard
  audio at 128 frames the system reported no delay at all, so the estimate
  said 2.7 ms where the device's clock measured 8 to 11 ms.
- `StreamStats::queued` measures the latency while the stream runs: the
  frames written but not yet played, by the device's clock, as the stream
  writes. It's the median of each block of 16 wake-ups, so a late wake-up
  doesn't show. Once the stream runs, use it rather than the estimate, as
  the app does.
- With ASIO, both are the output latency the driver reports
  ([ASIO](#asio)).
- There are no input streams yet, so no input latency either. They come with
  recording.

## Dropouts

A dropout is the device running out of audio. The Windows backend reads the
device clock (`IAudioClock::GetPosition`, with the QPC time of the reading) at
each wake-up, before writing, and looks for three signs:

- **The device clock fell behind the wall clock** by more than half a period
  since the last wake-up. A device with nothing of ours to play stops counting
  our position, and so do some exclusive-mode drivers. In shared mode the gap
  only counts if the buffer had run low (less than a period queued) since the
  wake-up before last, at a wake-up or as a write landed, and a wake-up in
  that time came late. A write landing low is where a render that took too
  long shows, and with audio still queued the gap is jitter in the clock
  readings. The audio engine wakes the stream at every pass, and the stream
  keeps two periods queued, so the buffer only runs dry when a wake-up comes
  late, or a render takes so long that the next one does.
- **The device played past what was written.** Most exclusive-mode drivers
  keep counting while they replay whatever the buffer held, so their clock
  never falls behind.
- **The device fell behind the audio engine for good**, in shared mode. The
  audio engine takes our audio from the buffer a period at a time, and the
  device plays it a steady delay later: the frames taken from the buffer
  minus those played hold steady while all is well. When Windows' audio
  engine or the driver runs late, the device plays something else for a
  moment while our buffer still holds audio, and plays ours that much later
  from then on. The clock gap that leaves looks like the jitter above, so
  the shift is what counts: the medians of two blocks of 16 wake-ups in a row
  more than half a period from where the delay held, with a late wake-up in
  the 48 before. The audio engine wakes the stream at each pass, so when it
  runs late, the stream wakes late too.

The late wake-ups are there for devices whose clocks drift against the
audio engine. Through the RME HDSPe AIO's WDM driver at 10 ms, the card
played about 8 parts per million faster than the audio engine took our
audio, so the delay crept down by 22 frames a minute, and every 22 minutes
the audio engine took an extra period to catch up. For a few minutes around
each catch-up, as readings land either side of the audio engine's passes,
the delay's medians jump back and forth by a period and the buffer can read
low. None of that comes with a late wake-up or loses audio. Before either
sign needed one, an hour on the RME counted 134 dropouts in bursts around
the catch-ups, without a single late wake-up, and music played through the
card during the first burst had no clicks. Needing one, half an hour on the
RME crossed a catch-up, nine and a half minutes in, and counted none.

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

The dropouts downstream, in the system, also count in
`StreamStats::system_dropouts`, and each one moves the output latency by the
audio it lost. In an hour of shared mode on onboard audio at 128 frames, the
latency stepped up by a period three times, each time at a late wake-up, from
11 to 19 ms, and stayed there for the rest of the run. Exclusive mode, with
no audio engine in between, held steady over the same hour. A later hour on
the same output counted one system dropout, at a late wake-up, which moved
the latency from 8.6 to 11.3 ms. Half an hour on it, at the same time as the
RME's above, counted three, each at a late wake-up, and each moved the
latency up a period, from 8.6 to 16.6 ms in all.

On macOS and Linux, a dropout is whatever cpal reports as an xrun: ALSA
underruns and CoreAudio overloads. cpal doesn't say how much audio they lost.
ASIO streams look for signs of their own ([ASIO](#asio)).

## When the device goes away

When the device is unplugged or disabled, its format is changed in the sound
settings, or the audio service restarts, the stream ends by itself:
`Stream::has_ended` turns true, and `Stream::stop` hands back the engine with
the reason, such as `Error::DeviceLost`. A device that stops waking the stream
without reporting anything ends it with `Error::Timeout` after two seconds. An
ASIO driver whose own settings change asks to be reset, which ends the stream
with `Error::DriverReset`.

To carry on, open the same `StreamRequest` again until it works, as the tone
example does once a second, and the app every two seconds
([app.md](app.md#audio)). Reuse the engine if the new stream runs at the same
rate. Otherwise create a new engine for the new `engine_config`, and set it up
as [engine.md](engine.md#notes-for-the-next-milestones) describes: tempo map,
loop, and a locate to where the old engine was.

A stream on the default output stays on the device it opened when the default
changes, until that device goes away. Following the default is still to do.

## The audio thread

Each WASAPI stream has its own thread, named `musicable-audio`. It joins COM's
multithreaded apartment and the MMCSS "Pro Audio" task at critical priority,
and does all its WASAPI calls itself, from opening the device to closing it.
The engine reaches it through a channel at `start` and comes back with the
thread at `stop`, so it's created and dropped on the control thread. The rest
of each wake-up, from the clock reading to releasing the buffer, runs inside
`rt::scope` and follows the [real-time rules](engine.md#real-time-rules):
errors are carried out of the scope before anything formats a message.

The engine renders straight into the device buffer when it takes 32-bit float,
and into a buffer of its own to convert from when it takes integers.

ASIO streams render on the driver's own thread instead ([ASIO](#asio)).

## ASIO

Windows builds with the `asio` feature play through ASIO drivers too, with a
host of our own in `src/asio/`:

```sh
cargo build --release -p musicable-app --features asio
```

Nothing else needs installing: no SDK, and no LLVM for bindings. The driver
interface is written from Steinberg's ASIO 2.3 specification, in our own
words, and the drivers come with the hardware. Builds are 64-bit only.

**License.** The source stays MIT OR Apache-2.0, as the rest of the
repository. Builds with the feature are distributed under the GNU GPL,
version 3, as the project decided for ASIO, and the app shows the notice the
GPL asks for at the foot of its audio settings in those builds. The default
build has no ASIO code: the module only compiles with the feature, and CI
checks that the app's default build doesn't turn it on. ASIO is a trademark
of Steinberg Media Technologies GmbH.

**Drivers as devices.** `output_devices` lists each ASIO driver installed for
64-bit apps (under `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO`, with its COM class
registered for 64-bit) as a `Device` with `driver_type: DriverType::Asio`, and
an id of `asio:` and its class id, which stays the same across runs.
`DriverType::of` tells an ASIO driver's id from another device's in any
build, and `driver_types` says which types a build plays through.

**What a stream gets.** A stream plays through the driver's first two
outputs, which drivers make their most useful pair: the first analog outputs,
on most interfaces. `capabilities` lists the common rates the driver can
switch to, and its buffer sizes: every one it takes, or for a driver that
takes many, the powers of two among them, with its smallest, largest and
preferred. With no `StreamRequest::period` the stream takes the preferred
size, which is the one set in the driver's own settings if it has any, and a
requested size is rounded up to one the driver takes. Many drivers, RME's
among them, only take the size set in their settings, so the stream gets
that one whatever it asks for. A requested rate switches the driver's rate,
for everything its hardware plays. `Mode` doesn't apply.

**Latency.** `StreamInfo::buffer` is each of the driver's two buffer halves,
a period long. `StreamInfo::latency` and `StreamStats::queued` are the output
latency the driver reports (`ASIOGetLatencies`): from a buffer switch until
the half written then plays. Drivers are asked to count their converters and
any buffers of their own in it. `queued` follows the driver when it says its
latency changed.

**Threads.** ASIO allows one driver per process, so there's one ASIO stream
at a time: opening a second fails. A stream loads its driver on a thread of
its own, `musicable-asio`, in a single-threaded COM apartment, as drivers
expect. That thread makes every call to the driver but the real-time ones,
from loading it to releasing it, and handles the window messages and requests
the driver sends meanwhile. The driver calls back on its own thread to have
each buffer half filled, and drivers run that thread at real-time priority.
The callback renders the engine there, inside `rt::scope`, and writes the
samples in each output's sample type: 16-, 24- and 32-bit integers in either
byte order, the 32-bit types with fewer bits in use, and 32- and 64-bit
floats. DSD isn't supported. A callback never waits for a lock: one that comes
while the last is still rendering, from a driver that calls again too soon,
leaves its half as it is. Both halves start silent, and some drivers ask for
the first one from inside `ASIOStart`, on the stream's thread. A panic in a
callback aborts the process, since it can't unwind into the driver.

**Dropouts.** At each buffer switch the stream looks for the signs of one:

- **The driver reported one**: an overload message, or a request to
  resynchronize because it lost audio.
- **The sample position jumped** by more than one and a half buffers since
  the last callback: the driver skipped buffer switches while the host ran
  late.
- **The driver asked for the same half twice in a row**, having skipped the
  switch to the other.
- **The last callback took longer than a buffer**, so the device played the
  half it was writing before it was done.

A driver that says it reports every dropout itself is taken at its word, as
the specification asks, and only its reports count. The first 8 callbacks
fill the device's buffers before it starts, so their timing means nothing yet
and the checks begin after them. Signs in the 10 callbacks after a dropout
count towards it, as on the other backends. `lost_frames` adds up what the
sample position skipped, or a buffer per sign when nothing says more.
`late_wakeups` counts callbacks more than one and a half buffers apart, and
`system_dropouts` stays at 0: there's no system audio engine in between.
`src/asio/detector.rs` has the details.

**The driver's settings, and resets.** `Stream::show_driver_settings` opens
the driver's own settings panel, from the stream's thread. When its settings
change, or its rate is changed from outside, the driver asks to be reset: the
stream ends with `Error::DriverReset`, and opening it again picks up the new
settings. The app does that at once, and says what the driver plays with now.

**Tests.** `src/asio/fake.rs` is a fake driver built in Rust, set up to behave
as drivers do, that records what the stream asks of it, and the tests in
`src/asio/tests.rs` play the driver's thread. They run on every platform with
`cargo test -p musicable-audio --features asio`: elsewhere than on Windows the
module builds only for them, with stand-ins for COM and for the event the
stream's thread waits on.

## Trying it

The tone example plays a sine wave and prints the stream's counters as it goes:

```sh
cargo run --release -p musicable-audio --example tone -- --list
cargo run --release -p musicable-audio --example tone -- --period 128 --seconds 3600
cargo run --release -p musicable-audio --example tone -- --exclusive --period 128 --seconds 3600
```

It exits with an error if there was a dropout, and says how many of them
were in the system. `--stall-every 5` holds up the audio thread for five
periods every five seconds, to see that each one is counted. `--device` takes
an id or part of a device name, and `--help` lists the rest. When the device
goes away mid-run, it keeps trying to open it again.

With the `asio` feature, `--list` shows the ASIO drivers as well, and
`--asio` plays through one: the one `--device` names, or the only one
installed. `--driver-settings` opens the driver's own settings once it
plays, to see a change there reset the stream and reopen it.

```sh
cargo run --release -p musicable-audio --features asio --example tone -- --list
cargo run --release -p musicable-audio --features asio --example tone -- --asio --seconds 3600
```

The tests in `crates/audio/tests/device.rs` that play need an output device,
so they're ignored by default:

```sh
cargo test -p musicable-audio --test device -- --ignored --test-threads 1
```

On Linux, building needs the ALSA development files (`libasound2-dev` on
Debian and Ubuntu).

## Not done yet

- An hour through an ASIO driver on real hardware, the RME HDSPe AIO's. Only
  the fake driver has played so far.
- ASIO's outputs past the first two, and its inputs, which come with
  recording, as do input streams and the input latency for the other
  backends.
- Choosing an ASIO driver's clock source. The driver's own settings do it
  for now.
- Packaging ASIO builds for release, with the GPL's text and the offer of
  the source.
- Following the default output when it changes.
- Counting glitches downstream that don't hold up the audio engine, such as
  a late driver on a device whose clock drifts against it. Without a late
  wake-up they look just like the audio engine catching up, so they go
  uncounted.
- Tuning on macOS and Linux.
