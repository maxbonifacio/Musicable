//! The cache of analyses, so each sound is analysed once. One folder holds
//! them all, and nothing goes beside the audio files: an analysis is found
//! by its file's path, size and modification time, or else by a hash of the
//! file's sound as analysis hears it, so a file that's moved, copied or
//! saved again with the same sound keeps its analysis.
//!
//! In the folder, `files` has a small entry for each file analysed, named
//! by a hash of its path, which says which sound the file had; `sounds` has
//! each sound's analysis as JSON, named by its hash. Each is written to a
//! file of its own and renamed into place, so a reader never sees half of
//! one, and two programs writing one entry leave it whole.

use std::fs;
use std::hash::Hasher;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::UNIX_EPOCH;

use musicable_io::audio::AudioError;
use serde::{Deserialize, Serialize};
use siphasher::sip::SipHasher13;

use crate::audio::{Audio, CHUNK, File};
use crate::{Analysis, AnalysisError, Tracker, VERSION};

/// The folders in the cache's folder.
const FILES: &str = "files";
const SOUNDS: &str = "sounds";

/// A sound as analysis hears it, mixed to one channel: a hash of its
/// samples, its sample rate and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sound(u64);

impl Sound {
    /// The sound of `audio`, read from start to end, calling `step` between
    /// the pieces, which says whether to go on.
    pub fn of(
        audio: &mut dyn Audio,
        step: &mut dyn FnMut() -> bool,
    ) -> Result<Self, AnalysisError> {
        let mut hasher = SipHasher13::new();
        hasher.write_u32(audio.sample_rate());
        hasher.write_u64(audio.frames());
        let mut chunk = vec![0.0; CHUNK];
        let mut at = 0;
        while at < audio.frames() {
            let read = audio.read(at, &mut chunk)?;
            if read == 0 {
                break;
            }
            for sample in &chunk[..read] {
                hasher.write_u32(sample.to_bits());
            }
            at += read as u64;
            if !step() {
                return Err(AnalysisError::Stopped);
            }
        }
        Ok(Self(hasher.finish()))
    }

    fn name(self) -> String {
        format!("{:016x}", self.0)
    }

    fn from_name(name: &str) -> Option<Self> {
        (name.len() == 16).then(|| u64::from_str_radix(name, 16).ok().map(Self))?
    }
}

/// A file as it was when it was looked for: its size, and when it last
/// changed, in milliseconds since 1970 began.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
}

impl Stamp {
    /// The file at `path` as it is now.
    pub fn of(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        let modified = (metadata.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_millis() as u64);
        Ok(Self {
            size: metadata.len(),
            modified,
        })
    }
}

/// What the cache knows of a file it was asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    /// Its analysis, kept for the file as it is, or for its sound.
    Kept(Box<Analysis>),
    /// No analysis as good as one now would be: what to keep the file's
    /// under once it's analysed.
    Missing(Missing),
}

/// A file the cache has no analysis for yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Missing {
    pub sound: Sound,
    pub stamp: Stamp,
}

/// A file's entry in `files`.
#[derive(Serialize, Deserialize)]
struct Entry {
    path: String,
    #[serde(flatten)]
    stamp: Stamp,
    sound: String,
}

/// The cache, in a folder of its own.
#[derive(Debug)]
pub struct Cache {
    folder: PathBuf,
}

impl Cache {
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// What the cache has for the audio file at `path`: an analysis as good
    /// as one now would be, by this version of analysis and, where
    /// `network` says the network is there, by the network. Without one by
    /// the file's path as the file is now, it reads the file to find its
    /// sound, calling `step` as it goes.
    pub fn find(
        &self,
        path: &Path,
        network: bool,
        step: &mut dyn FnMut() -> bool,
    ) -> Result<Found, AnalysisError> {
        let stamp = Stamp::of(path).map_err(AudioError::from)?;
        let good = |analysis: &Analysis| {
            analysis.version == VERSION && (analysis.tracker == Tracker::Network || !network)
        };
        // The file as it was: its sound, without reading it.
        let known = (self.entry(path))
            .filter(|entry| entry.stamp == stamp)
            .and_then(|entry| Sound::from_name(&entry.sound));
        let sound = match known {
            Some(sound) => sound,
            None => Sound::of(&mut File::open(path)?, step)?,
        };
        match self.analysis(sound).filter(good) {
            Some(analysis) => {
                if known.is_none() {
                    // Found by its sound, so next time it's found by its path.
                    let _ = self.write_entry(path, sound, stamp);
                }
                Ok(Found::Kept(Box::new(analysis)))
            }
            None => Ok(Found::Missing(Missing { sound, stamp })),
        }
    }

    /// Keeps `analysis` for the file at `path`, as [`find`](Self::find)
    /// found it missing.
    pub fn keep(&self, path: &Path, missing: Missing, analysis: &Analysis) -> io::Result<()> {
        let json = serde_json::to_vec(analysis).map_err(io::Error::other)?;
        let name = format!("{}.json", missing.sound.name());
        write_whole(&self.folder.join(SOUNDS).join(name), &json)?;
        self.write_entry(path, missing.sound, missing.stamp)
    }

    /// The analysis kept for `sound`, if one is.
    fn analysis(&self, sound: Sound) -> Option<Analysis> {
        let path = (self.folder.join(SOUNDS)).join(format!("{}.json", sound.name()));
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }

    /// The entry for the file at `path`, if it has one.
    fn entry(&self, path: &Path) -> Option<Entry> {
        let entry: Entry = serde_json::from_slice(&fs::read(self.entry_path(path)).ok()?).ok()?;
        (entry.path == path.to_string_lossy()).then_some(entry)
    }

    fn write_entry(&self, path: &Path, sound: Sound, stamp: Stamp) -> io::Result<()> {
        let entry = Entry {
            path: path.to_string_lossy().into_owned(),
            stamp,
            sound: sound.name(),
        };
        let json = serde_json::to_vec(&entry).map_err(io::Error::other)?;
        write_whole(&self.entry_path(path), &json)
    }

    /// Where the entry for the file at `path` goes: named by a hash of its
    /// path.
    fn entry_path(&self, path: &Path) -> PathBuf {
        let mut hasher = SipHasher13::new();
        hasher.write(path.to_string_lossy().as_bytes());
        (self.folder.join(FILES)).join(format!("{:016x}.json", hasher.finish()))
    }
}

/// Writes `bytes` to the file at `path` whole: to a file of its own beside
/// it, then renamed to `path`, replacing what's there.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(format!(
        ".{}-{}.part",
        std::process::id(),
        WRITES.fetch_add(1, Relaxed)
    ));
    fs::write(&part, bytes)?;
    fs::rename(&part, path).inspect_err(|_| {
        let _ = fs::remove_file(&part);
    })
}

#[cfg(test)]
mod tests {
    use musicable_io::audio::{SampleFormat, save_wav};

    use super::*;
    use crate::groove::{Groove, SAMPLE_RATE, synth};
    use crate::{Samples, analyse};

    /// A folder of its own for each test, removed when it ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("musicable-cache-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn not_kept(found: Found) -> Missing {
        match found {
            Found::Missing(missing) => missing,
            Found::Kept(_) => panic!("kept already"),
        }
    }

    #[test]
    fn a_file_is_found_by_its_path_and_once_moved_by_its_sound() {
        let dir = TempDir::new("found");
        let song = synth(8.0, &Groove::steady(120.0));
        let path = dir.0.join("Groove.wav");
        save_wav(&path, SAMPLE_RATE, SampleFormat::Float32, &[&song.audio]).unwrap();
        let cache = Cache::new(dir.0.join("cache"));
        let missing = not_kept(cache.find(&path, false, &mut || true).unwrap());
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let (analysis, _) = analyse(&mut audio, None, &mut || true).unwrap();
        cache.keep(&path, missing, &analysis).unwrap();
        // Found by its path, without reading the file.
        let mut steps = 0;
        let found = cache
            .find(&path, false, &mut || {
                steps += 1;
                true
            })
            .unwrap();
        assert_eq!(found, Found::Kept(Box::new(analysis.clone())));
        assert_eq!(steps, 0);
        // Moved, it's found by its sound, which reads it.
        let moved = dir.0.join("Moved.wav");
        fs::rename(&path, &moved).unwrap();
        let found = cache
            .find(&moved, false, &mut || {
                steps += 1;
                true
            })
            .unwrap();
        assert_eq!(found, Found::Kept(Box::new(analysis.clone())));
        assert!(steps > 0);
        // And then by its path again.
        steps = 0;
        cache
            .find(&moved, false, &mut || {
                steps += 1;
                true
            })
            .unwrap();
        assert_eq!(steps, 0);
        // A file with another sound isn't.
        let other: Vec<f32> = song.audio.iter().map(|s| s * 0.5).collect();
        save_wav(&moved, SAMPLE_RATE, SampleFormat::Float32, &[&other]).unwrap();
        let again = not_kept(cache.find(&moved, false, &mut || true).unwrap());
        assert_ne!(again.sound, missing.sound);
    }

    #[test]
    fn the_classic_trackers_analysis_is_redone_when_the_network_is_there() {
        let dir = TempDir::new("network");
        let song = synth(4.0, &Groove::steady(100.0));
        let path = dir.0.join("Loop.wav");
        save_wav(&path, SAMPLE_RATE, SampleFormat::Float32, &[&song.audio]).unwrap();
        let cache = Cache::new(dir.0.join("cache"));
        let missing = not_kept(cache.find(&path, false, &mut || true).unwrap());
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let (analysis, _) = analyse(&mut audio, None, &mut || true).unwrap();
        assert_eq!(analysis.tracker, Tracker::Classic);
        cache.keep(&path, missing, &analysis).unwrap();
        assert!(matches!(
            cache.find(&path, false, &mut || true),
            Ok(Found::Kept(_))
        ));
        // With the network, it's analysed again, and kept by the same sound.
        let again = not_kept(cache.find(&path, true, &mut || true).unwrap());
        assert_eq!(again, missing);
        // Nor is another version's analysis kept.
        let older = Analysis {
            version: VERSION - 1,
            ..analysis
        };
        cache.keep(&path, missing, &older).unwrap();
        assert!(matches!(
            cache.find(&path, false, &mut || true),
            Ok(Found::Missing(_))
        ));
    }

    #[test]
    fn a_damaged_entry_is_no_entry() {
        let dir = TempDir::new("damaged");
        let song = synth(3.0, &Groove::steady(90.0));
        let path = dir.0.join("Hit.wav");
        save_wav(&path, SAMPLE_RATE, SampleFormat::Float32, &[&song.audio]).unwrap();
        let cache = Cache::new(dir.0.join("cache"));
        let missing = not_kept(cache.find(&path, false, &mut || true).unwrap());
        let mut audio = Samples {
            samples: &song.audio,
            sample_rate: SAMPLE_RATE,
        };
        let (analysis, _) = analyse(&mut audio, None, &mut || true).unwrap();
        cache.keep(&path, missing, &analysis).unwrap();
        let sound = (cache.folder.join(SOUNDS)).join(format!("{}.json", missing.sound.name()));
        fs::write(&sound, b"{ half").unwrap();
        assert_eq!(
            cache.find(&path, false, &mut || true).unwrap(),
            Found::Missing(missing)
        );
        fs::write(cache.entry_path(&path), b"[").unwrap();
        assert_eq!(
            cache.find(&path, false, &mut || true).unwrap(),
            Found::Missing(missing)
        );
        // No file there is an error, as no file to analyse is.
        assert!(
            cache
                .find(&dir.0.join("Gone.wav"), false, &mut || true)
                .is_err()
        );
    }
}
