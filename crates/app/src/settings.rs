//! The app's settings, kept in `settings.toml` in the config folder, next to
//! the user's keymap.
//!
//! ```toml
//! format = "musicable-settings"
//! version = 1
//!
//! [audio]
//! device = "{0.0.0.00000000}.{a1b2c3d4-...}"
//! exclusive = false
//! buffer_size = 128
//! render_ahead = "normal"
//!
//! [behaviour]
//! play_from = "start_marker"
//! middle_c = "c3"
//! warp_drifting = false
//!
//! [appearance]
//! theme = "Studio.toml"
//!
//! [workspaces]
//! current = "Mixing"
//!
//! [profiles]
//! current = "Bitwig habits"
//!
//! [export]
//! format = "wav"
//! bits = 24
//! dither = true
//! normalize = false
//! tail = true
//! mixdown = true
//! stems = false
//!
//! [browser]
//! auto_preview = true
//! preview_volume = -6
//!
//! [analysis]
//! fast = true
//!
//! [render_cache]
//! max_size_gb = 20
//! min_free_gb = 5
//!
//! [agents]
//! enabled = true
//! mouse_and_keys = false
//!
//! [plugins.kinds]
//! "vst3:ABCDEF0123456789ABCDEF0123456789" = "midi_effect"
//!
//! [plugins.render_ahead]
//! "clap:com.example.hardware-synth" = "never"
//!
//! [recording]
//! capture_minutes = 5
//!
//! [[recording.offsets]]
//! device = "{0.0.0.00000000}.{a1b2c3d4-...}"
//! exclusive = true
//! sample_rate = 44100
//! frames = 878
//! ```
//!
//! Every setting is optional, and fields this version doesn't know are
//! ignored, so a file from a newer version still opens.
//!
//! `[behaviour]`, `[appearance]` and `[workspaces]` go with a profile
//! ([`ProfileSettings`]): they're Default's here, and each of the user's
//! other profiles has a settings file of its own, in its folder, that holds
//! only them ([`crate::profiles`]). `[profiles]` says which is in use.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_audio::{Api, Mode, StreamInfo, StreamRequest};
use musicable_commands::editor::Behaviour;
use musicable_engine::Lead;
use musicable_model::PluginFormat;
use musicable_model::plugin::PluginKind;
use musicable_ui::{Base, DEFAULT_WORKSPACE, MAX_PREVIEW_VOLUME, MIN_PREVIEW_VOLUME};
use serde::{Deserialize, Serialize};

use crate::export::ExportSettings;
use crate::profiles::DEFAULT_PROFILE;
use crate::renders::{GB, Limits};

const FORMAT: &str = "musicable-settings";
const VERSION: u32 = 1;

/// The settings file's name in the config folder.
pub const SETTINGS_FILE: &str = "settings.toml";
/// The user's keymap file's name in the config folder.
pub const KEYMAP_FILE: &str = "keymap.toml";
/// The user's mouse map file's name in the config folder.
pub const MOUSE_FILE: &str = "mouse.toml";
/// The folder of the user's theme files, in the config folder.
pub const THEMES_FOLDER: &str = "themes";
/// The analysis cache's folder, in the cache folder.
pub const ANALYSIS_FOLDER: &str = "analysis";
/// The render cache's folder, in the cache folder.
pub const RENDER_FOLDER: &str = "renders";

/// The folder for the settings, the user's keymap, mouse map and themes:
/// `%APPDATA%\Musicable` on Windows, `~/Library/Application
/// Support/Musicable` on macOS, and `$XDG_CONFIG_HOME/musicable` or
/// `~/.config/musicable` elsewhere. The `MUSICABLE_CONFIG_DIR` environment
/// variable picks another. `None` when the system doesn't say where.
pub fn config_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = var("MUSICABLE_CONFIG_DIR") {
        return Some(dir.into());
    }
    if cfg!(windows) {
        var("APPDATA").map(|dir| Path::new(&dir).join("Musicable"))
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| Path::new(&home).join("Library/Application Support/Musicable"))
    } else {
        var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| var("HOME").map(|home| Path::new(&home).join(".config")))
            .map(|dir| dir.join("musicable"))
    }
}

/// The folder for what Musicable can work out again, such as what analysis
/// found in audio files: `%LOCALAPPDATA%\Musicable\Cache` on Windows,
/// `~/Library/Caches/Musicable` on macOS, and `$XDG_CACHE_HOME/musicable`
/// or `~/.cache/musicable` elsewhere. The `MUSICABLE_CACHE_DIR` environment
/// variable picks another. `None` when the system doesn't say where.
pub fn cache_dir() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = var("MUSICABLE_CACHE_DIR") {
        return Some(dir.into());
    }
    if cfg!(windows) {
        var("LOCALAPPDATA").map(|dir| Path::new(&dir).join("Musicable").join("Cache"))
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| Path::new(&home).join("Library/Caches/Musicable"))
    } else {
        var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .or_else(|| var("HOME").map(|home| Path::new(&home).join(".cache")))
            .map(|dir| dir.join("musicable"))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio: AudioSettings,
    /// How editing behaves where people's habits differ, such as where
    /// Play/Stop starts from: the start marker (`start_marker`), as in Live,
    /// or wherever the playhead stopped (`playhead`).
    pub behaviour: Behaviour,
    pub appearance: AppearanceSettings,
    pub workspaces: WorkspaceSettings,
    pub profiles: ProfilesSettings,
    /// How the export window writes files, as it was last used: see
    /// [`ExportSettings`].
    pub export: ExportSettings,
    pub browser: BrowserSettings,
    pub recording: RecordingSettings,
    pub analysis: AnalysisSettings,
    pub render_cache: RenderCacheSettings,
    pub agents: AgentSettings,
    /// Written only once a plugin is marked.
    #[serde(skip_serializing_if = "PluginsSettings::is_empty")]
    pub plugins: PluginsSettings,
}

/// How Musicable looks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// The theme Musicable draws with: `dark` or `light`, the built-in
    /// ones, or the name of a file in the themes folder, such as
    /// `Studio.toml`.
    pub theme: String,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme: Base::Dark.name().to_owned(),
        }
    }
}

/// Which workspace lays the panels out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSettings {
    /// The workspace in use, by name: the window opens with it, unless the
    /// project opened was saved with another.
    pub current: String,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            current: DEFAULT_WORKSPACE.to_owned(),
        }
    }
}

/// Which of the user's profiles is in use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfilesSettings {
    /// The profile in use, by name: Default, whose settings are in this
    /// file, or one in the profiles folder.
    pub current: String,
}

impl Default for ProfilesSettings {
    fn default() -> Self {
        Self {
            current: DEFAULT_PROFILE.to_owned(),
        }
    }
}

/// The settings that go with a profile, as its keys and its workspaces do:
/// how editing behaves, the theme, and the workspace in use. Default's are
/// in the settings file, and the user's other profiles' in settings files
/// of their own, which hold only these.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileSettings {
    pub behaviour: Behaviour,
    pub appearance: AppearanceSettings,
    pub workspaces: WorkspaceSettings,
}

impl ProfileSettings {
    /// Reads a profile's settings from the settings file at `path`, which
    /// may hold others: the defaults, with no file there.
    pub fn read(path: &Path) -> Result<Self, SettingsError> {
        Settings::read(path).map(|settings| settings.profile())
    }

    /// A settings file of these alone, holding those that differ from the
    /// defaults, as a keymap file holds the keys that differ from Live's,
    /// so a later version's new defaults still arrive.
    pub fn to_toml(&self) -> String {
        #[derive(Serialize)]
        struct Raw<'a> {
            format: &'a str,
            version: u32,
            #[serde(flatten)]
            changed: toml::Table,
        }

        let table =
            |profile: &Self| toml::Table::try_from(profile).expect("settings always serialize");
        let defaults = table(&Self::default());
        let mut changed = table(self);
        for (name, section) in changed.iter_mut() {
            let first = defaults.get(name).and_then(toml::Value::as_table);
            if let (Some(section), Some(first)) = (section.as_table_mut(), first) {
                section.retain(|key, value| first.get(key) != Some(value));
            }
        }
        changed.retain(|_, section| section.as_table().is_none_or(|section| !section.is_empty()));
        let raw = Raw {
            format: FORMAT,
            version: VERSION,
            changed,
        };
        toml::to_string(&raw).expect("settings always serialize")
    }

    /// Writes a settings file of these alone to `path`, creating its folder
    /// if need be.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_toml())
    }
}

/// How the browser previews samples, and where Musicable's own presets go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserSettings {
    /// Whether picking a sample in the browser plays it.
    pub auto_preview: bool,
    /// How loud previews play, in dB, from [`MIN_PREVIEW_VOLUME`] to
    /// [`MAX_PREVIEW_VOLUME`].
    pub preview_volume: i32,
    /// Where presets are saved, if not in the Presets folder of the
    /// Musicable folder in the user's music folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presets_folder: Option<PathBuf>,
}

impl Default for BrowserSettings {
    fn default() -> Self {
        Self {
            auto_preview: true,
            preview_volume: 0,
            presets_folder: None,
        }
    }
}

impl BrowserSettings {
    /// The gain previews play at.
    pub fn preview_gain(&self) -> f32 {
        let db = self
            .preview_volume
            .clamp(MIN_PREVIEW_VOLUME, MAX_PREVIEW_VOLUME);
        10f32.powf(db as f32 / 20.0)
    }
}

/// What kind of plugin the user marked plugins as, where the scan's rule
/// gets them wrong ([`browser::SET_PLUGIN_KIND`]), and the plugins they set
/// to Render Ahead Never ([`browser::SET_PLUGIN_RENDER_AHEAD`]).
///
/// [`browser::SET_PLUGIN_KIND`]: musicable_commands::browser::SET_PLUGIN_KIND
/// [`browser::SET_PLUGIN_RENDER_AHEAD`]: musicable_commands::browser::SET_PLUGIN_RENDER_AHEAD
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginsSettings {
    /// Each plugin marked, by its format and its id, as
    /// `"clap:org.example.arp"`, and what it's marked as: `instrument`,
    /// `midi_effect` or `audio_effect`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kinds: BTreeMap<String, String>,
    /// Each plugin set to Render Ahead Never, by its format and its id, as
    /// `never`. The others are at Auto.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub render_ahead: BTreeMap<String, String>,
}

/// What the settings call the plugin `id` in `format`: `"clap:org.example.arp"`.
fn plugin_key(format: PluginFormat, id: &str) -> String {
    let format = match format {
        PluginFormat::Clap => "clap",
        PluginFormat::Vst3 => "vst3",
    };
    format!("{format}:{id}")
}

/// The plugin the settings call `key`, by format and id, if the format is
/// one there is.
fn parse_plugin_key(key: &str) -> Option<(PluginFormat, String)> {
    let (format, id) = key.split_once(':')?;
    let format = match format {
        "clap" => PluginFormat::Clap,
        "vst3" => PluginFormat::Vst3,
        _ => return None,
    };
    Some((format, id.to_owned()))
}

impl PluginsSettings {
    /// Whether no plugin is marked or set.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty() && self.render_ahead.is_empty()
    }

    /// The marks, by format and id, leaving out those the file has wrong.
    pub fn marks(&self) -> BTreeMap<(PluginFormat, String), PluginKind> {
        (self.kinds.iter())
            .filter_map(|(plugin, kind)| {
                Some((parse_plugin_key(plugin)?, PluginKind::from_key(kind)?))
            })
            .collect()
    }

    /// The plugins set to Render Ahead Never, by format and id, leaving out
    /// those the file has wrong.
    pub fn never_ahead(&self) -> BTreeSet<(PluginFormat, String)> {
        (self.render_ahead.iter())
            .filter(|(_, render_ahead)| *render_ahead == "never")
            .filter_map(|(plugin, _)| parse_plugin_key(plugin))
            .collect()
    }

    /// The settings that hold `marks` and the plugins set to Render Ahead
    /// Never, `never`.
    pub fn new(
        marks: &BTreeMap<(PluginFormat, String), PluginKind>,
        never: &BTreeSet<(PluginFormat, String)>,
    ) -> Self {
        let kinds = (marks.iter())
            .map(|((format, id), kind)| (plugin_key(*format, id), kind.key().to_owned()))
            .collect();
        let render_ahead = (never.iter())
            .map(|(format, id)| (plugin_key(*format, id), "never".to_owned()))
            .collect();
        Self {
            kinds,
            render_ahead,
        }
    }
}

/// How Musicable analyses audio files for warping: see `docs/analysis.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    /// Whether analysis leaves the Beat This! network out, finding beats
    /// with the classic tracker alone: quicker on slower computers, but
    /// less accurate. Off, as it is at first, the network finds them.
    pub fast: bool,
}

/// How much the render cache keeps of the audio that clips in Spectral play,
/// rendered ahead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenderCacheSettings {
    /// The most it holds, in GB.
    pub max_size_gb: u32,
    /// How much of its disk it always leaves free, in GB.
    pub min_free_gb: u32,
}

impl Default for RenderCacheSettings {
    fn default() -> Self {
        Self {
            max_size_gb: 20,
            min_free_gb: 5,
        }
    }
}

impl RenderCacheSettings {
    /// The largest size and free space the settings take, in GB.
    pub const MAX_GB: u32 = 10_000;

    /// The limits the render cache keeps to, each from 1 GB to
    /// [`MAX_GB`](Self::MAX_GB), whatever the file says.
    pub(crate) fn limits(&self) -> Limits {
        let gb = |n: u32| u64::from(n.clamp(1, Self::MAX_GB)) * GB;
        Limits {
            most: gb(self.max_size_gb),
            free: gb(self.min_free_gb),
        }
    }
}

/// Whether agents, such as Claude Code, can control Musicable: see
/// `docs/agents.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// Whether Musicable listens for agents, on a pipe only the user's
    /// account can open. Off until the user switches it on.
    pub enabled: bool,
    /// Whether agents may use the mouse and keys in Musicable's windows,
    /// as its input, never the system's pointer. Off until the user
    /// switches it on, and agents can't.
    pub mouse_and_keys: bool,
}

/// How Musicable records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    /// How many minutes of the inputs of armed tracks, and of those
    /// monitoring In, Capture holds in memory, at least: 0 turns it off.
    pub capture_minutes: u32,
    /// How late audio recorded through each device lands.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offsets: Vec<RecordingOffset>,
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            capture_minutes: 5,
            offsets: Vec::new(),
        }
    }
}

impl RecordingSettings {
    /// The recording offset of the stream `info` describes: 0 unless one
    /// was set for its device, in its mode and at its rate.
    pub fn offset_for(&self, info: &StreamInfo) -> i64 {
        let key = key(info);
        (self.offsets.iter())
            .find(|offset| offset.is_for(key))
            .map_or(0, |offset| offset.frames)
    }

    /// Sets the recording offset of the stream `info` describes, for its
    /// device, in its mode and at its rate. 0 leaves it out.
    pub fn set_offset_for(&mut self, info: &StreamInfo, frames: i64) {
        let key = key(info);
        self.offsets.retain(|offset| !offset.is_for(key));
        let (device, exclusive, sample_rate) = key;
        if frames != 0 {
            self.offsets.push(RecordingOffset {
                device: device.to_owned(),
                exclusive,
                sample_rate,
                frames,
            });
        }
    }
}

/// What a recording offset is for: a device, in exclusive mode or not, at
/// a rate. ASIO drivers have no modes.
fn key(info: &StreamInfo) -> (&str, bool, u32) {
    let exclusive = info.api == Api::WasapiExclusive;
    (&info.device.id, exclusive, info.sample_rate)
}

/// How many frames late audio recorded through a device lands, in a mode
/// and at a rate, beyond the latency its driver reports: what a cable from
/// one of its outputs to one of its inputs measures. Each audio take
/// recorded through it starts that much later in what the input captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingOffset {
    /// The device's id, as in [`AudioSettings::device`].
    pub device: String,
    /// Whether it's for the device in exclusive mode.
    #[serde(default)]
    pub exclusive: bool,
    pub sample_rate: u32,
    pub frames: i64,
}

impl RecordingOffset {
    fn is_for(&self, (device, exclusive, sample_rate): (&str, bool, u32)) -> bool {
        self.device == device && self.exclusive == exclusive && self.sample_rate == sample_rate
    }
}

/// The audio device to play through, and how.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// The device's id, or none for the system's default output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Whether to take the device for ourselves, for the lowest latency.
    pub exclusive: bool,
    /// None for the rate the device runs at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Frames per period, or none for the smallest the device supports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<u32>,
    /// How far ahead of the device the engine renders what doesn't play
    /// live.
    pub render_ahead: RenderAhead,
}

/// How far ahead of the audio device the engine renders the project, all
/// but what plays live, as a track monitoring its input or set to Always
/// Live does with what it reaches: `off`, `short` (about 20 ms), `normal`
/// (about 50 ms) or `long` (about 200 ms).
/// See [`EngineHandle::set_lead`](musicable_engine::EngineHandle::set_lead).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderAhead {
    Off,
    Short,
    #[default]
    Normal,
    Long,
}

impl RenderAhead {
    pub const ALL: [RenderAhead; 4] = [
        RenderAhead::Off,
        RenderAhead::Short,
        RenderAhead::Normal,
        RenderAhead::Long,
    ];

    /// The engine's lead for it.
    pub fn lead(self) -> Lead {
        match self {
            RenderAhead::Off => Lead::Off,
            RenderAhead::Short => Lead::Short,
            RenderAhead::Normal => Lead::Normal,
            RenderAhead::Long => Lead::Long,
        }
    }
}

impl AudioSettings {
    /// The stream these settings ask for.
    pub fn request(&self) -> StreamRequest {
        StreamRequest {
            device: self.device.clone(),
            mode: if self.exclusive {
                Mode::Exclusive
            } else {
                Mode::Shared
            },
            sample_rate: self.sample_rate,
            period: self.buffer_size,
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
        }
    }

    /// Asks for the stream `request` asks for from now on. Render Ahead
    /// stays as it is.
    pub fn set_request(&mut self, request: &StreamRequest) {
        self.device = request.device.clone();
        self.exclusive = request.mode == Mode::Exclusive;
        self.sample_rate = request.sample_rate;
        self.buffer_size = request.period;
    }
}

impl Settings {
    /// Reads settings from the text of a settings file.
    pub fn parse(text: &str) -> Result<Self, SettingsError> {
        #[derive(Deserialize)]
        struct Raw {
            format: Option<String>,
            version: Option<u32>,
            #[serde(flatten)]
            settings: Settings,
        }

        let raw: Raw = toml::from_str(text).map_err(SettingsError::Toml)?;
        if let Some(format) = raw.format.filter(|f| f != FORMAT) {
            return Err(SettingsError::NotSettings(format));
        }
        match raw.version.unwrap_or(VERSION) {
            VERSION => Ok(raw.settings),
            version => Err(SettingsError::UnsupportedVersion(version)),
        }
    }

    /// The settings that go with Default, the profile whose settings are
    /// these.
    pub fn profile(&self) -> ProfileSettings {
        ProfileSettings {
            behaviour: self.behaviour,
            appearance: self.appearance.clone(),
            workspaces: self.workspaces.clone(),
        }
    }

    /// Keeps `profile` as the settings that go with Default.
    pub fn set_profile(&mut self, profile: ProfileSettings) {
        self.behaviour = profile.behaviour;
        self.appearance = profile.appearance;
        self.workspaces = profile.workspaces;
    }

    /// Reads the settings file at `path`. With no file there, the settings
    /// are the defaults.
    pub fn read(path: &Path) -> Result<Self, SettingsError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(SettingsError::Io(error)),
        }
    }

    pub fn to_toml(&self) -> String {
        #[derive(Serialize)]
        struct Raw<'a> {
            format: &'a str,
            version: u32,
            #[serde(flatten)]
            settings: &'a Settings,
        }

        let raw = Raw {
            format: FORMAT,
            version: VERSION,
            settings: self,
        };
        toml::to_string(&raw).expect("settings always serialize")
    }

    /// Writes the settings to `path`, creating its folder if need be.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, self.to_toml())
    }
}

/// Why a settings file can't be used.
#[derive(Debug)]
pub enum SettingsError {
    Io(io::Error),
    Toml(toml::de::Error),
    /// The file's `format` says it's another kind of file.
    NotSettings(String),
    /// The file is from a version of Musicable that writes another version.
    UnsupportedVersion(u32),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "can't read the settings file: {error}"),
            Self::Toml(error) => write!(f, "the settings file isn't valid: {error}"),
            Self::NotSettings(format) => {
                write!(f, "this is a \"{format}\" file, not a \"{FORMAT}\" file")
            }
            Self::UnsupportedVersion(version) => write!(
                f,
                "the settings file is version {version}, and this version of Musicable reads \
                 version {VERSION}"
            ),
        }
    }
}

impl std::error::Error for SettingsError {}

#[cfg(test)]
mod tests {
    use musicable_commands::editor::{MiddleC, PlayFrom};

    use super::*;
    use crate::export::{Bits, FileFormat};

    #[test]
    fn settings_survive_a_round_trip() {
        let settings = Settings {
            audio: AudioSettings {
                device: Some("{0.0.0.00000000}.{guid}".into()),
                exclusive: true,
                sample_rate: Some(96_000),
                buffer_size: Some(64),
                render_ahead: RenderAhead::Long,
            },
            behaviour: Behaviour {
                play_from: PlayFrom::Playhead,
                middle_c: MiddleC::C4,
                warp_drifting: false,
                ..Behaviour::default()
            },
            appearance: AppearanceSettings {
                theme: "Studio.toml".into(),
            },
            workspaces: WorkspaceSettings {
                current: "Mixing".into(),
            },
            profiles: ProfilesSettings {
                current: "Bitwig habits".into(),
            },
            export: ExportSettings {
                format: FileFormat::Aiff,
                bits: Bits::Int16,
                sample_rate: Some(44_100),
                stems: true,
                ..ExportSettings::default()
            },
            browser: BrowserSettings {
                auto_preview: false,
                preview_volume: -12,
                presets_folder: Some(PathBuf::from("/music/presets")),
            },
            recording: RecordingSettings {
                capture_minutes: 0,
                offsets: vec![RecordingOffset {
                    device: "{0.0.0.00000000}.{guid}".into(),
                    exclusive: true,
                    sample_rate: 44_100,
                    frames: 878,
                }],
            },
            analysis: AnalysisSettings { fast: true },
            render_cache: RenderCacheSettings {
                max_size_gb: 50,
                min_free_gb: 10,
            },
            agents: AgentSettings {
                enabled: true,
                mouse_and_keys: true,
            },
            plugins: PluginsSettings {
                kinds: BTreeMap::from([(
                    "vst3:ABCDEF0123456789ABCDEF0123456789".to_owned(),
                    "midi_effect".to_owned(),
                )]),
                render_ahead: BTreeMap::from([(
                    "clap:com.example.hardware-synth".to_owned(),
                    "never".to_owned(),
                )]),
            },
        };
        let text = settings.to_toml();
        assert!(text.starts_with("format = \"musicable-settings\"\nversion = 1\n"));
        assert_eq!(Settings::parse(&text).unwrap(), settings);
        assert_eq!(
            Settings::parse(&Settings::default().to_toml()).unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn settings_files_can_leave_things_out() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        let settings = Settings::parse("[audio]\nbuffer_size = 256\nlater = true\n").unwrap();
        assert_eq!(settings.audio.buffer_size, Some(256));
        // The engine renders about 50 ms ahead.
        assert_eq!(settings.audio.render_ahead, RenderAhead::Normal);
        assert_eq!(settings.audio.render_ahead.lead(), Lead::Normal);
        assert_eq!(settings.behaviour, Behaviour::default());
        assert_eq!(settings.recording.capture_minutes, 5);
        assert_eq!(settings.appearance.theme, "dark");
        assert_eq!(settings.workspaces.current, "Default");
        assert_eq!(settings.profiles.current, "Default");
        // Agents can't control Musicable until the user lets them.
        assert!(!settings.agents.enabled);
        // Auto-warp's rules are all on, and analysis uses the network.
        assert!(settings.behaviour.warp_loops && settings.behaviour.tempo_from_first_file);
        assert!(!settings.analysis.fast);
        // The render cache holds up to 20 GB, leaving 5 GB of the disk, and
        // at least 1 GB of each, whatever the file says.
        let limits = settings.render_cache.limits();
        assert_eq!((limits.most, limits.free), (20 << 30, 5 << 30));
        let none = RenderCacheSettings {
            max_size_gb: 0,
            min_free_gb: u32::MAX,
        };
        let limits = none.limits();
        assert_eq!((limits.most, limits.free), (1 << 30, 10_000 << 30));
        assert_eq!(
            settings.audio.request(),
            StreamRequest {
                period: Some(256),
                ..StreamRequest::default()
            }
        );
        let export = Settings::parse("[export]\nformat = \"flac\"\n")
            .unwrap()
            .export;
        assert_eq!(export.format, FileFormat::Flac);
        assert_eq!(export.bits, ExportSettings::default().bits);
        // Previews play when a sample's picked unless the file says not.
        let browser = Settings::parse("[browser]\npreview_volume = -6\n")
            .unwrap()
            .browser;
        assert!(browser.auto_preview);
        assert!((browser.preview_gain() - 0.501).abs() < 0.001);
        assert_eq!(BrowserSettings::default().preview_gain(), 1.0);
    }

    #[test]
    fn plugins_marked_as_kinds_go_by_format_and_id() {
        let text = "[plugins.kinds]\n\
                    \"clap:org.example.arp\" = \"midi_effect\"\n\
                    \"vst3:ABCD\" = \"instrument\"\n\
                    \"au:org.example.old\" = \"instrument\"\n\
                    \"clap:org.example.wrong\" = \"synth\"\n";
        let plugins = Settings::parse(text).unwrap().plugins;
        let marks = plugins.marks();
        let expected = BTreeMap::from([
            (
                (PluginFormat::Clap, "org.example.arp".to_owned()),
                PluginKind::MidiEffect,
            ),
            (
                (PluginFormat::Vst3, "ABCD".to_owned()),
                PluginKind::Instrument,
            ),
        ]);
        assert_eq!(marks, expected);
        let kept = PluginsSettings::new(&marks, &BTreeSet::new());
        assert_eq!(kept.marks(), marks);
        assert_eq!(kept.kinds.len(), 2);
        // None at first, and none written.
        assert!(!Settings::default().to_toml().contains("plugins"));
    }

    #[test]
    fn plugins_set_to_render_ahead_never_go_by_format_and_id() {
        let text = "[plugins.render_ahead]\n\
                    \"clap:com.example.hardware\" = \"never\"\n\
                    \"vst3:ABCD\" = \"auto\"\n\
                    \"au:com.example.old\" = \"never\"\n";
        let plugins = Settings::parse(text).unwrap().plugins;
        let never = plugins.never_ahead();
        let hardware = (PluginFormat::Clap, "com.example.hardware".to_owned());
        assert_eq!(never, BTreeSet::from([hardware]));
        let kept = PluginsSettings::new(&BTreeMap::new(), &never);
        assert_eq!(kept.never_ahead(), never);
        let text = Settings {
            plugins: kept,
            ..Settings::default()
        }
        .to_toml();
        assert!(text.contains("[plugins.render_ahead]\n\"clap:com.example.hardware\" = \"never\""));
        assert!(!text.contains("[plugins.kinds]"));
    }

    #[test]
    fn other_files_are_refused() {
        assert!(matches!(
            Settings::parse("format = \"musicable-keymap\""),
            Err(SettingsError::NotSettings(_))
        ));
        assert!(matches!(
            Settings::parse("version = 2"),
            Err(SettingsError::UnsupportedVersion(2))
        ));
        assert!(matches!(
            Settings::parse("[audio]\nexclusive = \"yes\""),
            Err(SettingsError::Toml(_))
        ));
        assert!(matches!(
            Settings::parse("[behaviour]\nplay_from = \"anywhere\""),
            Err(SettingsError::Toml(_))
        ));
        assert!(matches!(
            Settings::parse("[audio]\nrender_ahead = \"far\""),
            Err(SettingsError::Toml(_))
        ));
    }

    #[test]
    fn requests_and_settings_match() {
        let request = StreamRequest {
            device: Some("speakers".into()),
            mode: Mode::Exclusive,
            sample_rate: Some(44_100),
            period: None,
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
        };
        let mut settings = AudioSettings {
            render_ahead: RenderAhead::Off,
            ..AudioSettings::default()
        };
        settings.set_request(&request);
        assert_eq!(settings.request(), request);
        // Render Ahead isn't the stream's.
        assert_eq!(settings.render_ahead, RenderAhead::Off);
    }

    #[test]
    fn recording_offsets_are_kept_for_each_device_mode_and_rate() {
        let info = |id: &str, api, sample_rate| StreamInfo {
            device: musicable_audio::Device {
                id: id.to_owned(),
                name: id.to_owned(),
                is_default: false,
                driver_type: musicable_audio::DriverType::System,
            },
            api,
            sample_rate,
            channels: 2,
            format: musicable_audio::SampleFormat::F32,
            period: 441,
            buffer: 882,
            latency: 441,
            realtime: true,
            inputs: Vec::new(),
            midi_inputs: Vec::new(),
        };
        let exclusive = info("rme", Api::WasapiExclusive, 44_100);
        let shared = info("rme", Api::WasapiShared, 44_100);
        let low_latency = info("rme", Api::WasapiLowLatency, 44_100);
        let asio = info("asio:RME", Api::Asio, 44_100);
        let mut recording = RecordingSettings::default();
        assert_eq!(recording.offset_for(&exclusive), 0);
        recording.set_offset_for(&exclusive, 878);
        recording.set_offset_for(&shared, 1_002);
        recording.set_offset_for(&asio, -3);
        assert_eq!(recording.offset_for(&exclusive), 878);
        // Shared mode is shared mode, at the audio engine's period or not.
        assert_eq!(recording.offset_for(&low_latency), 1_002);
        assert_eq!(recording.offset_for(&asio), -3);
        let other_rate = info("rme", Api::WasapiExclusive, 48_000);
        let other_device = info("speakers", Api::WasapiShared, 44_100);
        assert_eq!(recording.offset_for(&other_rate), 0);
        assert_eq!(recording.offset_for(&other_device), 0);
        // Setting one again replaces it, and 0 leaves it out.
        recording.set_offset_for(&exclusive, 879);
        recording.set_offset_for(&asio, 0);
        assert_eq!(recording.offsets.len(), 2);
        assert_eq!(recording.offset_for(&exclusive), 879);
        assert_eq!(recording.offset_for(&asio), 0);
        let settings = Settings {
            recording,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&settings.to_toml()).unwrap(), settings);
    }

    #[test]
    fn profiles_have_settings_files_of_their_own() {
        let profile = ProfileSettings {
            behaviour: Behaviour {
                play_from: PlayFrom::Playhead,
                middle_c: MiddleC::C4,
                ..Behaviour::default()
            },
            appearance: AppearanceSettings {
                theme: "light".into(),
            },
            workspaces: WorkspaceSettings {
                current: "Mixing".into(),
            },
        };
        let text = profile.to_toml();
        assert!(text.starts_with("format = \"musicable-settings\"\nversion = 1\n"));
        for left_out in [
            "[audio]",
            "[export]",
            "[browser]",
            "[recording]",
            "[profiles]",
            "[analysis]",
            "[agents]",
        ] {
            assert!(!text.contains(left_out), "{text}");
        }
        let read = Settings::parse(&text).unwrap();
        assert_eq!(read.profile(), profile);
        assert_eq!(read.audio, AudioSettings::default());
        // It holds what differs from the defaults.
        let light = ProfileSettings {
            appearance: profile.appearance.clone(),
            ..ProfileSettings::default()
        };
        assert_eq!(
            light.to_toml(),
            "format = \"musicable-settings\"\nversion = 1\n\n[appearance]\ntheme = \"light\"\n"
        );
        let middle_c = ProfileSettings {
            behaviour: Behaviour {
                middle_c: MiddleC::C4,
                ..Behaviour::default()
            },
            ..ProfileSettings::default()
        };
        assert_eq!(
            Settings::parse(&middle_c.to_toml()).unwrap().profile(),
            middle_c
        );
        assert!(!middle_c.to_toml().contains("play_from"));
        assert_eq!(
            ProfileSettings::default().to_toml(),
            "format = \"musicable-settings\"\nversion = 1\n"
        );
        // Default's are the settings file's own.
        let mut settings = Settings::default();
        settings.set_profile(profile.clone());
        assert_eq!(settings.profile(), profile);
        assert_eq!(settings.behaviour.middle_c, MiddleC::C4);
        // A profile's file with the computer's settings in it gives only the
        // profile's.
        let dir = std::env::temp_dir().join(format!("musicable-profile-{}", std::process::id()));
        let path = dir.join("Bitwig habits").join(SETTINGS_FILE);
        settings.audio.buffer_size = Some(64);
        settings.write(&path).unwrap();
        assert_eq!(ProfileSettings::read(&path).unwrap(), profile);
        profile.write(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(
            ProfileSettings::read(&dir.join(SETTINGS_FILE)).unwrap(),
            ProfileSettings::default()
        );
    }

    #[test]
    fn a_missing_file_holds_the_defaults() {
        let path = std::env::temp_dir().join("musicable-no-such-settings.toml");
        assert_eq!(Settings::read(&path).unwrap(), Settings::default());
    }
}
