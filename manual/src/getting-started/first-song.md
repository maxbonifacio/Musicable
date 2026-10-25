# A first song

This chapter makes a short song from start to finish with Musicable's own
devices: a part played on the Synth and a part recorded through a
microphone or an instrument, the best of each take comped, arranged, mixed
with an EQ, a compressor and a reverb, automated, and exported. Set up your
audio first ([Setting up audio](audio.md)). A MIDI keyboard helps, but you
can draw the notes instead.

## Start a project

1. **File > New Project** (Ctrl+N), then **File > Save Project As...**
   (Ctrl+Shift+S), in a folder of its own.
2. **File > Open Project...** (Ctrl+O) it again. Until then, a new project
   records into a folder for unsaved recordings, and this puts the
   recordings in the project's folder ([Recording and
   comping](../using/recording.md#the-files)).

## Add the tracks

3. **View > Browser** (Ctrl+Alt+B) shows the browser. Drag **Synth**, under
   **Built-in** in its **Devices** list, onto the arrangement: that makes a
   MIDI track with the Synth on it, and selects it, so the devices panel
   shows the Synth's controls ([Synth](../devices/synth.md)).
4. **Track > New Audio Track** (Ctrl+T). It records from the default input,
   and its strip's input menu, in the mixer, picks another
   ([Inputs](../using/recording.md#inputs)).

## Record

5. Drag across the strip along the top of the ruler, over the first bar or
   two: that's the loop, and looping is on. **O** turns the metronome on,
   and the tempo field in the transport bar sets the tempo.
6. Arm the MIDI track with the arm button in its header, and play a little:
   you hear the Synth. Press **Record** (F9), play the part two or three
   times round the loop, and press Space to stop. Each time round is a
   take, and the last one is on the track.

   Without a MIDI keyboard, or on macOS and Linux, where MIDI inputs don't
   open yet, drag across the MIDI track's lane over the loop, and **Clip >
   Insert MIDI Clip** (Ctrl+Shift+M) puts an empty clip there. Double-click
   it to open it in the piano roll, press **B** for Draw Mode, and click the
   notes in ([The piano roll](../window/piano-roll.md#the-mouse)).
7. Arm the audio track instead, and record the audio part the same way,
   singing or playing two or three times round the loop.

## Comp and arrange

8. Select a track, and **View > Show/Hide Take Lanes** (Ctrl+Alt+U) shows
   its takes under it. Drag across the best stretch of each take to copy it
   to the track: that's the comp ([Comping](../using/recording.md#comping)).
9. Arrange the clips: drag one by its title bar to move it, or Ctrl-drag it
   to copy it. Ctrl+D duplicates the selected clips, Ctrl+E splits them at
   the start marker, and Delete deletes them ([The
   arrangement](../window/arrangement.md#the-mouse)).

## Mix

10. Click the audio track's header to select it, double-click **EQ**, then
    **Compressor**, in the browser's Devices, and set them in the devices
    panel (Ctrl+Alt+L).
11. **Track > New Return Track** (Ctrl+Alt+T), select it, and give it the
    **Reverb** the same way, with its **Mix** at 100%, since the return
    carries only the reverb. Then, in the mixer (Ctrl+Alt+M), drag each
    track's send to the return up, for as much reverb as it wants ([The
    mixer](../window/mixer.md)).
12. Select a track and press **A** for its volume's automation lane. Drag
    across the lane to draw a dip, or a fade at the end
    ([Automation](../using/automation.md)).

## Undo, save and export

13. Undo (Ctrl+Z) as far back as you like, the recordings too, and redo
    (Ctrl+Y) all the way again: the song plays as it did. **View >
    History** (Ctrl+Alt+H) lists every step.
14. **File > Save Project** (Ctrl+S).
15. **File > Export Audio...** (Ctrl+Shift+R) writes the mix to a WAV
    file, faster than real time ([Exporting audio](../using/exporting.md)).
