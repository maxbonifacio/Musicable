<!-- Made from the code by crates/ui/src/manual.rs: don't edit it. `MUSICABLE_BLESS=1 cargo test -p musicable-ui --lib manual` makes it again. -->

### Automation

| Command | Id |
|---|---|
| Add Automation Point | `automation.add_point` |
| Clear Automation | `automation.clear` |
| Delete Automation Points | `automation.delete_points` |
| Hide Automation Lane | `automation.hide_lane` |
| Edit Automation | `automation.set_points` |
| Show Automation Lane | `automation.show_lane` |

### Browser

| Command | Id |
|---|---|
| Find Similar Samples | `browser.find_similar` |
| Preview Sample | `browser.preview` |
| Remove Folder from Library | `browser.remove_folder` |
| Search the Library | `browser.search` |
| Set Plugin Kind | `browser.set_plugin_kind` |
| Tag Sample | `browser.tag` |

### Clip

| Command | Id |
|---|---|
| Delete Clips | `clip.delete` |
| Insert Audio | `clip.insert_audio` |
| Move Clips | `clip.move` |
| Open in Sample Editor | `clip.open` |
| Rename Clip | `clip.rename` |
| Set Fades | `clip.set_fades` |
| Set Clip Gain | `clip.set_gain` |
| Show Clip Lane | `clip.show_lane` |
| Split Clips | `clip.split` |
| Trim Clip | `clip.trim` |

### Controllers

| Command | Id |
|---|---|
| Draw Controller | `controllers.draw` |
| Erase Controller | `controllers.erase` |
| Show Controller | `controllers.show` |

### Device

| Command | Id |
|---|---|
| Add Device | `device.add` |
| Add Plugin | `device.add_plugin` |
| Add Preset | `device.add_preset` |
| Copy Devices | `device.copy` |
| Delete Device | `device.delete` |
| Hide Plugin Window | `device.hide_window` |
| Load Sample... | `device.load_sample` |
| Move Devices | `device.move` |
| Save Device as Preset | `device.save_preset` |
| Select Devices | `device.select` |
| Set Device Active | `device.set_active` |
| Set Device Bypass | `device.set_bypass` |
| Set Device Parameter | `device.set_param` |
| Set Sample | `device.set_sample` |
| Show Plugin Window | `device.show_window` |

### Edit

| Command | Id |
|---|---|
| Select Clips | `edit.select_clips` |
| Select Time | `edit.select_time` |
| Select Tracks | `edit.select_tracks` |

### History

| Command | Id |
|---|---|
| Go to Step | `history.go_to` |

### Locator

| Command | Id |
|---|---|
| Delete Locator | `locator.delete` |
| Move Locator | `locator.move` |
| Rename Locator | `locator.rename` |

### Mixer

| Command | Id |
|---|---|
| Remove Send | `mixer.remove_send` |
| Set Arm | `mixer.set_arm` |
| Set Clip Type | `mixer.set_clip_type` |
| Set Input | `mixer.set_input` |
| Set Master Clip Type | `mixer.set_master_clip_type` |
| Set Master Pan | `mixer.set_master_pan` |
| Set Master Volume | `mixer.set_master_volume` |
| Set MIDI To | `mixer.set_midi_to` |
| Set Monitor | `mixer.set_monitor` |
| Set Mute | `mixer.set_mute` |
| Set Output | `mixer.set_output` |
| Set Pan | `mixer.set_pan` |
| Set Send Level | `mixer.set_send_level` |
| Set Send Pre-Fader | `mixer.set_send_pre_fader` |
| Set Solo | `mixer.set_solo` |
| Set Solo-Safe | `mixer.set_solo_safe` |
| Set Volume | `mixer.set_volume` |

### Modulation

| Command | Id |
|---|---|
| Add Modulator | `modulation.add` |
| Add Macro Control | `modulation.add_control` |
| Add Routing | `modulation.add_routing` |
| Delete Modulator | `modulation.delete` |
| Delete Macro Control | `modulation.delete_control` |
| Delete Routing | `modulation.delete_routing` |
| Move Modulator | `modulation.move` |
| Rename Modulator | `modulation.rename` |
| Rename Macro Control | `modulation.rename_control` |
| Set Routing Amount | `modulation.set_amount` |
| Set Routing Bypass | `modulation.set_bypass` |
| Set Routing Curve | `modulation.set_curve` |
| Set Routing Lag | `modulation.set_lag` |
| Set Modulator Parameter | `modulation.set_param` |
| Set Routing Scale | `modulation.set_scale` |

### Notes

| Command | Id |
|---|---|
| Add Note | `notes.add` |
| Delete Notes | `notes.delete` |
| Draw Notes | `notes.draw` |
| Move Notes | `notes.move` |
| Open in Piano Roll | `notes.open` |
| Select Notes | `notes.select` |
| Set Velocity | `notes.set_velocity` |
| Trim Notes | `notes.trim` |

### Take

| Command | Id |
|---|---|
| Comp | `take.comp` |
| Delete Take | `take.delete` |
| Rename Take | `take.rename` |

### Track

| Command | Id |
|---|---|
| Rename Track | `track.rename` |

### Transient

| Command | Id |
|---|---|
| Move Transient | `transient.move` |

### Transport

| Command | Id |
|---|---|
| Locate | `transport.locate` |
| Set Loop | `transport.set_loop` |
| Set Start Marker | `transport.set_start_marker` |
| Set Tempo | `transport.set_tempo` |

### View

| Command | Id |
|---|---|
| Set Focus | `view.set_focus` |
| Set Piano Roll Viewport | `view.set_piano_roll_viewport` |
| Set Sample Editor Viewport | `view.set_sample_editor_viewport` |
| Set Track Height | `view.set_track_height` |
| Set Viewport | `view.set_viewport` |

### Warp

| Command | Id |
|---|---|
| Warp From Here | `warp.from_here` |
| Warp From Here (Start at Song Tempo) | `warp.from_here_at_song_tempo` |
| Warp From Here (Straight) | `warp.from_here_straight` |
| Move Audio Under Warp Marker | `warp.move_audio` |
| Move Warp Markers | `warp.move_markers` |
| Select Warp Markers | `warp.select_markers` |
| Select Time in Sample Editor | `warp.select_time` |
| Set Clip Detune | `warp.set_detune` |
| Set 1.1.1 Here | `warp.set_downbeat` |
| Set Spectral Envelope | `warp.set_envelope` |
| Set Fluctuation | `warp.set_fluctuation` |
| Set Follow Pitch | `warp.set_follow_pitch` |
| Set Formants | `warp.set_formants` |
| Set Grain Size | `warp.set_grain_size` |
| Set High Quality | `warp.set_high_quality` |
| Set Warp Mode | `warp.set_mode` |
| Set Slice Preserve | `warp.set_preserve` |
| Set Clip Tempo | `warp.set_tempo` |
| Set Transient Envelope | `warp.set_transient_envelope` |
| Set Transient Loop Mode | `warp.set_transient_loop` |
| Set Clip Transpose | `warp.set_transpose` |
| Warp at Song Tempo From Here | `warp.song_tempo_from_here` |

### Wire

| Command | Id |
|---|---|
| Connect Wire | `wire.connect` |
| Disconnect Wire | `wire.disconnect` |
| Set Wire Channel | `wire.set_channel` |
| Set Wire Gain | `wire.set_gain` |
| Set Wire Source | `wire.set_source` |
