# The mouse

The mouse does what it does in Live at first. The Settings window's
**Mouse** page changes what each gesture does, from where it starts: in the
arrangement, the ruler and the loop, takes and automation, the piano roll,
the Sample Editor, the mixer and devices, and for the wheel.

## The Mouse page

Each gesture has a menu of what it can do there. Without modifiers, it can
do nothing; with them, it can do the same as without them, which is what
modifiers that have no action of their own there do.

- **Add Modifiers...** gives a gesture an action with other modifiers held,
  starting with what it does now.
- **Reset** puts one back as it was at first, and **Reset All** puts them
  all back.
- Gestures with Cmd show on a Mac only.

The changes go to your mouse map file at once. [The
arrangement](../window/arrangement.md#the-mouse), [the piano
roll](../window/piano-roll.md#the-mouse), [the Sample
Editor](../window/sample-editor.md#the-mouse), [the
mixer](../window/mixer.md#moving-the-controls) and the devices panel, for
[a device's title bar](../window/devices-panel.md#selecting-moving-and-copying)
and [its Bypass button](../window/devices-panel.md#bypassing-and-deactivating),
say what the gestures do at first.

## Modifiers part of the way through a drag

A drag starts as its gesture does with the modifiers held then. Pressing
or letting go of modifiers part of the way through changes it from there
on, when what they're bound to is the same drag, on the grid or off it, or
copying rather than moving: so at first, Alt keeps a drag off the grid
and Ctrl copies clips and notes, while they're held. Modifiers held
together that have no binding together there count one by one, so Ctrl
and Alt copy off the grid. Those bound to another drag, such as Shift on
a clip's edge, which stretches it rather than trimming it, change nothing
until the next drag.

A drag that snaps to the grid with no version off it of its own there,
such as moving notes, whose Alt-drag changes their velocity, the loop, a
locator or a time selection, keeps off the grid while the modifiers that
move clips off the grid are held: Alt, at first.

## Your mouse map file

Your mouse map file is `mouse.toml` in Musicable's config folder
([Settings](settings.md#the-config-folder)): the changes made on the Mouse
page. Musicable reads it as it starts and on **Reload**, writes each change
to it at once, and keeps `mouse.backup.toml` before writing over what would
be lost, as it does for the [keymap file](keys.md#backups).

Each table is a place where gestures start, and each line under it gives a
gesture there, with the modifiers held, what it does in place of what it
did:

```toml
format = "musicable-mouse"
version = 1

[clip_body]
"Drag" = "move"
"Double-click" = "none"

[solo_button]
"Click" = "add_solo"
"Ctrl+Click" = "exclusive_solo"
```

The modifiers are written as in keys, Ctrl, Alt, Shift and Cmd, and the
gestures are `Click`, `Double-click`, `Drag` and `Wheel`. `"none"` takes a
gesture's action away, so it does what it does without the modifiers, or
nothing if none are held.

**Create It**, on the page, writes a file that lists every place, what its
gestures can do there, and what they do at first, commented out: take the
`#` off a line to change it. An action a gesture can't do there, a place or
a gesture this version doesn't have, and other mistakes are left out, and
the page lists them. A file that can't be read leaves the mouse as it is at
first, and says why.
