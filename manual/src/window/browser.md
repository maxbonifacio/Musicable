# The browser

The browser is the panel on the left, with three lists: Samples, Devices and
Presets. **Browser > Search in Browser** (Ctrl+F) shows it, with the keys in
the search field of the list showing, and Up and Down there move through
what it lists.

## Samples

### The library

The library is the folders of samples you add, with the + at the foot of the
list, or **Browser > Add Folder to Library...**. Musicable looks through
each folder and the folders in it for WAV, AIFF and FLAC files, and lists
them once it has found them all, which takes about two seconds for 350,000
files on an SSD. Then it reads each one's header in the background, for the
format, sample rate, channels and length that the tooltips and Find Similar
use, while the foot says how many are left ("Reading: 1,200 files left").

Musicable keeps the library in its config folder
([Settings](../customising/settings.md#the-config-folder)), so the next
start has it at once, and if Musicable closed before the headers were all
read, it reads the rest. Each start looks through the folders again in the
background, reading only the headers of files whose size or time of change
differ, and **Browser > Rescan Library** does it when asked. A tagged file
that moved within its folder keeps its tags. Tags given while a folder is
looked through stay.

A folder's right-click menu has **Rescan** and **Remove from Library**,
which leaves the files where they are. A folder can't be added twice, nor
one inside a folder of the library.

### Browsing and searching

With the search field empty, the list is the folders of the library, which
a click opens and closes: each folder's folders first, then its files, each
in order of name.

Typing in the field searches as you type: every word has to be in a file's
name, its folders or its tags, and the best matches come first, those whose
names have the words before those whose folders do, and a word that starts
a word of the name before one inside it, so "kick" finds "Kick_01" before
"Sidekick". The list shows the first thousand files found, with their
folders, and the foot says how many there are in all. Searches of a quarter
of a million files take a few milliseconds.

### A sample's menu

A sample's right-click menu has **Preview**, **Insert**, **Find Similar**,
and its tags.

- **Find Similar** lists the files most like it, "Like Kick 01" above them,
  until × goes back to the search: those whose names share the most words
  with its name, then those in the same folder, then those nearest its
  length. The names and lengths are all it goes on for now.
- **The tags** are yours: type one in New tag and press Enter to give it,
  and click one to take it away. Searches find them, and the library keeps
  them. They aren't in the project's undo history, as they aren't the
  project's.

### Previews

Clicking a sample plays it, beside the song, while **Auto**, at the foot, is
on. The strip there shows its waveform with where it's playing, which a
click plays from the start again; **Stop** stops it, and the slider sets how
loud previews play, from -60 to +6 dB. Previews play after the master, so
they're never in an export, and a file at another sample rate is resampled.
The browser remembers Auto and the volume.

### Onto tracks

Double-click a sample, or pick **Insert**, to put it on the selected audio
track at the start marker, or on a new audio track named after it, as an
imported file goes ([Projects and files](../using/projects.md#audio-files)).
Or drag it onto a track, where it starts at the grid line nearest where it's
dropped, or below the tracks for a new track there. Dragged onto a sampler
in the devices panel, it's the sampler's sample
([Sampler](../devices/sampler.md)).

While a sample, a device or a preset is dragged, the place it would go is
outlined, and "New track" shows below the tracks.

## Devices

The Devices list has Musicable's own devices under **Built-in**, then the
plugins found, by name, under **Plugins** ([Plugins](../using/plugins.md)).
**All**, **Instruments**, **MIDI Effects** and **Audio Effects** show all
of them, or only those of one kind: an instrument plays notes, a MIDI
effect, such as an arpeggiator, plays them on, changed, to the instrument
after it ([Plugins](../using/plugins.md#midi-effects)), and an audio effect
works on sound. Hovering a plugin shows its vendor, its kind and its
format. The search field finds devices by name, and plugins by vendor too,
the best matches first.

A plugin's right-click menu has its **Kind**, for a plugin Musicable takes
for the wrong one: pick another and the browser lists it in that kind's
folder from then on, and adds it as that kind
([Plugins](../using/plugins.md#finding-them)). It has its **Render Ahead**
too: **Never** has it render live wherever it is, for one that
misbehaves rendered ahead ([Plugins](../using/plugins.md#rendering-ahead)).

Double-click a device to put it at the end of the selected track's devices,
or the master's while the master track is selected. Or drag it: into the
devices panel, between the devices where a line shows it would go, onto a
track or its mixer strip for the end of its devices, onto the master track
for the master's, or below the tracks for a new track named after it, a
MIDI track for an instrument or a MIDI effect and an audio track for an
audio effect. A MIDI effect goes no later than just before the track's
instrument, wherever it's dropped, and only on a MIDI track. The track it
goes on is selected, so the devices panel shows it there. Devices dragged
by their title bars in the devices panel go the same places
([The devices panel](devices-panel.md#selecting-moving-and-copying)).

## Presets

Presets are a device's settings in a file, a plugin's state and all.

**Save as Preset...**, in a device's right-click menu in the devices panel,
asks for a name, and saves the device as a Musicable preset: a
`.musicable-preset` file in a folder named after the device in the Presets
folder, `Music/Musicable/Presets` in your home folder, or the one the
Settings window's Browser page picks. A name the device has a preset for
already is refused.

The Presets list also finds the VST3 presets (`.vstpreset`) in VST3's
preset folders:

| System | Folders |
|---|---|
| Windows | `Documents\VST3 Presets`, `%PROGRAMDATA%\VST3 Presets` |
| macOS | `~/Library/Audio/Presets`, `/Library/Audio/Presets`, `/Network/Library/Audio/Presets` |
| Linux | `~/.vst3/presets`, `/usr/share/vst3/presets`, `/usr/local/share/vst3/presets` |

The list groups presets by device, finds them by name and device as the
samples are found, and leaves out those for plugins that aren't installed,
which its foot counts.

Double-click a preset to add its device to the chain the devices panel
shows, set as it was saved, or drag it there, onto a track, or below the
tracks for a new track. Adding a preset is a step in the undo history named
after it. A sampler's preset names its sample in full, so it plays in any
project; added to a project whose folder has the sample, the sampler names
it from there, as clips name their files.
