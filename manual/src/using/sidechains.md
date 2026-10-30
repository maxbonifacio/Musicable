# Sidechains

A sidechain lets one track's sound set off a device on another: a kick
that ducks the bass through the bass's Compressor, say, or a snare that
opens a Gate on a room microphone. The [Compressor](../devices/compressor.md)
and the [Gate](../devices/gate.md) each have a side input, as do many
plugins, and a wire takes a signal to it from a point on any track. While
the device's Sidechain is on, its detector follows that signal, the key,
instead of what comes into the device, and turns down what comes into it as
before.

## The Sidechain column

The Compressor and the Gate have a **Sidechain** column after their other
controls:

- **The source**: where the key comes from, **No Input** until you pick
  one. Its menu lists the other tracks, each opening to its points:
  **Pre FX**, what comes into its devices from its clips or its input, then
  **after** each of its devices, **Post FX**, what comes out of the last of
  them, and **Post Fader**, after its fader and pan. Then **This track**
  opens to the points before the device on its own track, such as a vocal
  as it was before a saturator, to key a de-esser. **No Input** takes the
  source away. A MIDI track's Pre FX is notes, and so is the point after
  each MIDI effect in front of its instrument, which a key can't take.
- **Gain**, under each source: how loud its key is, from silence to
  +24 dB. Right-click it to type a value, as with any bar.
- **On**: the detector follows the key, not the device's own input. Picking
  a source turns it on. With no source, the device follows its own input
  whatever On says, so it never keys from silence.
- **Listen**: plays the key in place of the device's output, filtered, for
  setting the filters.
- **High-Pass** and **Low-Pass**: filter the key at 12 dB an octave, so a
  bass line doesn't pump a compressor, or a gate opens on a kick's thump
  and not on the hi-hats in its microphone. At their ends, 20 Hz and
  20 kHz, they cut nothing.

The filters and Listen work with On off too, on the device's own input, as
a compressor's sidechain filter usually does.

The **+** by the column's name adds another source under the first, such
as a snare beside a kick, both ducking a pad. The keys of all the sources
add up, each at its own gain.

The [Routing panel](../window/routing.md) lists every source of every side
input in the project under **Keys**, with its menu and its gain.

## Plugins' side inputs

A plugin with side inputs, such as a compressor's sidechain or a vocoder's
carrier and modulator, has a row for each in its box in the devices panel,
under the plugin's name for it. Each has the same source picker, a gain
under each source and the **+** by its name, as the Sidechain column has.

A plugin hears a side input only while it has a source. Picking the first
switches the input on, and taking away the last, with **No Input**,
switches it off. Plugins switch their inputs only while they're stopped,
so the plugin restarts each time, with a moment's gap in its sound, as
when its latency changes. With no source, the plugin hears nothing at
that input, so a plugin that keys from its side input whenever it's on
never keys from silence.

A plugin's box shows its side inputs once it has loaded, and keeps them
while the plugin is missing, so their sources stay. If an update of the
plugin drops an input that has a source, the row stays, after the others,
so the source does, though the plugin no longer hears it.

## What the key hears

A track's points come before its mute and solo, so a muted kick still
ducks the bass, as a ghost kick does. A point after a bypassed or
deactivated device has what goes on past it.

The key reaches the device in time with the beat it belongs to, to the
sample, whatever latency either track's devices have. When the key's path
is the later one, say through a linear-phase EQ, the keyed track waits for
it, and the whole mix lines up again at the master. An export sounds the
same as playback.

## Loops

Signal never goes round in a loop, so a source that would close one can't
be picked: its menu shows it greyed out. A device can't take its key from a
group its track feeds, or from the master, or from after itself, since
what it puts out reaches those. Two tracks can key each other from their
Pre FX, though, since that comes before either device.

A move, an output or a send that would close a loop through a wire is
refused, with a message naming the wire in the way, such as "That would
make a loop, through the wire from Kick Post FX to the Compressor on
Bass." A track's input from another track counts as well
([Loops](routing.md#loops)).

## Undoing, deleting and copying

Each change is a step in the undo history: **Connect Wire** for a new
source, **Wire Source** for another one picked in its place, **Disconnect
Wire** for No Input, and **Wire Gain**. Deleting a track or a device
deletes the wires to and from it in the same step, so undoing brings them
back too.

A copy of a device keeps its sources, so a copied compressor ducks the same
way, but the wires from a device's point don't come with its copy, so a
copy of the kick's gate doesn't key the bass twice.

A source's gain can be automated. Its lane is named after the source, such
as **Sidechain from Kick Post FX**, and the lane chooser has it in its
device's submenu ([Automation](automation.md#showing-the-lanes)). The gain's
right-click menu has **Show Automation** too. Modulators on the keyed
device's track can move it as well, as they move a send
([Modulation](modulation.md)).
