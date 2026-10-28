# Recording and comping

Arm a track with its arm button, in its header or its strip, choose its
input in the strip's input menu, and press **Record** (F9) in the transport
bar. Playing starts from the start marker, as Space does, and every armed
track records its input while the transport plays: an audio track what its
audio input captures, or another track's sound, a MIDI track the notes its
MIDI input plays. Record while playing turns recording on or off there.

Stop ends the recording, and each pass of the playhead over the song becomes
a take on its track: one take for a recording straight through, and one for
each time round the loop while looping, the last of them on the track. One
undo takes the whole recording back.

## Punching in and out

**Punch In** and **Punch Out**, beside Loop in the transport bar, record
only from the loop region's start on, and only up to its end. With both on
and looping on, each time round the loop records the region and nothing
else.

## Inputs

An audio track records from one of the inputs the audio settings' device or
another input device has, or on Windows from the loopback of an output
device: everything the system plays there, listed as "Loopback:" and the
output's name. A new audio track takes the default input in stereo,
"Default 1/2": the system's default recording device, or with an ASIO
driver, the driver's own first inputs.

The input menu offers each input's pairs of channels and each channel on its
own, and lists the inputs again when it opens, for a device plugged in
since. An input that isn't plugged in keeps its name, in red. Musicable
opens the inputs that armed tracks and tracks monitoring In take, and keeps
them open until the audio device changes. Opening one restarts the audio,
so it waits while recording. An input that doesn't open, such as one
another app has to itself, says so in the status bar, and a track taking it
records nothing.

After the inputs, the menu lists the other tracks, each opening to its
points, and **Resampling**, the master: an audio track can record another
track's sound, or the whole mix ([Routing](routing.md)).

### MIDI inputs

A MIDI track records from one MIDI input or all of them, and from one MIDI
channel or all sixteen. A new MIDI track takes every channel of every input,
"All Inputs", as an MPE controller needs: its channels read as MPE, and a
keyboard's single channel plays as it is. The input menu lists All Inputs
and then each MIDI input plugged in, each with **All** and its channels 1 to
16, and lists them again when it opens.

Musicable opens the MIDI inputs that armed MIDI tracks and MIDI tracks
monitoring In take, while they're plugged in. While tracks want any, it
looks at what's plugged in every two seconds, except while recording: it
opens an input plugged in since that a track takes, or one plugged in again
after it was pulled out. MIDI inputs open on Windows only so far.

## Monitoring

A track's strip sets how it monitors its input:

- **In** plays the track's input instead of its clips.
- **Auto**, a new track's, plays it while the track is armed and the
  transport is stopped or recording, so you hear yourself as you record and
  the takes as you play them back. A MIDI track plays its input's notes on
  its instrument whenever it's armed, as in Live.
- **Off** never does, for an interface that monitors itself.

The input plays through the track's devices, as late as the audio device's
input and output latency makes it.

## Where it lands

A track records what its input captured as each moment of the song was
heard, so it lands where it was played, however late the plugins and the
audio device make what you hear. Latency the driver doesn't report, such as
an interface's converters', is made up for by the device's **recording
offset**: each audio take starts that many frames later in what its input
captured. The Settings window's **Recording** page sets it for the device
playing, in its mode and at its rate, in frames and milliseconds: drag it,
or type it and press Enter. Musicable keeps one for each device, mode and
sample rate, and the next recording uses it, as Capture does. A MIDI
message lands the same way as audio, without the offset, and so does what a
track takes from another track ([Printing a track](routing.md#printing-a-track)).

To measure the offset, loop an output into an input with a cable, and run
the record round trip example from Musicable's source folder, naming the
device:

```sh
cargo run --release -p musicable-app --example record_round_trip -- --output RME --input RME
```

It plays clicks out and records them back in, and says how late each one
landed, the same every time: that's the recording offset to set for the
device, in that mode and at that rate. With an ASIO driver, add `--asio`,
and it records from the driver's own inputs. `--help` lists the other
options, such as `--exclusive` and `--rate`, for measuring each mode and
rate. In Windows' shared mode, the offset can differ by a few frames each
time the device opens.

## What a MIDI track records

A MIDI track records the notes played, with their velocities, their release
velocities and their expression: pitch bend, pressure and slide, each
note's own with MPE. Each pass with notes is a take with a MIDI clip as long
as the pass; a pass without notes makes no take.

A note held as recording punches in isn't recorded, and one held as it
punches out or stops ends there. Round a loop, a note held over the loop's
end ends there and starts again at the loop's start, in the next pass.

It records how the controllers moved too, such as the sustain pedal and the
mod wheel, as the clip's [controller curves](../window/piano-roll.md), each
pass starting with them where they are: a pedal held down as a pass starts
holds its notes in the take as it did while recording.

## The files

Recordings are 32-bit float WAV files at the audio device's sample rate, in
a `Recordings` folder in the project's folder, named after the track and
numbered, as in `Lead 1.wav`. A project that has never been saved records
into `Music/Musicable/Unsaved Recordings` in your home folder, and its clips
name those files by their whole paths. After a new project's first save, it
records there until it's opened again, so save a new project and open it
again before recording into it.

A file's header says how long it is as of the last second, so a crash loses
a second at most. A loop recording is a file for each track, its passes one
after another, and a file that reaches the 4 GB a WAV file can hold goes on
in another, in the same take.

If Musicable falls more than 8 seconds behind in writing a recording, or an
input stops bringing audio for two seconds, the recording has silence
there, and the status bar says how much. A recording that finishes after
its project was closed doesn't go into the project open then, and the status
bar names its files.

## Capture

**Capture** (Ctrl+Shift+C), beside Record in the transport bar and in the
Transport menu, records what you just played without recording, as Live's
Capture MIDI does, for audio tracks as well as MIDI tracks. Musicable holds
the last few minutes of what the inputs of armed tracks, and of tracks
monitoring In, bring, and of what they take from other tracks. Capture makes what they played over the transport's
last run into takes, as if they had recorded it: the same files, passes and
takes, a pass for each time round the loop, in one step of the undo history,
named Capture.

The last run goes from where the transport last started playing to where it
stopped, or to now if it still plays. Stretches the transport recorded are
left out, since they're takes already, and so is what came in while it was
stopped, which has no place in the song. A run longer than the inputs are
held is captured from as far back as they go. With nothing to capture, the
status bar says why.

**How long.** The Settings window's Recording page sets how long each input
is held, 5 minutes at first; 0 turns Capture off. At 48 kHz, 5 minutes of a
stereo input take 128 MiB. A MIDI input holds 2,000 messages for each
second: one that sends more than that on average, such as an MPE controller
played throughout, holds less than the whole length. Tracks on the same
input channels, or taking the same point on another track, share what's
held. The inputs held take 2 GiB at most
together: an input that would go past that isn't held, and the status bar
names the tracks it's for.

What's held is dropped when another project opens, when how long changes,
and when new audio settings start. An input that no armed or monitoring-In
track takes any more stops being held. Holding starts again with what the
inputs bring next.

## Comping

Takes show in lanes under their track, each named in its header, after
**View > Show/Hide Take Lanes** (Ctrl+Alt+U) or the same in the track
header's menu. A take's clips show their waveforms or their notes, grey
where the track doesn't play them, and in the track's colour where it does:
where its own clips play the same audio or the same notes from the same
place, until one is moved or its notes change.

Dragging across a take copies that stretch of it to the track, replacing
what the track has there, and clicking one of its clips copies all of the
clip: that's comping. Alt-drag to copy a stretch off the grid. A take's
right-click menu has **Comp**, **Rename**, **Delete Take** and
**Show/Hide Take Lanes**, and double-clicking its name renames it.
