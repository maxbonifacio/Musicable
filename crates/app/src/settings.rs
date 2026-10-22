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
//!
//! [behaviour]
//! play_from = "start_marker"
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

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_audio::{Api, Mode, StreamInfo, StreamRequest};
use musicable_commands::editor::Behaviour;
use serde::{Deserialize, Serialize};

const FORMAT: &str = "musicable-settings";
const VERSION: u32 = 1;

/// The settings file's name in the config folder.
pub const SETTINGS_FILE: &str = "settings.toml";
/// The user's keymap file's name in the config folder.
pub const KEYMAP_FILE: &str = "keymap.toml";

/// The folder for the settings and the user's keymap: `%APPDATA%\Musicable`
/// on Windows, `~/Library/Application Support/Musicable` on macOS, and
/// `$XDG_CONFIG_HOME/musicable` or `~/.config/musicable` elsewhere. The
/// `MUSICABLE_CONFIG_DIR` environment variable picks another. `None` when
/// the system doesn't say where.
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio: AudioSettings,
    /// How editing behaves where people's habits differ, such as where
    /// Play/Stop starts from: the start marker (`start_marker`), as in Live,
    /// or wherever the playhead stopped (`playhead`).
    pub behaviour: Behaviour,
    pub recording: RecordingSettings,
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

    pub fn from_request(request: &StreamRequest) -> Self {
        Self {
            device: request.device.clone(),
            exclusive: request.mode == Mode::Exclusive,
            sample_rate: request.sample_rate,
            buffer_size: request.period,
        }
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
    use musicable_commands::editor::PlayFrom;

    use super::*;

    #[test]
    fn settings_survive_a_round_trip() {
        let settings = Settings {
            audio: AudioSettings {
                device: Some("{0.0.0.00000000}.{guid}".into()),
                exclusive: true,
                sample_rate: Some(96_000),
                buffer_size: Some(64),
            },
            behaviour: Behaviour {
                play_from: PlayFrom::Playhead,
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
        assert_eq!(settings.behaviour, Behaviour::default());
        assert_eq!(settings.recording.capture_minutes, 5);
        assert_eq!(
            settings.audio.request(),
            StreamRequest {
                period: Some(256),
                ..StreamRequest::default()
            }
        );
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
        assert_eq!(AudioSettings::from_request(&request).request(), request);
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
    fn a_missing_file_holds_the_defaults() {
        let path = std::env::temp_dir().join("musicable-no-such-settings.toml");
        assert_eq!(Settings::read(&path).unwrap(), Settings::default());
    }
}
