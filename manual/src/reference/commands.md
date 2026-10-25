# Every command

Everything you do in Musicable runs a command: the menus, the keys, the
command palette and the mouse all run commands. This page lists every one,
with its default keys.

Keys that work in one panel only, such as the arrow keys in the piano roll,
say which panel. You can change any command's keys on the Settings window's
Keys page (**Options > Keyboard Shortcuts**, Ctrl+K). The command palette
(Ctrl+Shift+P) finds any of these commands by its name, or by its keys.

A command's id is its name in keymap files.

## The menus

{{#include ../generated/commands.md}}

## Commands the mouse runs

These commands need to be told what to work on, which the gestures and the
right-click menus of the arrangement, the piano roll and the other panels
tell them. The menus, the keys and the command palette can't, so they leave
these out. Scripts and agents will run them too.

{{#include ../generated/commands-with-arguments.md}}
