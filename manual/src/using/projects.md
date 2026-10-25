# Projects and files

A project is a `.musicable` file: the tracks, their clips, devices and
automation, the mix, the tempo and the locators. Audio clips play audio
files kept beside it.

## Opening and saving

**File > Open Project** (Ctrl+O), **Save Project** (Ctrl+S) and **Save
Project As** (Ctrl+Shift+S) use the system's file dialogs, starting in the
folder of the last project. Saving adds `.musicable` to a name without it,
and writes the new file before it replaces the old one, so a failed save
leaves the old file as it was. Errors show in the status bar.

**New Project** (Ctrl+N), **New Demo Project**, **Open Project** and
**Quit** (Ctrl+Q) would lose the project's unsaved changes, so they ask
first: **Save**, **Don't Save** or **Cancel**. So does closing the window.
Saving there can still be cancelled in the file dialog, which cancels the
rest.

A project remembers the workspace it was saved with, and opening it
switches there, if you have a workspace by that name
([Workspaces](../customising/workspaces.md)).

## Audio files

An audio clip plays a file by its path: relative to the folder the project
file is in, or the whole path for a file elsewhere. Musicable plays WAV,
AIFF and FLAC files, and resamples a file at another sample rate than the
audio device's.

**Adding files.** Drop audio files on the arrangement, or pick them with
**File > Import Audio Files...**, and they become clips, one after another.
A file in the project's folder, or a folder inside it, is named relative to
it, as `Samples/kick.wav`; any other is named by its whole path. Musicable
reads a file's header before it adds the clips, so they're as long as the
file, and a file it can't read is left out, with the reason in the status
bar. The browser puts samples on tracks too
([The browser](../window/browser.md#onto-tracks)).

**Long files**, over about 44 seconds at 48 kHz, stream from the disk as
they play, and shorter ones are read whole.

**Missing files.** A file that can't be read, because it's missing or isn't
one Musicable reads, leaves its clips silent, and the status bar says so,
once for each file until another project is opened. So does a file that
fails while it streams. A project that hasn't been saved has no folder, so
relative paths lead nowhere.

**Moving projects.** Save Project As doesn't copy the files or change the
clips' paths, so a project saved in another folder, away from its files,
plays its clips silent when it's opened again. To move a project, move its
folder with the files in it: the paths relative to the folder still lead to
them.

Recordings go in a `Recordings` folder in the project's folder
([Recording and comping](recording.md#the-files)).
