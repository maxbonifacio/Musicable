# Themes

Musicable draws with a theme: the dark one, a light one, or a theme file of
your own. The Settings window's **Appearance** page (**Options >
Appearance**) picks one, and the window changes at once. A theme colours
Musicable's views and the menus, buttons, fields and windows too, and the
window's title bar is dark or light as the theme is, whichever the system
is in. Plugins' own windows keep their own look.

## Theme files

Theme files live in the `themes` folder in Musicable's config folder
([Settings](settings.md#the-config-folder)), and hold only what they change
in a built-in theme, the one their `base` names:

```toml
base = "light"
playhead = "#ff8800"

[mixer]
strip = 96

[widgets.resting]
background = "#d0d4e0"

[fonts]
text = "Inter.ttf"
```

Each entry has the name of one of the theme's colours or sizes, in the
section that holds it. Colours are `"#rrggbb"`, or `"#rrggbbaa"` with an
opacity, sizes are in points, and pairs of sizes, across and down, are
`[8, 3]`. A file without a `base` changes the dark theme.

**Export Theme...** on the Appearance page writes the theme in use as a file
with every entry in it, with those that keep the base's value commented out
and each group explained, to start a theme of your own from. Written into
the themes folder, it becomes the theme in use. Musicable looks at the theme
file in use twice a second, and draws with what you change in it as soon as
you save it. **Reload** reads it again, and its fonts.

## What the entries change

- Most entries colour or size Musicable's views: the arrangement's, the
  transport bar's, and those under `[mixer]`, `[devices]`, `[piano_roll]`,
  `[sample_editor]` and `[browser]`.
- `[dock]` sizes the panels' tab bars (`tab_bar`), the gaps between panels
  (`gap`), the outline around the panel with the focus (`outline`, in the
  `focus` colour) and the least a panel's edge can be dragged to
  (`min_panel`).
- `[widgets]` is the fill of panels and of windows and menus, their outline
  and shadow, text fields (`field`), the selection, links, warnings, corners
  and spacing. Under it, `[widgets.passive]` is what can't be clicked, such
  as labels and separators, and `resting`, `hovered`, `pressed` and `open`
  are buttons, checkboxes and sliders at rest, with the mouse over them,
  while they're pressed, and with their menu open, each with a
  `background`, a `fill` for checkboxes and the like, an `outline`, `text`
  and a `corner`.
- `[fonts]` names the font of text and of monospaced text: a font file in
  the themes folder, or one of those built in, `Ubuntu-Light` for text and
  `Hack` for monospace. Text a font can't write falls back to the built-in
  fonts. `[fonts.sizes]` holds the text sizes of menus, buttons and fields;
  Musicable's views have sizes of their own, such as `text`.

## Mistakes

A mistake doesn't stop the rest of the file from applying: an entry no theme
has, or one whose value isn't what it should be, keeps the base's value, and
a font file that can't be read keeps the built-in font. A file that can't be
used at all, such as one that isn't valid TOML, leaves its base in its
place, or the dark theme when even that can't be told, and so does a file
that's gone. The status bar says so until the file is mended, and the
Appearance page says what was wrong.

Themes can't be edited on the Appearance page yet: edit theme files in a
text editor.
