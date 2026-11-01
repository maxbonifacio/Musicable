# Plugins

Musicable plays CLAP and VST3 plugins, instruments, MIDI effects and
audio effects, beside its own [built-in devices](../devices/index.md).

## Finding them

Musicable finds plugins in each format's usual folders, and the folders in
them:

| System | CLAP | VST3 |
|---|---|---|
| Windows | `%COMMONPROGRAMFILES%\CLAP`, `%LOCALAPPDATA%\Programs\Common\CLAP` | `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3` |
| macOS | `/Library/Audio/Plug-Ins/CLAP`, `~/Library/Audio/Plug-Ins/CLAP` | `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3` |
| Linux | `~/.clap`, `/usr/lib/clap` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` |

`%COMMONPROGRAMFILES%` is usually `C:\Program Files\Common Files`. To look
in other folders too, list them in the `CLAP_PATH` and `VST3_PATH`
environment variables before starting Musicable.

Musicable scans in the background as it starts, with the plugins the last
scan found ready at once, and loads only the plugins that are new or
changed since. A plugin that crashes, hangs or fails as it loads is left
out, and the status bar says which; **Options > Rescan Plugins** tries
those again too.

The browser's Devices list has the plugins found under **Plugins**, after
Musicable's own devices. A plugin installed in both formats is listed in
each, with its format after its name: "Surge XT (CLAP)" and "Surge XT
(VST3)". The list's foot says how many plugins there are, and has
**Rescan**, or says "Scanning..." while a scan runs.

Each plugin is an instrument, a MIDI effect or an audio effect, as the scan
finds it. A CLAP plugin says which it is. A VST3 plugin says when it's an
instrument, but not when it's a MIDI effect, so Musicable takes one that
takes notes and plays notes, but no sound, for a MIDI effect, as an
arpeggiator does. Where that gets a plugin wrong, its right-click menu in
the browser marks it as another **Kind**
([The browser](../window/browser.md#devices)), which Musicable remembers,
and marking it as the kind the scan found takes the mark away. Devices
already on tracks keep the kind they were added as: delete one and add it
again for the new kind.

## Putting one on a track

Double-click a plugin in the browser to put it at the end of the selected
track's devices, or the master's while the master track is selected. Or drag
it into the devices panel, onto a track for the end of its devices, onto the
master track for the master's, or below the tracks for a new track named
after it: a MIDI track for an instrument or a MIDI effect and an audio
track for an audio effect ([The browser](../window/browser.md#devices)).

Drag a plugin device by its title bar in the devices panel to move it along
the chain or to another track, and Ctrl-drag it to copy it ([The devices
panel](../window/devices-panel.md#selecting-moving-and-copying)). A moved
plugin plays on, with its window open if it was. A copy loads the plugin
again, with the state the original has in its window.

## MIDI effects

A MIDI effect, such as an arpeggiator, a chord maker or a note repeater,
plays the notes that come into it on, changed, rather than sound. It goes
on a MIDI track, in front of the instrument, as in Live, and plays the
instrument the notes it makes, as the notes come: an arpeggiator plays a
synth the chord its track's clip holds, a note at a time.

- **Where it goes.** A MIDI effect added or copied to a track goes no
  later than just before its instrument, wherever it's dropped, and an
  instrument no earlier than just after the last MIDI effect. A MIDI effect
  can't go on an audio track, a group, a return track or the master, which
  have no notes, nor move past the instrument: the move is refused, and
  says why.
- **Several** play in turn, left to right, each the notes the last one
  played, and the last plays the instrument. The track's modulators, such
  as an Envelope that each note starts, get the notes the instrument does
  ([Modulation](modulation.md)).
- **Bypassed**, a MIDI effect passes the notes on as they come, and
  **deactivated**, it's out of their way. Either way the notes it was
  playing end, and a sustain pedal it held down comes up, so none hangs.
  Brought back, it starts afresh. A pedal the track's clips or input hold
  down holds on whichever way the notes go, and an effect brought back
  hears it's down.
- **Notes of its own.** What it plays are notes of its own, which the
  instrument tells apart from those played into it, each with the
  expression the plugin gives it, such as a pitch bend of its own. A
  plugin that names its notes, as CLAP and VST3 plugins can, can play
  several on one key at once, such as a note repeater's overlapping
  notes, and each sounds until the plugin ends it. Notes it plays as MIDI
  play as a keyboard's do: a key played again ends the note on it first.
- **On another track.** Another MIDI track can take the notes it plays,
  after it, or those that reach the instrument, at Post FX, to double them
  on an instrument of its own or record them as a clip ([Taking another
  track's notes](routing.md#taking-another-tracks-notes)).

## Notes a plugin plays out

Some instruments and effects play notes out as they play: a synth with an
arpeggiator or a sequencer of its own, or an effect that hears the notes
in what comes into it, such as a pitch tracker on a vocal or a drum
trigger. Musicable finds out as the plugin loads, from its note output,
and then the point after it carries those notes as well as its sound.

- **On another track.** A MIDI track can take them, after the plugin, to
  double them on an instrument of its own or record them as a clip
  ([Taking another track's notes](routing.md#taking-another-tracks-notes)),
  and the `wire.connect` command can wire them into an instrument
  elsewhere. They play there at the same moment the plugin plays them,
  never a block late.
- **Bypassed or deactivated**, the plugin plays none out, and the notes it
  was playing end as it stops, and a sustain pedal it held down comes up,
  so none hangs.
- **A plugin that changes.** The project keeps whether a plugin has a note
  output, so a track taking its notes keeps them while the plugin is
  missing. If a later version has none, the track still has it as its
  input, and gets notes again once the plugin plays them.

## Notes' timing and latency

A plugin with a latency, such as one that looks ahead or oversamples,
puts out its sound that long after what came into it, and Musicable has
the rest of the mix wait for it. The notes it plays can come one of two
ways, which neither CLAP nor VST3 says, so plugins differ:

- **With Its Sound**: as late as its sound. A MIDI effect that looks ahead,
  such as a humanizer that can move notes early, holds its notes back by
  its latency, and a pitch tracker or a drum trigger can only say a note
  has started once it's heard more of it. The rest of the mix waits for
  these notes as it does for the plugin's sound, so what plays them lines
  up with it.
- **As It Plays Them**: as early as what comes into it. A synth's
  arpeggiator plays a step as it fires, and its sound for the step comes
  its latency after.

It matters for a MIDI effect, and for a plugin that plays its notes out
([above](#notes-a-plugin-plays-out)), with a latency of more than about
3 ms. For one of those, its menu in the devices panel has **Notes'
Timing**, with the latency, and three choices:

- **Auto**, at first, says which the device has, and why. As soon as such
  a plugin loads, Musicable plays it a note, or a tone for an effect,
  away from the song, in a process of its own, and sees when its first
  note comes: its latency later, or at once. If that says nothing, as
  when it plays nothing for a held note, it's taken to be With Its Sound
  for an effect and As It Plays Them for an instrument.
- **With Its Sound** or **As It Plays Them** chooses for this device,
  whatever Musicable found out.

If what plays a plugin's notes, on another track, or after a MIDI effect,
is about the plugin's latency late or early, try the other. The project
keeps each device's, so it plays the same on another computer.

The plugin's menu in the browser has **Notes' Timing** too, which marks
the plugin, for every project: its devices on Auto follow the mark. Auto
there takes the mark away.

Bypassed, a MIDI effect whose notes come with its sound holds back the
notes it passes on by as long, as a bypassed effect's sound waits for its
latency, so nothing moves as it's bypassed. Notes that come to an
instrument both from such an effect and from somewhere without one, such as
two MIDI tracks' notes when only one has the effect, wait for each other,
so they all play as late as the effect's. A recording of the notes a
plugin plays puts them as much earlier as they're late.

## Multi-timbral instruments

A multi-timbral instrument, such as a sampler with a part on each MIDI
channel, plays another sound on each channel. Give each part a MIDI track
without an instrument, and set the track's **MIDI To** to the instrument,
on the part's channel ([Playing another track's
instrument](routing.md#playing-another-tracks-instrument)): four tracks on
channels 1 to 4 play four parts, each with its own clips, mute and solo.
The instrument's own track plays it as it would without them: on the first
channel, unless the plugin takes MPE, which spreads its notes over a
channel each.

## Plugin windows

**Plugin Window**, on the plugin's device in the devices panel, opens the
plugin's own window, or closes it, and double-clicking the device's name
opens it too, as in Live. On Windows, the plugin's window goes inside a
window of Musicable's that opens over the window the devices panel is in, on
its monitor, stays in front of it and minimizes with it. Elsewhere it's a
window of the plugin's own. VST3 plugins' windows open only on Windows so
far.

## Bypassing

A plugin device's Bypass button stops the plugin, as it does any device
([The devices
panel](../window/devices-panel.md#bypassing-and-deactivating)). A plugin
with a bypass switch of its own shares it with its device: bypassing the
device switches the plugin's own on, once the device has faded out, and
clicking the plugin's own in its window bypasses the device, as a step in
the undo history. So they never disagree, and Musicable still does the
bypassing, which keeps the plugin's latency and frees its CPU. When a
project opens, the device's Bypass wins over what the plugin's state says.
A bypassed plugin still takes the changes made to it, from its window,
automation or the project, so it comes back as you left it.

## Side inputs

Many plugins take a second signal, beside what comes into them: a
compressor's sidechain, a vocoder's carrier, a gate's key. Each such input
gets a row in the plugin's box in the devices panel, under the plugin's
name for it, and takes its signal from a point on any track, as the
Compressor's Sidechain does ([Sidechains](sidechains.md#plugins-side-inputs)).

## Rendering ahead

With [Render Ahead](../getting-started/audio.md#render-ahead) on, plugins
render ahead of the audio device with the rest of the project. A plugin
that says it must render live, as one that talks to hardware does,
renders live instead, as the device asks for each buffer, with what it
plays into, and its track's header has a LIVE mark that says so. A VST3
plugin can change its mind as it plays, and its track changes over with
it.

**Render Ahead**, in a plugin's right-click menu in the browser, or in its
device's in the devices panel, is **Auto** at first, which does as the
plugin says. **Never** renders it live wherever it is, for a plugin that
misbehaves rendered ahead, such as one that talks to its copies on other
tracks to compare them, which a lead ahead of some of them throws out. It
holds for every device of the plugin, in every project.

## Undo, saving and timing

Adding a plugin, moving it, copying it, deleting it and every change made
in its window are steps in the undo history, named after the plugin, such
as "Change Surge XT"; a whole drag of a knob is one. What a plugin changes
on its own, as it loads, or with its window closed, goes into the project
without a step, as do the side inputs it lists. Saving the project saves
each plugin's state as it is then.

Delay compensation lines up every track with the latest one, so a plugin
that looks ahead doesn't put its track out of time.

A device whose plugin isn't playing, because it isn't installed, didn't
load, or the scan hasn't found it yet, lets the signal through as it is,
and keeps its settings for when the plugin is there. The devices panel
says why ([The devices panel](../window/devices-panel.md#plugins)).
