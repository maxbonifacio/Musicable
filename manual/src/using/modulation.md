# Modulation

Modulators move other controls as the song plays: an LFO sweeping a
filter's cutoff, an envelope shaping each note, or one knob turning
several controls at once. They never change the controls' own values: a
control comes back to where it was once nothing modulates it, and nothing
a modulator does goes into the project or the undo history.

Each track has modulators of its own, and so has the master. There are
three kinds:

- A **Macro** has knobs and buttons, which you play by hand, or automate. A
  knob turns from 0 to 1. A **toggle** button turns on or off at each press,
  and a **momentary** one is on while it's held. A new Macro has one knob.
- An **LFO** plays a shape that repeats.
- An **Envelope** plays a shape once each time it's started, by a note, say.

The modulation commands add modulators and routings and change them, each
change a step you can undo ([Commands the mouse
runs](../reference/commands.md#commands-the-mouse-runs)). Agents run them
([Agents](../reference/command-line.md#agents)), and the project file keeps
what they make.

## Routings

A modulator moves a control through a **routing**, from one of its
outputs. A Macro has an output for each knob and button, and an LFO and an
Envelope have one each. An output goes from 0 to 1, or from -1 to 1 for an
LFO that isn't unipolar. Several routings can move the same control, from
one modulator or from several, and they add up.

Each routing has its own settings:

- **Amount**: how far it moves the control while its output is at 1, as a
  share of the control's range, from -100% to 100%, evenly along the
  control as you'd turn it. At 25%, it moves a frequency a quarter of its
  octaves. Below 0, it moves the control the other way. A plugin's
  parameter takes it in the plugin's own units instead.
- **Curve**: how it bends its output first. **Linear** is straight,
  **Exponential** moves slowly at first and then fast, **Logarithmic** fast
  at first and then slowly, and **S-Curve** slowly at both ends.
  **Stepped** moves in even steps, 4 of them unless the routing says
  otherwise, up to 128. **Absolute** takes the output's size without its
  sign, so an LFO moves the control one way only, and **Positive** and
  **Negative** take only the output above 0, or below it. A bipolar output
  bends evenly either side of 0.
- **Scale**: another output on the same channel that multiplies the
  amount, as velocity would.
- **Lag**: how long it takes to follow its output, from 0 ms, at once, to
  10 s. It gets about two thirds of the way in that time, as a gentle low
  pass would.
- **Bypass**: a bypassed routing moves nothing, and keeps its settings.

Modulation moves a control from its value, or from what its automation
gives it at the time, and keeps it within its range. Modulators move the
mixer's faders, pans and sends, the gains of
[sidechains](sidechains.md), the controls of built-in devices that turn
smoothly, a CLAP plugin's parameters, and other modulators' controls. They
don't move switches, choices, Bypass, clips' settings, or a VST3 plugin's
parameters.

A modulator's own controls can be automated, and moved by other
modulators, as a device's can: an LFO that speeds up over the chorus, or a
Macro's knob turning an envelope's decay.

## What a modulator reaches

A modulator moves the controls on its own channel: its devices, with the
gains of their sidechains, its mixer strip and its other modulators. A
group's modulators also reach the tracks that feed the group, and the
master's reach every track. A routing to a control that's out of reach,
after its track leaves the group, say, moves nothing until the track is
back.

## Notes and triggers

A MIDI track's modulators hear the notes it plays, from its clips and its
input. An LFO in the Note start mode and an Envelope start on them, as
their **Note Trigger** says:

- **Always**: every note starts it.
- **Legato**: the first of notes that overlap starts it, and the rest
  don't, until they've all ended.
- **Never**: no note starts it.
- **Auto**, the default: as Always, unless a routing or automation moves
  its Trigger, or an Envelope's Gate Level. Then as Never, so that routing
  has it to itself.

## The LFO

{{#include ../generated/modulators/lfo.md}}

An LFO plays one of four shapes: a sine, a triangle, a saw that rises, and a
square, each starting its cycle at its middle on the way up, as a sine
does. **Skew** bends each cycle so its first half takes that share of it: a
triangle that rises for longer than it falls, a saw that bends, and on the
square, its pulse width. At 50%, each shape is even. **Phase** moves where
in its cycle it starts, in degrees.

It runs at **Rate** cycles a second, or with **Sync** on, a cycle each
**Note** at the song's tempo: 1/4 lasts a beat, T is a triplet and D
dotted.

Its **Start Mode** says what moves it on:

- **Free** runs on its own, all the time, whether the song plays or not.
- **Song** locks it to the song position: its cycles count from the start
  of the song, so a synced LFO keeps time with the beats and bars wherever
  you start playing, and it holds still while the song stands still.
- **Note** starts it again on notes, as Note Trigger says, and each time
  its **Trigger** rises past half way. After each start it waits **Delay**,
  then fades in over **Fade In**.

**Unipolar** moves its output from 0 to 1, rather than from -1 to 1.

## The Envelope

{{#include ../generated/modulators/envelope.md}}

Each time its gate opens, an Envelope waits **Delay**, rises over
**Attack** to full, holds there for **Hold**, then falls over **Decay** to
**Sustain**, where it stays while the gate is open. When the gate closes,
it falls over **Release** to nothing, from wherever it has got to. A stage
of 0 ms is left out, so with no Delay and no Hold it's an ADSR. **One
Shot** leaves out the sustain: it falls over Decay to nothing whatever the
gate does, or over Release if Decay is 0 ms.

**Attack Curve**, **Decay Curve** and **Release Curve** bend their stages.
At 0% a stage moves in a straight line, above it fast at first and then
slowly, as an analog envelope does, and below it slowly at first.

Its gate is open while a note is held, as Note Trigger says, while its Gate
**Level** is above half way, which another modulator or automation can
move, and, with **Transport** on, while the song plays. Each time the gate
opens again, the envelope starts from where it is, so it never jumps. Its
output goes from 0 to 1.
