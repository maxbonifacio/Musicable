//! Presets: a device's settings in a file of their own, to put on a track
//! again.
//!
//! Musicable's own presets, `.musicable-preset` files, are project files
//! that hold the one device, on the master and nothing else. So they keep
//! everything a project keeps of a device, and open in every later version
//! as projects do. The browser also finds VST3 plugins' own preset files,
//! `.vstpreset`, in the folders VST3 keeps them in. A project keeps a VST3
//! plugin's state as such a file, so a device made from one starts with the
//! file as its state.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use musicable_io::project;
use musicable_model::plugin::PluginState;
use musicable_model::{DeviceKind, PluginFormat, Project};

use crate::scan::walk;

/// The file extension for Musicable's own presets, without the dot.
pub const EXTENSION: &str = "musicable-preset";
/// The file extension for VST3 preset files, without the dot.
pub const VST3_EXTENSION: &str = "vstpreset";

/// A preset file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub path: PathBuf,
    /// The file's name, without its extension.
    pub name: String,
    pub kind: PresetKind,
}

/// What a preset is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PresetKind {
    /// A Musicable preset of a plugin: its format, its id in the format,
    /// and its name.
    Plugin {
        format: PluginFormat,
        id: String,
        name: String,
    },
    /// A Musicable preset of one of Musicable's own devices, by its name.
    Device { name: String },
    /// A VST3 preset file, for the VST3 plugin with this class id, as 32
    /// hex digits in capitals: its id in Musicable.
    Vst3 { class_id: String },
}

impl Preset {
    /// The plugin the preset is for, by format and id, if it's for one.
    pub fn plugin(&self) -> Option<(PluginFormat, &str)> {
        match &self.kind {
            PresetKind::Plugin { format, id, .. } => Some((*format, id)),
            PresetKind::Vst3 { class_id } => Some((PluginFormat::Vst3, class_id)),
            PresetKind::Device { .. } => None,
        }
    }
}

/// What's in a preset file.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded {
    /// A device, set as the preset has it: one of Musicable's own presets.
    Device(DeviceKind),
    /// A VST3 plugin's state, for the plugin with this class id: a VST3
    /// preset file. The plugin's name and whether it plays notes come from
    /// the plugin.
    Vst3 {
        class_id: String,
        state: PluginState,
    },
}

/// Why a preset didn't load or save.
#[derive(Debug)]
pub enum PresetError {
    /// A VST3 preset file couldn't be read, or a folder for a Musicable one
    /// couldn't be made.
    Io(io::Error),
    /// A Musicable preset didn't load as a project.
    Load(project::LoadError),
    /// A Musicable preset didn't save as a project.
    Save(project::SaveError),
    /// The file is no preset: a project with other than one device on the
    /// master and nothing else, or a broken VST3 preset file.
    NotAPreset,
}

impl fmt::Display for PresetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Load(error) => error.fmt(f),
            Self::Save(error) => error.fmt(f),
            Self::NotAPreset => f.write_str("the file isn't a preset"),
        }
    }
}

impl Error for PresetError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Load(error) => Some(error),
            Self::Save(error) => Some(error),
            Self::NotAPreset => None,
        }
    }
}

/// Saves `device` as a Musicable preset at `path`, making the folders it
/// goes in, and replacing a file already there.
pub fn save(path: &Path, device: &DeviceKind) -> Result<(), PresetError> {
    let mut project = Project::new();
    let device = project.new_device(device.clone());
    project.master.devices.push(device);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(PresetError::Io)?;
    }
    project::save(path, &project).map_err(PresetError::Save)
}

/// Reads the preset at `path`: one of Musicable's own, or a VST3 preset
/// file, by its extension.
pub fn load(path: &Path) -> Result<Loaded, PresetError> {
    if has_extension(path, VST3_EXTENSION) {
        let file = fs::read(path).map_err(PresetError::Io)?;
        let class_id = vst3_class_id(&file).ok_or(PresetError::NotAPreset)?;
        return Ok(Loaded::Vst3 {
            class_id,
            state: PluginState::new(file),
        });
    }
    let project = project::load(path).map_err(PresetError::Load)?;
    the_device(project)
        .map(Loaded::Device)
        .ok_or(PresetError::NotAPreset)
}

/// The presets in `folders` and the folders under them, by name: Musicable's
/// own, and VST3 preset files. Files that can't be read or aren't presets
/// are left out, and so are folders that aren't there. It reads every
/// Musicable preset, and the first bytes of every VST3 one. It stops when
/// `wanted` says no, returning `None`.
pub fn find(folders: &[PathBuf], wanted: &dyn Fn() -> bool) -> Option<Vec<Preset>> {
    let mut presets = Vec::new();
    for folder in folders {
        let walked = walk(folder, wanted, &mut |_, path, _| {
            if let Some(preset) = read(path) {
                presets.push(preset);
            }
        });
        if let Ok(false) = walked {
            return None;
        }
    }
    presets.sort_by_cached_key(|preset| (preset.name.to_lowercase(), preset.path.clone()));
    // Folders given twice, or one inside another.
    presets.dedup_by(|a, b| a.path == b.path);
    Some(presets)
}

/// The preset at `path`, if it's one.
fn read(path: PathBuf) -> Option<Preset> {
    let kind = if has_extension(&path, VST3_EXTENSION) {
        let mut header = Vec::with_capacity(VST3_HEADER);
        File::open(&path)
            .ok()?
            .take(VST3_HEADER as u64)
            .read_to_end(&mut header)
            .ok()?;
        PresetKind::Vst3 {
            class_id: vst3_class_id(&header)?,
        }
    } else if has_extension(&path, EXTENSION) {
        kind_of(&the_device(project::load(&path).ok()?)?)
    } else {
        return None;
    };
    let name = path.file_stem()?.to_str()?.to_owned();
    Some(Preset { path, name, kind })
}

/// What a preset of `device` is for.
fn kind_of(device: &DeviceKind) -> PresetKind {
    match device {
        DeviceKind::Plugin(plugin) => PresetKind::Plugin {
            format: plugin.format,
            id: plugin.id.clone(),
            name: plugin.name.clone(),
        },
        DeviceKind::TestGain { .. } => PresetKind::Device {
            name: "Test Gain".to_owned(),
        },
        DeviceKind::TestInstrument { .. } => PresetKind::Device {
            name: "Test Instrument".to_owned(),
        },
    }
}

/// The device of a project that's a preset: the one on the master, with
/// no tracks.
fn the_device(mut project: Project) -> Option<DeviceKind> {
    if !project.tracks.is_empty() || project.master.devices.len() != 1 {
        return None;
    }
    project.master.devices.pop().map(|device| device.kind)
}

/// A VST3 preset file's header: `VST3`, the version, the class id as 32 hex
/// digits, and where the list of chunks starts.
const VST3_HEADER: usize = 48;

/// The class id in a VST3 preset file's header, in capitals, if `file`
/// starts with one.
fn vst3_class_id(file: &[u8]) -> Option<String> {
    let header = file.get(..VST3_HEADER)?;
    let version = i32::from_le_bytes(header[4..8].try_into().ok()?);
    let class_id = &header[8..40];
    let valid =
        &header[..4] == b"VST3" && version >= 1 && class_id.iter().all(u8::is_ascii_hexdigit);
    valid.then(|| String::from_utf8_lossy(class_id).to_ascii_uppercase())
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// The path for a Musicable preset called `name` of a device called
/// `device`, in `folder`: a folder for each device, named after it, with a
/// file for each of its presets. Characters that file names can't have
/// become `_`.
pub fn path_for(folder: &Path, device: &str, name: &str) -> PathBuf {
    folder
        .join(file_name(device, "Device"))
        .join(format!("{}.{EXTENSION}", file_name(name, "Preset")))
}

/// `name` as a file name every system takes, or `otherwise` if nothing's
/// left of it.
fn file_name(name: &str, otherwise: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows drops dots and spaces at the end of a name, and names that
    // start with a dot are hidden elsewhere.
    let name = name
        .trim()
        .trim_end_matches('.')
        .trim_start_matches('.')
        .trim();
    if name.is_empty() {
        otherwise.to_owned()
    } else {
        name.to_owned()
    }
}

/// The folders VST3 keeps preset files in, the user's first, as the VST3
/// SDK lays them out: a folder for each company, and one in that for each
/// plugin. Folders the system doesn't say where they are are left out.
pub fn vst3_folders() -> Vec<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    let mut folders = Vec::new();
    if cfg!(windows) {
        let documents = documents()
            .or_else(|| var("USERPROFILE").map(|home| Path::new(&home).join("Documents")));
        folders.extend(documents.map(|documents| documents.join("VST3 Presets")));
        folders.extend(var("PROGRAMDATA").map(|data| Path::new(&data).join("VST3 Presets")));
    } else if cfg!(target_os = "macos") {
        folders.extend(var("HOME").map(|home| Path::new(&home).join("Library/Audio/Presets")));
        folders.push("/Library/Audio/Presets".into());
        folders.push("/Network/Library/Audio/Presets".into());
    } else {
        folders.extend(var("HOME").map(|home| Path::new(&home).join(".vst3/presets")));
        folders.push("/usr/share/vst3/presets".into());
        folders.push("/usr/local/share/vst3/presets".into());
    }
    folders
}

/// The user's documents folder, wherever it was moved to, as when OneDrive
/// keeps it.
#[cfg(windows)]
fn documents() -> Option<PathBuf> {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// `FOLDERID_Documents`.
    const DOCUMENTS: Guid = Guid {
        data1: 0xFDD3_9AD0,
        data2: 0x238F,
        data3: 0x46AF,
        data4: [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7],
    };

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            id: *const Guid,
            flags: u32,
            token: *mut c_void,
            path: *mut *mut u16,
        ) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: *mut c_void);
    }

    let mut path = ptr::null_mut();
    // SAFETY: the id and the out pointer are valid for the call, and a null
    // token means the calling user.
    let result = unsafe { SHGetKnownFolderPath(&DOCUMENTS, 0, ptr::null_mut(), &mut path) };
    let documents = (result == 0 && !path.is_null()).then(|| {
        // SAFETY: on success the path is a wide string ending with a zero,
        // which the system keeps until it's freed.
        let wide = unsafe {
            let length = (0..).take_while(|&i| *path.add(i) != 0).count();
            std::slice::from_raw_parts(path, length)
        };
        PathBuf::from(OsString::from_wide(wide))
    });
    // SAFETY: the system allocated the path, which is freed whether the call
    // succeeded or not; freeing null does nothing.
    unsafe { CoTaskMemFree(path.cast()) };
    documents
}

#[cfg(not(windows))]
fn documents() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use musicable_model::TrackKind;
    use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings};

    use super::*;

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("musicable-presets-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn surge() -> DeviceKind {
        DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.surge-synth-team.surge-xt".to_owned(),
            name: "Surge XT".to_owned(),
            instrument: true,
            settings: PluginSettings {
                state: Some(PluginState::new(vec![1, 2, 3, 255])),
                params: vec![PluginParam { id: 7, value: 0.25 }],
            },
        })
    }

    /// A VST3 preset file for `class_id`, with a component state, as VST3
    /// lays them out.
    fn vst3_file(class_id: &str) -> Vec<u8> {
        let mut file = b"VST3".to_vec();
        file.extend_from_slice(&1i32.to_le_bytes());
        file.extend_from_slice(class_id.as_bytes());
        file.extend_from_slice(&52i64.to_le_bytes());
        file.extend_from_slice(b"gain");
        file.extend_from_slice(b"List");
        file.extend_from_slice(&1i32.to_le_bytes());
        file.extend_from_slice(b"Comp");
        file.extend_from_slice(&48i64.to_le_bytes());
        file.extend_from_slice(&4i64.to_le_bytes());
        file
    }

    #[test]
    fn musicable_presets_keep_the_device_as_it_was() {
        let folder = folder("keep");
        let path = path_for(&folder, "Surge XT", "Big: Pad?");
        assert_eq!(path, folder.join("Surge XT/Big_ Pad_.musicable-preset"));
        save(&path, &surge()).unwrap();
        assert_eq!(load(&path).unwrap(), Loaded::Device(surge()));
        let gain = path_for(&folder, "Test Gain", "Quiet");
        save(&gain, &DeviceKind::TestGain { gain: 0.5 }).unwrap();
        assert_eq!(
            load(&gain).unwrap(),
            Loaded::Device(DeviceKind::TestGain { gain: 0.5 })
        );
        let presets = find(std::slice::from_ref(&folder), &|| true).unwrap();
        assert_eq!(
            presets,
            [
                Preset {
                    path: path.clone(),
                    name: "Big_ Pad_".to_owned(),
                    kind: PresetKind::Plugin {
                        format: PluginFormat::Clap,
                        id: "org.surge-synth-team.surge-xt".to_owned(),
                        name: "Surge XT".to_owned(),
                    },
                },
                Preset {
                    path: gain,
                    name: "Quiet".to_owned(),
                    kind: PresetKind::Device {
                        name: "Test Gain".to_owned(),
                    },
                },
            ]
        );
        assert_eq!(
            presets[0].plugin(),
            Some((PluginFormat::Clap, "org.surge-synth-team.surge-xt"))
        );
        assert_eq!(presets[1].plugin(), None);
        // A folder given twice, and one that isn't there.
        let twice = [
            folder.clone(),
            folder.join("Surge XT"),
            folder.join("Missing"),
        ];
        assert_eq!(find(&twice, &|| true).unwrap(), presets);
        assert_eq!(find(&twice, &|| false), None);
    }

    #[test]
    fn vst3_preset_files_are_found_by_their_class_id() {
        let folder = folder("vst3");
        let class_id = "0123456789abcdef0123456789ABCDEF";
        let file = vst3_file(class_id);
        let path = folder.join("Company/Plugin/Warm.vstpreset");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &file).unwrap();
        fs::write(folder.join("Company/Broken.vstpreset"), &file[..40]).unwrap();
        fs::write(
            folder.join("Company/Other.vstpreset"),
            b"not a preset at all, but long enough to have a header",
        )
        .unwrap();
        fs::write(folder.join("Company/notes.txt"), b"not a preset").unwrap();
        let upper = class_id.to_ascii_uppercase();
        assert_eq!(
            find(std::slice::from_ref(&folder), &|| true).unwrap(),
            [Preset {
                path: path.clone(),
                name: "Warm".to_owned(),
                kind: PresetKind::Vst3 {
                    class_id: upper.clone()
                },
            }]
        );
        assert_eq!(
            load(&path).unwrap(),
            Loaded::Vst3 {
                class_id: upper,
                state: PluginState::new(file),
            }
        );
        assert!(matches!(
            load(&folder.join("Company/Broken.vstpreset")),
            Err(PresetError::NotAPreset)
        ));
    }

    #[test]
    fn projects_that_hold_more_than_a_device_arent_presets() {
        let folder = folder("projects");
        let mut project = Project::new();
        project.add_track("Bass", TrackKind::Midi);
        let device = project.new_device(DeviceKind::TestGain { gain: 1.0 });
        project.master.devices.push(device);
        let path = folder.join("Song.musicable-preset");
        project::save(&path, &project).unwrap();
        assert!(matches!(load(&path), Err(PresetError::NotAPreset)));
        assert_eq!(find(&[folder], &|| true).unwrap(), []);
        assert!(matches!(
            load(Path::new("/nowhere/Missing.musicable-preset")),
            Err(PresetError::Load(_))
        ));
    }

    #[test]
    fn names_become_file_names_every_system_takes() {
        assert_eq!(file_name("Pad / Lead", "Preset"), "Pad _ Lead");
        assert_eq!(file_name(" .Hidden. ", "Preset"), "Hidden");
        assert_eq!(file_name("...", "Preset"), "Preset");
        assert_eq!(file_name("Tab\there", "Preset"), "Tab_here");
    }

    #[test]
    fn vst3_folders_are_where_the_system_says() {
        let folders = vst3_folders();
        assert!(folders.iter().all(|folder| folder.is_absolute()));
        assert!(folders.iter().all(|folder| folder.ends_with("VST3 Presets")
            || folder.ends_with("Presets")
            || folder.ends_with("presets")));
    }
}
