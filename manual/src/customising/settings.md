# Settings

**Options > Settings...** (Ctrl+Comma) opens the Settings window, with a
page for each area of the settings down its left side. It opens at the page
it showed last, and **Options > Audio Settings...**, **Keyboard
Shortcuts...** (Ctrl+K), **Appearance...**, **Agents...** and
**Profiles...** open it at theirs. Changes apply at once, and are kept
([below](#the-config-folder)). Escape closes the window, unless a menu or
the command palette is open over it, or the Keys page is waiting for keys:
then Escape closes that, or stops waiting, first. The Settings window isn't
a panel, so workspaces don't keep it.

- **Audio**: the device to play through, its mode, rate and buffer size,
  which **Apply** opens, and what's playing now ([Setting up
  audio](../getting-started/audio.md)).
- **Recording**: how long Capture holds the inputs, 0 for off
  ([Capture](../using/recording.md#capture)), and the recording offset of
  the device playing, in its mode and at its rate ([Where it
  lands](../using/recording.md#where-it-lands)).
- **Keys**: every command's keys ([Keys](keys.md)).
- **Mouse**: what each gesture does ([The mouse](mouse.md)).
- **Behaviour**: where Play/Stop plays from, what the piano roll calls
  middle C, which of auto-warp's rules are on, and Fast analysis
  ([below](#behaviour)).
- **Appearance**: the theme ([Themes](themes.md)).
- **Workspaces**: your workspaces, with the keys that switch to each, and
  picking one switches to it. **Save As...**, **Rename...**, **Delete...**
  and **Reset** do what the Workspaces menu's commands do, and **Open
  Workspaces Folder** shows their files ([Workspaces](workspaces.md)).
- **Browser**: whether picking a sample previews it, how loud previews play
  ([The browser](../window/browser.md#previews)), and the folder presets are
  saved in: **Choose Folder...** picks another, and **Use the Default** goes
  back to `Music/Musicable/Presets` in your home folder.
- **Files**: how much the render cache, which keeps the clips in Spectral
  as they were rendered, holds, and how much of its disk it always leaves
  free, with **Show Folder** ([Spectral](../using/warping.md#spectral)).
- **Profiles**: your profiles, and picking one switches to it
  ([Profiles](profiles.md)).
- **Agents**: **Let agents control Musicable**, off until you switch it on,
  how they're doing, and the command that adds Musicable to Claude Code,
  with a **Copy** button ([The command
  line](../reference/command-line.md#agents)).

## Behaviour

The Behaviour page has the settings where people's habits differ. They go
with the [profile](profiles.md) in use.

- **Play/Stop plays from** the start marker, as in Live, so playing again
  starts where the last play did, or where you put the marker; or from
  where the playhead stopped.
- **Middle C, MIDI note 60, is** C3, as in Live, Cubase, Logic and Bitwig;
  C4, as in Reaper and scientific pitch notation; or C5, as in FL Studio.
  The piano roll names its Cs and its notes by it.
- **Auto-warp** has a switch for each of its rules, all on until you switch
  them off ([Auto-warp](../using/warping.md#auto-warp)): **Recordings follow
  the tempo**, **Loops warp to their whole bars**, **Files at a steady tempo
  warp from their first downbeat**, **Files whose tempo drifts lock to the
  grid**, **Sounds without a pulse land on their hit**, and **The first file
  sets the song's tempo**. A file whose rule is off comes in as it is, and
  **Warp** warps it with one marker.
- **Fast analysis** finds beats with the classic beat tracker alone, without
  the Beat This! network, which is quicker on a slow computer but less
  accurate. It's the computer's setting, the same in every profile. Files
  analysed before it was switched on keep what was found, and once it's
  off, those it analysed are analysed again the next time they're opened.

## The config folder

Musicable keeps its settings, your keys, your mouse map, your themes, your
workspaces and your profiles in its config folder:

| System | Folder |
|---|---|
| Windows | `%APPDATA%\Musicable` |
| macOS | `~/Library/Application Support/Musicable` |
| Linux | `$XDG_CONFIG_HOME/musicable`, or `~/.config/musicable` |

The `MUSICABLE_CONFIG_DIR` environment variable picks another folder, such
as one for trying things out. Without a config folder, changes last until
Musicable closes.

| In the folder | What it is |
|---|---|
| `settings.toml` | The settings ([below](#the-settings-file)). |
| `keymap.toml` and `keymap.backup.toml` | Your keys ([Keys](keys.md#your-keymap-file)). |
| `mouse.toml` and `mouse.backup.toml` | Your mouse map ([The mouse](mouse.md#your-mouse-map-file)). |
| `workspaces` | A file for each workspace ([Workspaces](workspaces.md#workspace-files)). |
| `profiles` | A folder for each of your profiles but Default ([Profiles](profiles.md#profile-folders)). |
| `themes` | Your theme files, and the fonts they use ([Themes](themes.md#theme-files)). |
| `plugins.json` | What the plugin scans found, so the next start has the plugins at once and scans only what changed ([Plugins](../using/plugins.md#finding-them)). Deleting it makes the next scan load every plugin again. |
| `library.json` | The browser's library: the folders of samples you added, what's in them, and your tags ([The browser](../window/browser.md#the-library)). Deleting it empties the library, tags and all. |
| `musicable.log` and `musicable.old.log` | What went wrong while Musicable ran ([Troubleshooting](../reference/troubleshooting.md#the-log-file)). |
| `agents` | On macOS and Linux, the pipe agents connect through. |

The keymap, the mouse map, the workspaces and the settings that go with a
profile, in `settings.toml`, are the profile Default's.

## The cache folder

Musicable keeps what it can work out again in its cache folder: in
`analysis`, what it found in each audio file a project played, so it finds
a file's tempo and beats only the first time, even after the file moves,
and in `renders`, the render cache, the clips in Spectral as they were
rendered ([Spectral](../using/warping.md#spectral)).

| System | Folder |
|---|---|
| Windows | `%LOCALAPPDATA%\Musicable\Cache` |
| macOS | `~/Library/Caches/Musicable` |
| Linux | `$XDG_CACHE_HOME/musicable`, or `~/.cache/musicable` |

The `MUSICABLE_CACHE_DIR` environment variable picks another folder.
`analysis` grows by tens of kilobytes for each minute of audio, and
`renders` by about 21 MB for each minute of a stereo clip in Spectral, up
to the size on the Files page of the Settings window. Deleting the folder,
or anything in it, is safe: Musicable works it out again when it next needs
it.

## The settings file

`settings.toml` holds the audio settings, written whenever you apply new
ones, and every other setting, written as soon as it changes. Musicable
reads it as it starts:

```toml
format = "musicable-settings"
version = 1

[audio]
device = "{0.0.0.00000000}.{a1b2c3d4-...}"
exclusive = false
buffer_size = 128

[behaviour]
play_from = "start_marker"
middle_c = "c3"
warp_drifting = false

[analysis]
fast = true

[render_cache]
max_size_gb = 50
min_free_gb = 10

[appearance]
theme = "Studio.toml"

[workspaces]
current = "Mixing"

[profiles]
current = "Bitwig habits"

[export]
format = "wav"
bits = 24
dither = true
normalize = false
tail = true
mixdown = true
stems = false

[browser]
auto_preview = true
preview_volume = -6

[recording]
capture_minutes = 5

[[recording.offsets]]
device = "{0.0.0.00000000}.{a1b2c3d4-...}"
exclusive = true
sample_rate = 44100
frames = 878

[agents]
enabled = true
```

Every setting can be left out:

| Setting | What it is | Left out |
|---|---|---|
| `audio.device` | The device to play through, by its id. An ASIO driver's id starts with `asio:`; a build without ASIO plays through the default output instead, and says why. | The default output |
| `audio.exclusive` | Whether Musicable takes the device for itself, for the lowest latency. | `false` |
| `audio.sample_rate` | The rate the device plays at. | The rate it runs at |
| `audio.buffer_size` | The buffer size, in frames. | The smallest the device supports, or an ASIO driver's preferred size |
| `behaviour.play_from` | Where Play/Stop plays from: the start marker (`start_marker`) or where the playhead stopped (`playhead`). | `start_marker` |
| `behaviour.middle_c` | What the piano roll calls MIDI note 60: `c3`, `c4` or `c5`. | `c3` |
| `behaviour.warp_recordings` | Whether recordings follow the tempo they were played to. | `true` |
| `behaviour.warp_loops` | Whether loops of whole bars at a steady tempo warp to them. | `true` |
| `behaviour.warp_steady` | Whether files at a steady tempo warp from their first downbeat. | `true` |
| `behaviour.warp_drifting` | Whether files whose tempo drifts get a marker on every downbeat, and on the beats that stray. | `true` |
| `behaviour.sync_one_shots` | Whether sounds without a pulse play their first hit, or a riser's peak, where they're dropped. | `true` |
| `behaviour.tempo_from_first_file` | Whether the first file with a pulse dropped into an empty song gives it its tempo and meter. | `true` |
| `analysis.fast` | Whether analysis finds beats with the classic tracker alone, without the network. It's not one of a profile's settings. | `false` |
| `render_cache.max_size_gb` | The most the render cache holds, in GB of 2^30 bytes, from 1 to 10,000. It's not one of a profile's settings. | 20 |
| `render_cache.min_free_gb` | How much of its disk the render cache always leaves free, in GB, from 1 to 10,000. It's not one of a profile's settings. | 5 |
| `appearance.theme` | The theme: `dark`, `light`, or the name of a file in the `themes` folder. | `dark` |
| `workspaces.current` | The workspace in use, by name, which Musicable opens with unless the project it opens was saved with another. | Default |
| `profiles.current` | The profile in use, which Musicable opens with: one in the `profiles` folder, whose own settings file has `[behaviour]`, `[appearance]` and `[workspaces]` in place of these. | Default |
| `export` | The export window's settings as it was last used: `format` is `wav`, `aiff` or `flac`, `bits` is 16, 24, or 32 for floating point, a `sample_rate` renders at that rate instead of Musicable's, and the others are the window's ticks ([Exporting audio](../using/exporting.md)). | A 24-bit WAV of the mix, dithered, with the tail |
| `browser.auto_preview` | Whether picking a sample plays it. | `true` |
| `browser.preview_volume` | How loud previews play, in dB, from -60 to 6. | 0 |
| `browser.presets_folder` | The folder presets are saved in. | `Music/Musicable/Presets` in your home folder |
| `recording.capture_minutes` | How long Capture holds the inputs, in minutes: 0 turns it off. | 5 |
| `recording.offsets` | Each is the recording offset, in `frames`, of a `device`, by its id, in exclusive mode or not (`exclusive`), at a `sample_rate`. An offset of 0 isn't kept. | None |
| `agents.enabled` | Whether agents can control Musicable. | `false` |

The Settings window has a control for every setting but the export
window's, which that window keeps as it was last used. Settings this
version doesn't know are ignored, so a file from a later version still
opens. A file that can't be read leaves the default settings, and says why.
