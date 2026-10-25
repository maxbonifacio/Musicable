<!-- Made from the code by crates/ui/src/manual.rs: don't edit it. `MUSICABLE_BLESS=1 cargo test -p musicable-ui --lib manual` makes it again. -->

### File

| Command | Keys | Id |
|---|---|---|
| New Project | Ctrl+N | `file.new` |
| New Demo Project |  | `file.new_demo` |
| Open Project... | Ctrl+O | `file.open` |
| Save Project | Ctrl+S | `file.save` |
| Save Project As... | Ctrl+Shift+S | `file.save_as` |
| Import Audio Files... |  | `file.import_audio` |
| Export Audio... | Ctrl+Shift+R | `file.export_audio` |
| Quit | Ctrl+Q | `file.quit` |

### Edit

| Command | Keys | Id |
|---|---|---|
| Undo | Ctrl+Z; Up in the History panel | `edit.undo` |
| Redo | Ctrl+Y, Ctrl+Shift+Z; Down in the History panel | `edit.redo` |
| Cut | Ctrl+X | `edit.cut` |
| Copy | Ctrl+C | `edit.copy` |
| Paste | Ctrl+V | `edit.paste` |
| Duplicate | Ctrl+D | `edit.duplicate` |
| Delete | Backspace, Delete | `edit.delete` |
| Split | Ctrl+E | `edit.split` |
| Rename | Ctrl+R | `edit.rename` |
| Quantize | Ctrl+U | `edit.quantize` |
| Select All | Ctrl+A | `edit.select_all` |
| Select Master Track |  | `edit.select_master` |

### Track

| Command | Keys | Id |
|---|---|---|
| New Audio Track | Ctrl+T | `track.new_audio_track` |
| New MIDI Track | Ctrl+Shift+T | `track.new_midi_track` |
| New Return Track | Ctrl+Alt+T | `track.new_return_track` |
| New Group Track |  | `track.new_group_track` |
| Delete Tracks |  | `track.delete` |

### Clip

| Command | Keys | Id |
|---|---|---|
| Insert MIDI Clip | Ctrl+Shift+M | `clip.insert_midi_clip` |
| Warp |  | `clip.set_warp` |
| Double Clip Tempo |  | `warp.double_tempo` |
| Halve Clip Tempo |  | `warp.halve_tempo` |
| Warp Selection as a Loop |  | `warp.selection_as_loop` |
| Insert Warp Marker | Ctrl+I in the Sample Editor panel | `warp.insert_markers` |
| Delete Warp Markers |  | `warp.delete_markers` |
| Nudge Warp Markers Left | Left in the Sample Editor panel | `warp.nudge_left` |
| Nudge Warp Markers Right | Right in the Sample Editor panel | `warp.nudge_right` |
| Insert Transient | Ctrl+Shift+I in the Sample Editor panel | `transient.insert` |
| Delete Transients | Ctrl+Shift+Backspace in the Sample Editor panel | `transient.delete` |
| Reset Transients |  | `transient.reset` |

### Notes

| Command | Keys | Id |
|---|---|---|
| Draw Mode | B | `notes.toggle_draw` |
| Transpose Up | Up in the Piano Roll panel | `notes.transpose_up` |
| Transpose Down | Down in the Piano Roll panel | `notes.transpose_down` |
| Octave Up | Shift+Up in the Piano Roll panel | `notes.octave_up` |
| Octave Down | Shift+Down in the Piano Roll panel | `notes.octave_down` |
| Nudge Notes Left | Left in the Piano Roll panel | `notes.nudge_left` |
| Nudge Notes Right | Right in the Piano Roll panel | `notes.nudge_right` |
| Lengthen Notes | Shift+Right in the Piano Roll panel | `notes.lengthen` |
| Shorten Notes | Shift+Left in the Piano Roll panel | `notes.shorten` |
| Velocity Up | Ctrl+Up in the Piano Roll panel | `notes.velocity_up` |
| Velocity Down | Ctrl+Down in the Piano Roll panel | `notes.velocity_down` |
| Show Velocities |  | `controllers.show_velocities` |
| Clear Controller |  | `controllers.clear` |

### Automation

| Command | Keys | Id |
|---|---|---|
| Show/Hide Automation | A | `automation.show` |
| Re-Enable Automation |  | `automation.re_enable` |

### Locator

| Command | Keys | Id |
|---|---|---|
| Add Locator |  | `locator.add` |
| Previous Locator |  | `locator.previous` |
| Next Locator |  | `locator.next` |

### Transport

| Command | Keys | Id |
|---|---|---|
| Play/Stop | Space | `transport.play_stop` |
| Play | Shift+Space | `transport.play` |
| Stop | Ctrl+Period | `transport.stop` |
| Go to Start | Home | `transport.go_to_start` |
| Record | F9 | `transport.record` |
| Capture | Ctrl+Shift+C | `transport.capture` |
| Toggle Punch In |  | `transport.toggle_punch_in` |
| Toggle Punch Out |  | `transport.toggle_punch_out` |
| Toggle Loop | Ctrl+L | `transport.toggle_loop` |
| Toggle Metronome | O | `transport.toggle_metronome` |
| Tempo Up | Ctrl+Shift+Up | `transport.tempo_up` |
| Tempo Down | Ctrl+Shift+Down | `transport.tempo_down` |

### Browser

| Command | Keys | Id |
|---|---|---|
| Search in Browser | Ctrl+F | `browser.focus_search` |
| Add Folder to Library... |  | `browser.add_folder` |
| Rescan Library |  | `browser.rescan` |
| Stop Preview |  | `browser.stop_preview` |

### View

| Command | Keys | Id |
|---|---|---|
| Mixer | Ctrl+Alt+M | `view.toggle_mixer` |
| Devices | Ctrl+Alt+L | `view.toggle_devices` |
| Piano Roll |  | `view.toggle_piano_roll` |
| Sample Editor |  | `view.toggle_sample_editor` |
| Clip Editor | Ctrl+Alt+3 | `view.toggle_clip_editor` |
| Switch Devices and Clip Editor | F12 | `view.switch_devices_piano_roll` |
| Browser | Ctrl+Alt+B | `view.toggle_browser` |
| History | Ctrl+Alt+H | `view.toggle_history` |
| Move Panel to New Window |  | `view.move_to_new_window` |
| Move Panel to Main Window |  | `view.move_to_main_window` |
| Zoom In | Equals, NumpadAdd | `view.zoom_in` |
| Zoom Out | Minus, NumpadSubtract | `view.zoom_out` |
| Zoom to Selection | Z | `view.zoom_to_selection` |
| Follow | Ctrl+Shift+F | `view.toggle_follow` |
| Show/Hide Take Lanes | Ctrl+Alt+U | `view.toggle_take_lanes` |
| Narrow Grid | Ctrl+1 | `view.narrow_grid` |
| Widen Grid | Ctrl+2 | `view.widen_grid` |
| Triplet Grid | Ctrl+3 | `view.triplet_grid` |
| Fixed Grid | Ctrl+5 | `view.fixed_grid` |
| Snap to Grid | Ctrl+4 | `view.toggle_snap` |
| Command Palette... | Ctrl+Shift+P | `view.command_palette` |

### Workspaces

| Command | Keys | Id |
|---|---|---|
| Switch to Workspace 1 | Alt+1 | `workspace.switch_1` |
| Switch to Workspace 2 | Alt+2 | `workspace.switch_2` |
| Switch to Workspace 3 | Alt+3 | `workspace.switch_3` |
| Switch to Workspace 4 | Alt+4 | `workspace.switch_4` |
| Switch to Workspace 5 | Alt+5 | `workspace.switch_5` |
| Switch to Workspace 6 | Alt+6 | `workspace.switch_6` |
| Switch to Workspace 7 | Alt+7 | `workspace.switch_7` |
| Switch to Workspace 8 | Alt+8 | `workspace.switch_8` |
| Switch to Workspace 9 | Alt+9 | `workspace.switch_9` |
| Save Workspace As... |  | `workspace.save_as` |
| Rename Workspace... |  | `workspace.rename` |
| Delete Workspace... |  | `workspace.delete` |
| Reset Workspace |  | `workspace.reset` |

### Options

| Command | Keys | Id |
|---|---|---|
| Settings... | Ctrl+Comma | `options.settings` |
| Audio Settings... |  | `options.audio_settings` |
| Keyboard Shortcuts... | Ctrl+K | `options.keyboard_shortcuts` |
| Appearance... |  | `options.appearance` |
| Agents... |  | `options.agents` |
| Profiles... |  | `options.profiles` |
| Switch to Next Profile |  | `options.next_profile` |
| Rescan Plugins |  | `options.rescan_plugins` |

