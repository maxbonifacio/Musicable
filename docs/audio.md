# Audio devices

How `crates/audio` gets the engine's output to a device, and what the device's
inputs capture back to the engine: the backends, opening a stream, periods and
latency, counting dropouts, what happens when a device goes away, inputs, MIDI
inputs, and ASIO.

## Backends

| Platform | Backend | Modes |
|---|---|---|
| Windows | Our own WASAPI backend | Shared (the default) and exclusive |
| Windows, ASIO drivers | Our own ASIO host as well ([ASIO](#asio)) | One: the driver's |
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
- Inputs have latencies of their own, which [Inputs](#inputs) takes care of:
  a recording lands where it was played, and `StreamStats::input_delay` says
  how far behind the output monitoring plays.

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

## Inputs

A stream records from the inputs `StreamRequest::inputs` lists, along with its
output, and lines each one up with the output's timeline: frame `w` of an
input's buffer holds what the input captured as engine frame `w` was heard. A
recording reads it there, so it lands where it was played, and the engine gets
it a fixed delay later, to monitor.

```rust
let request = StreamRequest {
    inputs: vec![input.id.clone()],
    ..StreamRequest::default()
};
let open = musicable_audio::open(&request)?;
let buffers = open.input_buffers(); // Some(InputBuffer) for each input that opened
let stream = open.start(engine)?;
// On any thread: frames `range.start..range.end` are there to read.
let range = buffers[0].as_ref().unwrap().frames();
```

- `input_devices` lists what a stream can record from: each input device
  that's plugged in and enabled (`InputKind::Device`), then on Windows each
  output device's loopback (`InputKind::Loopback`: `loopback_id` of the
  output's id), and each ASIO driver's inputs, which have the driver's own
  id. The ids stay the same across runs.
- `StreamInfo::inputs` says what each requested input opened as: its name,
  channels, rate and `InputClock`, and its first channel among the engine's
  device inputs, or why it didn't open. An input that doesn't open has no
  channels, and doesn't stop the stream.
- `Stream::input_buffers` hands out each input's `InputBuffer`, which any
  thread can read without locks: the last 8 seconds at least, from a fraction
  of a second after the stream starts, when the input has lined up, or a few
  seconds for an output whose clock moves in steps (below). Frames it
  hasn't captured yet read as `ReadError::NotYet`, and frames it no longer
  holds as `ReadError::Gone`, so a reader that falls behind finds out.
- `Stream::input_stats` has each input's counters: frames captured and lost,
  slips, discontinuities the device flagged, whether it has lined up, its
  clock and drift, and why it stopped if it did. A stopped input's channels
  are silent.
- The engine gets every input's channels, one input after another, in
  `ProcessContext::input` ([engine.md](engine.md)), `StreamStats::input_delay`
  frames behind the output: the monitoring latency, from a sound at the input
  to hearing it, leaving out converter delays.

**Lining up, on WASAPI.** Each input captures on a thread of its own, in
shared mode. At each wake-up the output publishes which engine frame its
device clock says it was playing, and when, by the performance counter. Each
packet an input captures comes with the device's position for its first frame
and when that was captured, on the same counter. Together they give each
packet's offset: its position, less the engine frame heard as it was
captured (scaled by the two rates).

Some drivers move the position they report only in steps: the RME HDSPe's,
in shared mode, 256 frames at a time, so a reading lags the frame being
heard by anything up to 256 frames, and offsets wander by tens. So the output
smooths its readings: less the steady advance at its rate, the highest of the
last 256 lags least, and that's the one it publishes. A reading more than 64
frames above them all, or below them all, is held back until three in a row
agree, as when the device's clock jumps. The output publishes nothing until
the highest is likely within a frame of the truth: after the first 32
readings for a clock that keeps time, about a third of a second, and after
the first 256, a few seconds, for one that moves in steps. On a device clock
slow against the performance counter, the highest is the oldest, which puts
the frame published ahead by the drift over the readings kept: a frame for
10 ppm over 2.56 s at 44.1 kHz, the same throughout. All this relies on the
readings falling on every part of a step. At 44.1 kHz they do, as the
shared-mode period of 441 frames shares no factor with 256. At 48 kHz, 480
frames shares a factor of 32 with it, so the readings fall on only 8 points
of a step, and in simulations the frame published lags by up to 31 frames,
less the more wake-ups jitter. It stays the same while the device plays, so
inputs don't slip, but it changes each time the device opens, and with it
where recordings land.

- An input on the output's clock, at its rate, has an offset that can't
  drift: the median of the first 16 packets', then of the first full window
  of 64, to the nearest frame. Its frames are copied as they are. That's the
  output device's own loopback, and an input in the same physical device as
  the output: the same container id, which Windows gives every endpoint of a
  device, leaving out the one all built-in endpoints share. For those, such
  as a sound card's, it's the same adapter: the device their topology
  connects to, whose input and output filters share its clock. Every 16
  packets the median is checked again. Moved by more than 8 frames twice in a
  row, the input slips to it, which `slips` counts: real devices' timings,
  each up to 8 frames off, move the median by up to 5. A second slip the same
  way means the clocks drift apart after all, and the input goes over to
  being resampled, without a gap.
- Any other input is resampled with the engine's resampler, following the
  offsets with a phase-locked loop: wide at first to line up within a second,
  then narrower, so jitter in the timings doesn't come through as wow. Its
  corrections glide in over the next 10 ms rather than jump, and
  `drift_ppm` says how much faster the input's clock runs. If the loop ends up
  more than 5 ms from the median offset twice in a row, as when the device's
  timing jumps, or 2,048 frames off at once, it starts again from there, which
  counts as a slip.

Frames the device skips are silence in the buffer and count as lost. Packets
whose time the device can't vouch for are recorded, but don't move the offset.

**Loopback** records what an output device plays: every app's audio as the
system mixes it, Musicable's own included when it plays there. Windows sends
a loopback nothing while nothing plays, so for a device other than the
stream's output the input plays silence to that device itself, to keep it
running. A loopback of the stream's own output in exclusive mode can't open,
since that bypasses the mix. Protected audio may record as silence.

**Monitoring.** The engine reads every input the same delay behind the
output: as far behind as the furthest input was at its first packet, plus a
packet's worth of room for inputs on threads of their own, since their
packets come in steps. If an input falls further behind, the delay grows to
match and never shrinks back, so monitoring never jumps back in time:
`input_late` counts the frames that played silent meanwhile. Recordings
don't lose them. An input more than a quarter of a second behind has
stalled, and plays silent rather than holding back the rest, as does one
that stopped.

**When Windows won't let Musicable record.** An input the microphone privacy
setting blocks fails with `Error::AccessDenied`'s message, which says where
to allow it.

**Elsewhere.** On macOS and Linux, inputs don't open yet: each says so in
`StreamInfo::inputs`.

## MIDI inputs

A stream plays and records from the MIDI inputs `StreamRequest::midi_inputs`
lists, lined up with the output like its audio inputs: each message comes
with the engine frame heard as it came in, the frame a player heard as they
played it, which is where a recording of it belongs. The engine plays it a
fixed delay later, to monitor it.

```rust
let request = StreamRequest {
    midi_inputs: musicable_audio::midi_input_devices()?
        .into_iter()
        .map(|device| device.id)
        .collect(),
    ..StreamRequest::default()
};
let open = musicable_audio::open(&request)?;
let buffers = open.midi_buffers(); // Some(MidiBuffer) for each MIDI input that opened
let stream = open.start(engine)?;
// On any thread, each message with the engine frame heard as it came in.
let mut reader = buffers[0].as_ref().unwrap().reader();
while let Some(MidiEvent { frame, bytes }) = reader.next_before(u64::MAX) {}
```

- `midi_input_devices` lists the MIDI inputs that are plugged in. An input's
  id is its name, with ` #2`, ` #3` and so on for the second and later with
  the same name, so it stays the same across runs while the same devices are
  plugged in.
- `StreamInfo::midi_inputs` says whether each requested input opened, or why
  not: for one, when another app has it and its driver serves one app at a
  time. One that doesn't open doesn't stop the stream.
- `Stream::midi_buffers` hands out each input's `MidiBuffer`, which any
  thread reads without locks, each with a `MidiReader` of its own. It holds
  the newest 16,384 messages; a reader that falls further behind loses the
  oldest, which `MidiReader::lost` counts. `MidiBuffer::settled` says the
  frame below which every message is in, so a reader that has read up to it
  has them all: the stream settles each input 5 ms behind the frame heard at
  each callback, since an input stamps a message as it comes in and writes
  it a moment later. Once the stream stops, the buffers end and every
  message is in.
- Only channel messages come through: notes, aftertouch, controllers,
  program changes, channel pressure and pitch bend, three bytes each, as
  `MidiEvent::bytes`. MIDI clock, active sensing and SysEx don't.

**Timing.** On Windows, the inputs come through WinMM, which every MIDI
driver serves, including Windows MIDI Services'. WinMM calls back with each
message as it comes in, on a thread of its own. The callback reads the
performance counter and takes the engine frame heard then from the output's
latest clock reading, the one audio inputs line up by
([Inputs](#inputs)), extended at the output's rate. WinMM's own time stamps
count whole milliseconds from when the input started, so they go unused. A
message that comes before the output publishes its first reading is
dropped: there's no frame to place it on yet.

**Monitoring.** The engine plays each message its frame plus a fixed delay
later: how far ahead of what's heard a callback renders, at most, which is
the output latency and a period. Messages heard during one period play
spread over the next, the time between them as it was played, wherever the
wake-ups fall. The delay is set at the first callback once the output
plays, grows if a callback ever renders further ahead, as after a late
wake-up, and never shrinks, so monitoring never goes back in time. A message
that comes too late for its frame plays at the start of the callback, and
`StreamStats::midi_late` counts it; `midi_delay` has the delay. A callback
takes 1,024 messages at most and leaves the rest for the next. The engine
gets them in `ProcessContext::midi` ([engine.md](engine.md#time-and-events)),
with each one's input as its place among the stream's MIDI inputs.

ASIO streams take MIDI inputs the same way: the driver's callback publishes
the output's clock reading, and the messages play the driver's output
latency and a period after they were heard.

**Elsewhere.** On macOS and Linux, MIDI inputs don't open yet: each says so
in `StreamInfo::midi_inputs`.

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

Each WASAPI input captures on a thread of its own, `musicable-input`, the same
way: in the multithreaded apartment and the "Pro Audio" task, with its WASAPI
calls on it and its work on each packet inside `rt::scope`.

MIDI inputs come in on WinMM's own thread, whose callback only stamps each
message and writes it to the input's buffer, without allocating or waiting.

ASIO streams render on the driver's own thread instead ([ASIO](#asio)).

## ASIO

Every Windows build plays through ASIO drivers too, with a host of our own
in `src/asio/`. Nothing else needs installing: no SDK, and no LLVM for
bindings. The driver interface is written from Steinberg's ASIO 2.3
specification, in our own words, and the drivers come with the hardware.
Builds are 64-bit only. Elsewhere than on Windows, the module builds only for
its tests.

Like the rest of Musicable, the host is licensed under the GNU GPL, version 3
or later. ASIO is a trademark of Steinberg Media Technologies GmbH.

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

**Inputs.** A stream on a driver records from the driver's own inputs when
`StreamRequest::inputs` has the driver's id: all its input channels, up to 64
(`AsioCapabilities::inputs`). Their buffers are created with the outputs', and
each half they hand over is taken in the same callback, before the engine
renders, so they're on the output's clock and copied as they are. The input
latency the driver reports is how old a half's first frame is at its buffer
switch, a buffer at the least, and the half rendered at that switch plays the
output latency after it, so the half goes in at the engine frame rendered then
less the two latencies. That's also the monitoring delay. Inputs through the
system can open alongside, lined up by when the driver says each half plays,
and always resampled, since ASIO doesn't say which clock the driver is on.
Another driver's inputs can't: ASIO runs one driver at a time. A driver that
won't create its input buffers plays without them.

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
- **The callback came after its half started playing**: more than a buffer
  and the output latency the driver reports after the last one. Buffer
  switches come a buffer apart, and the half asked for at one plays the
  latency after it. The specification asks drivers to call back for every
  half, late or not, and then the position and the halves keep step: only
  the time shows the dropout.

A driver that says it reports every dropout itself is taken at its word, as
the specification asks, and only its reports count: `reports_dropouts` in
`AsioCapabilities` says which drivers do. The first 8 callbacks fill the
device's buffers before it starts, so their timing means nothing yet and the
checks begin after them. The next 64 show whether the driver calls back
evenly: one that calls back twice within a quarter of a buffer in them does
it in bursts, from buffers of its own, and its late callbacks don't count.
Signs in the 10 callbacks after a dropout count towards it, as on the other
backends. `lost_frames` adds up what the sample position skipped, or a buffer
per sign when nothing says more. `late_wakeups` counts callbacks more than one
and a half buffers apart, and `system_dropouts` stays at 0: there's no system
audio engine in between. `src/asio/detector.rs` has the details.

**The driver's settings, and resets.** `Stream::show_driver_settings` opens
the driver's own settings panel, from the stream's thread. When its settings
change, or its rate is changed from outside, the driver asks to be reset: the
stream ends with `Error::DriverReset`, and opening it again picks up the new
settings. The app does that at once, and says what the driver plays with now.
Some drivers' settings are a window of another app of theirs, so the stream
lets other apps bring a window to the front while it opens them.

**On the RME HDSPe AIO.** Its ASIO driver, "ASIO Hammerfall DSP" (version
4.6.2.0), only takes the buffer size set in its settings, and reports that
size and 65 frames more as its latency: 321 frames at 256, 7.28 ms at
44.1 kHz. Its settings are a window of RME's settings app, which opened
behind the others when the tone example asked for it from the background.
Changing the size there from 256 to 512 and back reset the stream each time,
and it played again at the new size within a tenth of a second, with no
dropouts. Holding up the audio thread for five buffers every 10 seconds
counted one dropout each time. An hour at 256 frames counted none in 620,162
callbacks, the longest of which took 0.23 ms. Two callbacks came late, in the
minute Windows made a shadow copy of the disk, one of them 18.26 ms after the
one before: more than a buffer and the latency, 13.08 ms, so its half had
started playing. The position and the halves kept step, and nothing counted
it: the last of the signs above comes from that hour. Whether the RME reports
its own dropouts, which would leave that sign out for it, wasn't recorded;
`--list` shows it now.

**Tests.** `src/asio/fake.rs` is a fake driver built in Rust, set up to behave
as drivers do, that records what the stream asks of it, and the tests in
`src/asio/tests.rs` play the driver's thread. They run on every platform with
`cargo test -p musicable-audio`: elsewhere than on Windows the module builds
only for them, with stand-ins for COM and for the event the stream's thread
waits on.

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

On Windows, `--list` shows the ASIO drivers as well, and whether each
reports its own dropouts. `--asio` plays through one: the one
`--device` names, or the only one installed. `--driver-settings` opens the
driver's own settings once it plays, to see a change there reset the stream
and reopen it.

```sh
cargo run --release -p musicable-audio --example tone -- --asio --seconds 3600
```

The round trip example plays clicks and records them back, through a cable from
an output to an input or through a loopback, and says where each one landed,
to a fraction of a frame. It opens the stream again for each run, to see
whether the offset stays put, which is what calibrating it needs. With
`--soak`, it plays a tone instead, for as many minutes as asked, and checks
every frame it records for a glitch: a tone changes smoothly, so a lost,
repeated or misplaced frame shows.

```sh
cargo run --release -p musicable-audio --example round_trip -- --list
cargo run --release -p musicable-audio --example round_trip -- --loopback --runs 5
cargo run --release -p musicable-audio --example round_trip -- --output RME --input RME
cargo run --release -p musicable-audio --example round_trip -- --asio
cargo run --release -p musicable-audio --example round_trip -- --input RME --soak 60
```

Under Wine, its capture timings are a packet or two out, so a resampled input
wanders by a few milliseconds there. That says nothing of Windows.

The MIDI inputs example plays a click for each note played on every MIDI
input, or those `--input` names, so the monitoring delay can be heard, and
prints each message as it comes in, with the engine frame heard then and the
time since the one before. It ends with the MIDI delay and what came late.

```sh
cargo run --release -p musicable-audio --example midi_inputs -- --list
cargo run --release -p musicable-audio --example midi_inputs -- --seconds 60
```

The tests in `crates/audio/tests/device.rs` that play need an output device,
so they're ignored by default:

```sh
cargo test -p musicable-audio --test device -- --ignored --test-threads 1
```

On Linux, building needs the ALSA development files (`libasound2-dev` on
Debian and Ubuntu).

## Not done yet

- A late ASIO callback counted on real hardware, and whether the RME reports
  its own dropouts: its hour ran before the stream timed callbacks.
- ASIO's outputs past the first two.
- Inputs measured on real hardware: where a click lands through a cable and
  through a loopback, and whether that stays put from one stream to the next.
  What WASAPI's capture times mean for a loopback, mixed or played, isn't
  known yet.
- Calibrating inputs by what the round trip measures. Recording itself is
  the app's ([app.md](app.md#recording)).
- MIDI inputs measured on real hardware: how far WinMM's callback runs
  behind a key press, and how much that varies.
- MIDI inputs on macOS and Linux, Windows MIDI Services' own API (MIDI 2.0
  and its time stamps), SysEx, and MIDI outputs.
- Inputs on macOS and Linux.
- A resampled input follows its device's timings, so timings that jitter by a
  millisecond leave it wandering by a few frames. A loop that learned the
  jitter and narrowed to suit it would do better.
- Choosing an ASIO driver's clock source. The driver's own settings do it
  for now.
- Packaging builds for release, with the GPL's text and the offer of the
  source.
- Following the default output when it changes.
- Counting glitches downstream that don't hold up the audio engine, such as
  a late driver on a device whose clock drifts against it. Without a late
  wake-up they look just like the audio engine catching up, so they go
  uncounted.
- Tuning on macOS and Linux.
