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
//! ```
//!
//! Every setting is optional, and fields this version doesn't know are
//! ignored, so a file from a newer version still opens.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use musicable_audio::{Mode, StreamRequest};
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
    }

    #[test]
    fn requests_and_settings_match() {
        let request = StreamRequest {
            device: Some("speakers".into()),
            mode: Mode::Exclusive,
            sample_rate: Some(44_100),
            period: None,
        };
        assert_eq!(AudioSettings::from_request(&request).request(), request);
    }

    #[test]
    fn a_missing_file_holds_the_defaults() {
        let path = std::env::temp_dir().join("musicable-no-such-settings.toml");
        assert_eq!(Settings::read(&path).unwrap(), Settings::default());
    }
}
