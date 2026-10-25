# Profiles

A profile keeps your keys, your mouse map, the theme, your workspaces and
the Behaviour page's settings ([Settings](settings.md#behaviour)) together,
so switching to another, such as one with another DAW's habits, changes
them all at once, without starting Musicable again.

What belongs to the computer stays the same whichever profile is in use:
the audio and MIDI devices, the recording settings, the browser's library
and settings, the plugins, the export window's settings, and the theme
files in the `themes` folder, which a profile picks from.

## Switching profiles

- The Settings window's **Profiles** page (**Options > Profiles...**) lists
  them, and picking one switches to it.
- **Options > Switch to Next Profile** goes to the next, in the page's
  order, and from the last back to Default. The status bar says which.

Changes go to the profile in use, and Musicable opens with the profile it
closed with.

## The Profiles page

- **New...** makes a profile with Musicable's own keys, mouse, theme,
  layout and behaviour, and **Duplicate...** one with the profile in use's.
  Both switch to it.
- **Rename...** and **Delete...** rename or delete the profile in use.
  Deleting asks first, then switches to Default. Default keeps its name and
  can't be deleted.
- **Open Profiles Folder** shows their folders.

A profile's name can't be another's, in any letters, start with a dot, or
have characters a folder's name can't.

## Profile folders

Default is the profile Musicable starts with, and its files are the config
folder's own ([Settings](settings.md#the-config-folder)). Each of your
other profiles is a folder in the config folder's `profiles` folder, named
after it, such as `profiles\Bitwig habits\`, which holds the same files for
it:

- `keymap.toml`, its [keymap file](keys.md#your-keymap-file);
- `mouse.toml`, its [mouse map file](mouse.md#your-mouse-map-file);
- the `workspaces` folder, with its [workspace
  files](workspaces.md#workspace-files);
- a `settings.toml` with only `[behaviour]`, `[appearance]` and
  `[workspaces]` in it.

Each holds only what differs from Musicable's own, so a later version's new
defaults still reach you:

```toml
format = "musicable-settings"
version = 1

[behaviour]
play_from = "playhead"

[appearance]
theme = "light"
```

To take a profile to another computer, copy its folder to that computer's
`profiles` folder as it is: the Profiles page finds it the next time it
shows. A theme file of your own goes in that computer's `themes` folder
too. A file of a profile's that can't be used leaves what Musicable starts
with, and says why, as Default's do. Without a config folder, there's only
Default, and changes last until Musicable closes.
