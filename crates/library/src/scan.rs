//! Scanning a place: finding the audio files in a folder and the folders
//! under it, and reading what their headers say.

use std::collections::{HashMap, HashSet};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use musicable_io::audio::Decoder;

use crate::index::{AudioFacts, Record, is_audio_file};

/// How many files a scan finds between reports of how far it got.
const REPORT_EVERY: usize = 256;

/// Scans `folder`, and the folders under it, for the files the library
/// indexes, and returns their records, in order of path. It goes through
/// the folders as [`walk`] does.
///
/// A file whose size and time of change are those its record in `old` has
/// keeps that record, tags, analysis and all, and isn't opened. Others have
/// their headers read. A file that moved keeps its tags: a file gone from
/// where `old` had it and one new to the scan with the same name and size
/// are taken for the same file.
///
/// The scan stops when `wanted` says no, returning `None`. It tells `found`
/// how many files it has found every so often.
pub fn scan(
    folder: &Path,
    old: &[Record],
    wanted: &dyn Fn() -> bool,
    found: &mut dyn FnMut(usize),
) -> io::Result<Option<Vec<Record>>> {
    let mut records = Vec::new();
    let mut fresh = Vec::new();
    let finished = walk(folder, wanted, &mut |relative, path, metadata| {
        if !is_audio_file(relative.rsplit('/').next().unwrap_or_default()) {
            return;
        }
        let (size, modified) = (metadata.len(), modified(metadata));
        let known = old
            .binary_search_by(|record| record.path.as_str().cmp(&relative))
            .ok()
            .map(|i| &old[i]);
        let record = match known {
            Some(record) if record.size == size && record.modified == modified => record.clone(),
            Some(record) => {
                // Changed: what's in it is read again, and the user's tags
                // stay.
                let mut changed = Record::new(relative, size, modified, header(&path));
                changed.tags = record.tags.clone();
                changed
            }
            None => {
                fresh.push(records.len());
                Record::new(relative, size, modified, header(&path))
            }
        };
        records.push(record);
        if records.len().is_multiple_of(REPORT_EVERY) {
            found(records.len());
        }
    })?;
    if !finished {
        return Ok(None);
    }
    follow_moves(old, &mut records, &fresh);
    records.sort_by(|a, b| a.path.cmp(&b.path));
    found(records.len());
    Ok(Some(records))
}

/// Goes through `folder` and the folders under it, and tells `file` of each
/// file there: its path from `folder`, with `/` between names, its full
/// path, and what the system says of it, links followed. Hidden files and
/// folders, whose names start with a dot, are left out, as are macOS's
/// `__MACOSX` folders and names that aren't Unicode. Folders that link back
/// to one the walk has been in are only gone through once.
///
/// The walk asks `wanted` between files, and stops when it says no,
/// returning `false`. Folders under `folder` that can't be read are
/// skipped; only `folder` itself not being readable is an error.
pub(crate) fn walk(
    folder: &Path,
    wanted: &dyn Fn() -> bool,
    file: &mut dyn FnMut(String, PathBuf, &Metadata),
) -> io::Result<bool> {
    let mut visited = HashSet::new();
    let mut folders: Vec<(String, PathBuf)> = vec![(String::new(), folder.to_owned())];
    let mut first = true;
    while let Some((prefix, dir)) = folders.pop() {
        if let Ok(real) = fs::canonicalize(&dir)
            && !visited.insert(real)
        {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if first => return Err(error),
            Err(_) => continue,
        };
        first = false;
        for entry in entries.flatten() {
            if !wanted() {
                return Ok(false);
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if name.starts_with('.') || name == "__MACOSX" {
                continue;
            }
            let path = entry.path();
            // Follows links, to what they link to.
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let relative = match prefix.as_str() {
                "" => name,
                prefix => format!("{prefix}/{name}"),
            };
            if metadata.is_dir() {
                folders.push((relative, path));
            } else if metadata.is_file() {
                file(relative, path, &metadata);
            }
        }
    }
    Ok(true)
}

/// Gives the files at `fresh` in `records`, which `old` didn't have, the
/// tags of the files `old` had that are gone, where their names and sizes
/// match. Those with the same time of change keep what analysis found too.
fn follow_moves(old: &[Record], records: &mut [Record], fresh: &[usize]) {
    if fresh.is_empty() {
        return;
    }
    let here: HashSet<&str> = records.iter().map(|r| r.path.as_str()).collect();
    let mut gone: HashMap<(&str, u64), &Record> = HashMap::new();
    for record in old {
        if !here.contains(record.path.as_str()) && !record.tags.is_empty() {
            gone.entry((record.name(), record.size)).or_insert(record);
        }
    }
    if gone.is_empty() {
        return;
    }
    let moved: Vec<(usize, Record)> = fresh
        .iter()
        .filter_map(|&i| {
            let record = &records[i];
            let was = gone.get(&(record.name(), record.size))?;
            Some((i, (*was).clone()))
        })
        .collect();
    for (i, was) in moved {
        let record = &mut records[i];
        record.tags = was.tags;
        if was.modified == record.modified {
            record.measured = was.measured;
            record.embedding = was.embedding;
            record.analyser = was.analyser;
        }
    }
}

/// When the file changed, in milliseconds since 1970 began, or 0 if the
/// system doesn't say.
fn modified(metadata: &Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_millis() as u64)
}

/// What the header of the file at `path` says, if it's audio Musicable
/// reads.
pub fn header(path: &Path) -> Option<AudioFacts> {
    Decoder::open(path)
        .ok()
        .map(|decoder| decoder.info().into())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use musicable_io::audio::{SampleFormat, save_wav};

    use super::*;
    use crate::index::{Format, Tag};

    /// A folder of its own for each test, empty.
    pub(crate) fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir()
            .join(format!("musicable-scan-{}", std::process::id()))
            .join(name);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn wav(path: &Path, frames: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let samples = vec![0.25; frames];
        save_wav(path, 44_100, SampleFormat::Int16, &[&samples]).unwrap();
    }

    fn paths(records: &[Record]) -> Vec<&str> {
        records.iter().map(|r| r.path.as_str()).collect()
    }

    fn all(folder: &Path, old: &[Record]) -> Vec<Record> {
        scan(folder, old, &|| true, &mut |_| {}).unwrap().unwrap()
    }

    #[test]
    fn scans_find_audio_files_in_folders_under_the_place() {
        let place = folder("finds");
        wav(&place.join("Kick.wav"), 441);
        wav(&place.join("Drums/Snare.WAV"), 100);
        wav(&place.join("Drums/Hats/Hat.aif"), 100);
        fs::write(place.join("Drums/notes.txt"), "not audio").unwrap();
        fs::write(place.join("Drums/fake.flac"), "not audio either").unwrap();
        wav(&place.join(".hidden/Secret.wav"), 100);
        wav(&place.join("__MACOSX/._Kick.wav"), 100);
        wav(&place.join("._Clap.wav"), 100);
        let records = all(&place, &[]);
        assert_eq!(
            paths(&records),
            [
                "Drums/Hats/Hat.aif",
                "Drums/Snare.WAV",
                "Drums/fake.flac",
                "Kick.wav"
            ]
        );
        let kick = records[3].audio.unwrap();
        assert_eq!(
            (kick.format, kick.sample_rate, kick.channels, kick.frames),
            (Format::Wav, 44_100, 1, 441)
        );
        // Named like audio, but not audio Musicable reads.
        assert_eq!(records[2].audio, None);
        // A file that's really a WAV, whatever its extension says.
        assert_eq!(records[0].audio.unwrap().format, Format::Wav);
        assert!(records.iter().all(|r| r.size > 0 && r.modified > 0));
        assert!(scan(&place.join("Missing"), &[], &|| true, &mut |_| {}).is_err());
    }

    #[test]
    fn rescans_keep_what_hasnt_changed_and_follow_moved_files() {
        let place = folder("rescans");
        wav(&place.join("Kick.wav"), 100);
        wav(&place.join("Snare.wav"), 200);
        wav(&place.join("Old/Hat.wav"), 300);
        let mut old = all(&place, &[]);
        for record in &mut old {
            record.tags.push(Tag::yours("mine"));
            record.analyser = Some("test".to_owned());
        }
        // A record the scan keeps as it is, without opening the file.
        old[0].audio = None;
        // The snare changed, and the hat moved.
        wav(&place.join("Snare.wav"), 250);
        fs::create_dir(place.join("New")).unwrap();
        fs::rename(place.join("Old/Hat.wav"), place.join("New/Hat.wav")).unwrap();
        let records = all(&place, &old);
        assert_eq!(paths(&records), ["Kick.wav", "New/Hat.wav", "Snare.wav"]);
        assert_eq!(records[0], old[0]);
        let hat = &records[1];
        assert_eq!(hat.tags, [Tag::yours("mine")]);
        assert_eq!(hat.analyser.as_deref(), Some("test"));
        let snare = &records[2];
        assert_eq!(snare.audio.unwrap().frames, 250);
        assert_eq!(snare.tags, [Tag::yours("mine")]);
        assert_eq!(snare.analyser, None);
    }

    #[test]
    fn scans_stop_when_not_wanted_and_say_how_far_they_got() {
        let place = folder("stops");
        for i in 0..600 {
            wav(&place.join(format!("{i:03}.wav")), 10);
        }
        let mut reports = Vec::new();
        let records = scan(&place, &[], &|| true, &mut |n| reports.push(n)).unwrap();
        assert_eq!(records.unwrap().len(), 600);
        assert_eq!(reports, [256, 512, 600]);
        let asked = Cell::new(0);
        let wanted = || {
            asked.set(asked.get() + 1);
            asked.get() < 10
        };
        assert_eq!(scan(&place, &[], &wanted, &mut |_| {}).unwrap(), None);
        assert_eq!(asked.get(), 10);
    }

    #[cfg(unix)]
    #[test]
    fn folders_that_link_back_are_scanned_once() {
        let place = folder("links");
        wav(&place.join("Drums/Kick.wav"), 10);
        std::os::unix::fs::symlink(&place, place.join("Drums/Back")).unwrap();
        std::os::unix::fs::symlink(place.join("Drums/Kick.wav"), place.join("Link.wav")).unwrap();
        let records = all(&place, &[]);
        assert_eq!(paths(&records), ["Drums/Kick.wav", "Link.wav"]);
    }
}
