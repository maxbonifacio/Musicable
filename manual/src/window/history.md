# The History panel

The History panel lists every step of the undo history by name, the oldest
at the top. Live has no such list. **View > History** (Ctrl+Alt+H) shows
it, in a tab beside the browser at first.

- **The top row** is the project before any of the steps, named after it,
  and a row follows for each step, up to the 1000 the history keeps. The row
  the project is at is highlighted, and the steps after it, which redo makes
  again, are greyed out under it.
- **Clicking a row** takes the project back or forward to it, undoing or
  redoing the steps in between. Clicking the top row undoes them all, and
  the project has no changes to save if it had none when it opened. The list
  scrolls to keep the row the project is at in sight.
- **A new change** after going back drops the greyed steps, as undo does.
- **Up and Down** step back and forward through the list while the panel has
  the focus, as Undo and Redo do. They're the panel's own keys, so the arrow
  keys still move the notes in the piano roll.

Undo (Ctrl+Z) and Redo (Ctrl+Y or Ctrl+Shift+Z) do the same a step at a
time, wherever the focus is. Undo and redo cover every change to the
project: the clips and their notes, the takes, the locators, the loop, the
tempo, the tracks, the devices, the automation and the mix.

The History panel docks, moves to a window of its own and goes in
workspaces as the other panels do ([The window](index.md#the-panels)).
