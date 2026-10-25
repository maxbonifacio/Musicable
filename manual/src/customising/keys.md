# Keys

You can change the keys of every command in the menus, and give keys to the
ones that have none. Musicable's keys follow Live's where Live has one
([Default keys](../reference/keys.md)).

## The Keys page

The Settings window's **Keys** page (**Options > Keyboard Shortcuts**,
Ctrl+K) lists the commands by menu, each with its keys, and says where they
work: each command has a row for its keys that work everywhere, and one for
each panel it has keys of its own in, such as the piano roll's arrow keys. A
panel's keys win while the panel has the focus
([The window](../window/index.md#the-focus)). One that runs another command
everywhere else says so beside it, as "over Toggle Metronome". The search
field at the top finds commands the way the command palette does, by name or
by keys, wherever they work.

- **Click a key** to change it, or **+** to add one. The page waits for the
  keys, and any keys can be pressed, Escape, Enter and Tab included; they run
  nothing while it waits. **Where** picks the place the new keys work:
  everywhere, or one panel. It starts at the row's. Then it says what the
  keys do there now:
  - "isn't used yet": **Assign** (or Enter) gives them to the command. If a
    panel gives them to another command, it says so: the panel keeps them.
  - "runs *another command* now": **Reassign** (or Enter) takes them from
    that command there.
  - "runs *another command* everywhere": in a panel, **Assign** gives them
    to this command there, over the other.
  - "runs *this command* already": there's nothing to do.

  **Try Other Keys** waits for keys again. **Cancel**, Escape, a click
  outside the window or another page stops without changing anything.
- **×** next to a key removes it.
- **Reset** shows on a command whose keys aren't the default ones,
  everywhere or in a panel, and gives it its default keys back. If another
  command has one of them now, the tooltip says so, and Reset takes it.

Every change is saved to your keymap file at once
([below](#your-keymap-file)). Under the list:

- **Mistakes in your keymap file** ([below](#mistakes)). A key that two
  lines of one table claim runs none of their commands there, and gets a
  button for each to give it to. Keys for commands this version doesn't have
  can be removed.
- **Import...** uses the keys in another keymap file in place of yours. What
  it gets wrong is left out, and listed.
- **Export...** saves your keys to a keymap file, to share or keep.
- **Reset All** gives every command its default keys again.
- **Reload** reads your keymap file again after you edit it, and **Create
  It** creates it when there's none yet, to edit.
- Escape closes the window, once the page isn't waiting for keys.

## How keys reach commands

A key that runs a command runs it wherever you are, so Space plays even when
a button has the keyboard focus. The panel with the focus has its own keys
first, then the ones that work everywhere, so the transport's keys work in
every panel. A key that runs nothing, or whose command doesn't apply now,
does what it would do anyway: with no notes selected in the piano roll, the
arrow keys move the keyboard focus between controls.

- While a text field is being typed in, such as the tempo field, every key
  goes to it instead. Ctrl+C, Ctrl+X and Ctrl+V run their commands like
  other keys, and copy, cut and paste text only in text fields.
- Holding a key down runs its command once.
- While Musicable asks whether to save changes, keys go to that question.
- While the command palette is open, keys go to it, and run no commands.
- While the Keys page waits for keys, every key press goes to it, Escape and
  Enter too.

## Keyboard layouts

Keys are named after a US keyboard ([below](#key-names)). A key pressed on
another layout is named like this:

1. **The digit row by its place**: the key that types 1 on a US keyboard is
   `1`, even where the layout needs Shift for 1, as AZERTY does. So Ctrl+1 is
   the same key on every layout.
2. **Other keys by what they type**, when there's a name for it: Ctrl+Z is
   the key labelled Z, wherever the layout puts it (German, AZERTY), and a
   key that types `-` is `Minus`.
3. **Otherwise by its place on a US keyboard.** That covers letters on
   layouts without Latin letters (Ctrl+F on a Russian keyboard is the key
   where a US keyboard has F), characters a US keyboard has no key for (ö is
   `Semicolon`, ß is `Minus`), and keys pressed with Shift, since Shift
   changes what they type: Shift+1 is `Shift+1`, not `!`.

This is what most apps do with letters, and keeps shortcuts on the digit row
and punctuation where a keymap file puts them. The numeric keypad comes
through as the main keyboard's keys, so the keypad's 5 is `5`, except
NumpadAdd. The other keypad names in keymap files don't match any key press
yet. The Windows key doesn't reach Musicable, so Cmd keys only work on
macOS.

## Your keymap file

Your keymap file is `keymap.toml` in Musicable's config folder
([Settings](settings.md#the-config-folder)). Musicable reads it as it
starts and when you click **Reload** on the Keys page, which shows any
mistakes in it. A file that can't be used leaves the default keys, and says
why. The page saves every change of keys to the file at once.

The [default
keymap](https://github.com/maxbonifacio/Musicable/blob/main/crates/commands/keymaps/default.toml)
is built into Musicable. Your keymap file holds only your changes to it:

```toml
format = "musicable-keymap"
version = 1

[bindings]
"transport.toggle_metronome" = ["M", "Ctrl+M"]
"transport.toggle_loop" = []

[panels.piano_roll]
"notes.toggle_draw" = ["D"]
```

- Each line gives a command, by its id, the keys that run it, in place of
  its default ones. [Every command](../reference/commands.md) has every
  command's id. A single key can be written without the list:
  `"transport.stop" = "Escape"`.
- An empty list, `[]`, takes away the command's keys.
- Commands that aren't listed keep their default keys.
- A key runs one command. Giving a key to one command takes it from the
  command that had it.
- Bare dotted names work too: `transport.play = ["P"]` is the same as
  `"transport.play" = ["P"]`.
- A command whose id has changed can still be named by its old one, which
  Musicable writes as the new one: Quantize was `notes.quantize`, and is
  `edit.quantize`.
- `[bindings]` holds the keys that work everywhere. A panel's table,
  `[panels.piano_roll]` and the others (`arrangement`, `mixer`, `devices`,
  `browser`, `history`, `sample_editor`), holds the keys that work in that
  panel only, while
  it has the focus, where they win over the ones everywhere: in the piano
  roll, D runs Draw Mode above whatever it runs everywhere else. Its lines
  give the command its keys in that panel, in place of its default ones
  there. A key runs one command in each place. The default keymap's arrow
  keys for notes are the piano roll's, and Up and Down are Undo and Redo in
  the History panel.
- `format` and `version` are optional when you write the file by hand.
  Musicable writes both.

When Musicable saves your keymap, it writes only the commands whose keys
differ from the default, so new default keys in later versions still reach
you.

### Key names

A key is its modifiers and its key, joined by `+`: `Space`, `Ctrl+L`,
`Ctrl+Shift+Up`. Case doesn't matter and the modifiers can come in any
order. Musicable writes them as Ctrl, Alt, Shift, Cmd.

| Modifier | Also written | Notes |
|---|---|---|
| `Ctrl` | `Control` | |
| `Alt` | `Option` | |
| `Shift` | | |
| `Cmd` | `Command`, `Super`, `Win` | Command on macOS, the Windows key on Windows |

The keys:

- Letters `A` to `Z` and digits `0` to `9`.
- `F1` to `F24`.
- `Space`, `Enter`, `Tab`, `Backspace`, `Escape`, `Insert`, `Delete`,
  `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right`.
- Punctuation, named after the character the key types without Shift on a
  US layout: `Minus`, `Equals`, `OpenBracket`, `CloseBracket`, `Backslash`,
  `Semicolon`, `Quote`, `Backtick`, `Comma`, `Period`, `Slash`. The
  character itself works too: `Ctrl+.` is `Ctrl+Period`. The `+` key is
  `Equals` (or `NumpadAdd`), since `+` joins the parts of a key.
- The numeric keypad: `Numpad0` to `Numpad9`, `NumpadAdd`,
  `NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`,
  `NumpadEnter`.
- Other spellings: `Esc`, `Return`, `Del`, `PgUp`, `PgDn`, and `ArrowUp`,
  `ArrowDown`, `ArrowLeft`, `ArrowRight`.

The default keys use Ctrl for now, on macOS too; a macOS default with Cmd
comes with macOS support.

### Mistakes

A line with a mistake in it, such as a misspelt key, is skipped and the
command keeps its default keys. A key that two lines of one table claim runs
neither command there. A table for a panel Musicable doesn't have, or a
panel's entry that isn't a table, is skipped too. Musicable reports them and
loads the rest of the file. A line for a command that doesn't exist is kept,
in case a later version adds it, and reported too. The Keys page lists them
all, with a button to give a key two lines claim to one of them.

A file that isn't valid TOML, has an unknown field or comes from a newer
version is ignored as a whole: Musicable uses the default keys and shows the
error.

### Backups

Before writing over your keymap file, Musicable copies it to
`keymap.backup.toml` in the same folder when the write would lose
something:

- When the file has mistakes, or can't be read at all, the first change made
  on the page writes a new file, and the old one becomes the backup. The
  page says so.
- Reset All and Import replace all your changes at once, so the file with
  your keys from before becomes the backup. Importing the backup brings them
  back.

Single changes to a file without mistakes make no backup: there's nothing to
lose. If the backup or the file can't be written, the page says so, and the
keys you set still work until Musicable closes.

### Trying a keymap file

`musicable keymap FILE` prints every command with the keys a keymap file
gives it, along with any mistakes in the file, without opening the window
([The command line](../reference/command-line.md)). Leave out the file to
see the default keys. Keys that work in one panel only say which, as in `Up
in piano_roll`.
