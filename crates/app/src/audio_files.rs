//! The open project's audio files as the arrangement needs them: each file's
//! header, which the commands that place and trim its clips read, and its
//! waveform, which its clips draw. A thread of their own reads them, so a
//! long file never holds up the window.

use std::collections::{HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use musicable_commands::Editor;
use musicable_commands::editor::AudioFile;
use musicable_io::audio::{AudioError, Decoder};
use musicable_model::Project;
use musicable_model::clip::ClipContent;
use musicable_ui::{Peaks, PeaksBuilder, Waveforms};

/// How many frames the thread reads at a time.
const CHUNK: usize = 16_384;

/// Reads the headers and the waveforms of the audio files the open
/// project's clips play.
///
/// Files are found as the clips name them: relative to the project's
/// folder, or absolute. Headers come first, as they're quick to read and
/// commands need them; then the waveforms, a file at a time. A file that
/// can't be read is left out: the session reports it when it builds a graph.
pub(crate) struct AudioFiles {
    jobs: Sender<Job>,
    read: Receiver<Read>,
    /// Counts the projects opened: what's read for an older one is dropped.
    project: Arc<AtomicU64>,
    /// The files asked for, as the clips name them, so each is read once.
    asked: HashSet<String>,
    waveforms: Waveforms,
}

struct Job {
    project: u64,
    /// The file as the clips name it, and where it is.
    name: String,
    path: PathBuf,
    /// Whether the header still needs reading, or only the waveform.
    header: bool,
}

enum Read {
    Header {
        project: u64,
        name: String,
        file: AudioFile,
    },
    Waveform {
        project: u64,
        name: String,
        peaks: Arc<Peaks>,
    },
}

impl AudioFiles {
    /// Starts the thread that reads the files.
    pub(crate) fn new() -> Self {
        let (jobs, received) = mpsc::channel();
        let (sent, read) = mpsc::channel();
        let project = Arc::new(AtomicU64::new(0));
        let current = project.clone();
        thread::Builder::new()
            .name("musicable-audio-files".into())
            .spawn(move || read_files(&received, &sent, &current))
            .expect("the system starts a thread");
        Self {
            jobs,
            read,
            project,
            asked: HashSet::new(),
            waveforms: Waveforms::new(),
        }
    }

    /// The waveforms read so far, by the path the clips give.
    pub(crate) fn waveforms(&self) -> &Waveforms {
        &self.waveforms
    }

    /// Forgets the files of the project that was open, for another one.
    pub(crate) fn forget(&mut self) {
        self.project.fetch_add(1, Relaxed);
        self.asked.clear();
        self.waveforms.clear();
    }

    /// Asks for the files of `project`'s audio clips that haven't been asked
    /// for yet. Relative paths start from `folder`, the folder of the
    /// project's file; without one they lead nowhere.
    pub(crate) fn find(&mut self, project: &Project, folder: Option<&Path>) {
        for clip in project.tracks.iter().flat_map(|track| &track.clips) {
            let ClipContent::Audio(audio) = &clip.content else {
                continue;
            };
            if self.asked.contains(&audio.file) {
                continue;
            }
            self.asked.insert(audio.file.clone());
            let Some(path) = locate(&audio.file, folder) else {
                continue;
            };
            self.ask(audio.file.clone(), path, true);
        }
    }

    /// Reads the header of the file at `path` now, for clips that will name
    /// it `name`, and its waveform on the thread.
    pub(crate) fn read_now(&mut self, name: &str, path: &Path) -> Result<AudioFile, AudioError> {
        let file = header(&Decoder::open(path)?);
        if self.asked.insert(name.to_owned()) || !self.waveforms.contains_key(name) {
            self.ask(name.to_owned(), path.to_owned(), false);
        }
        Ok(file)
    }

    fn ask(&mut self, name: String, path: PathBuf, header: bool) {
        let job = Job {
            project: self.project.load(Relaxed),
            name,
            path,
            header,
        };
        // The thread only stops when this is dropped.
        let _ = self.jobs.send(job);
    }

    /// Takes in what the thread has read: headers into `editor`'s files,
    /// and waveforms. Returns whether there was anything.
    pub(crate) fn receive(&mut self, editor: &mut Editor) -> bool {
        let current = self.project.load(Relaxed);
        let mut any = false;
        while let Ok(read) = self.read.try_recv() {
            match read {
                Read::Header {
                    project,
                    name,
                    file,
                } if project == current => {
                    editor.files.insert(name, file);
                }
                Read::Waveform {
                    project,
                    name,
                    peaks,
                } if project == current => {
                    self.waveforms.insert(name, peaks);
                }
                _ => continue,
            }
            any = true;
        }
        any
    }

    /// Waits until the thread has read every file asked for. For tests.
    #[cfg(test)]
    pub(crate) fn wait(&mut self, editor: &mut Editor) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self
            .asked
            .iter()
            .any(|name| !self.waveforms.contains_key(name))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the files weren't read"
            );
            self.receive(editor);
            thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

/// Where the file a clip names `name` is: relative to `folder`, or absolute.
fn locate(name: &str, folder: Option<&Path>) -> Option<PathBuf> {
    let path = Path::new(name);
    match (path.is_absolute(), folder) {
        (true, _) => Some(path.to_owned()),
        (false, Some(folder)) => Some(folder.join(path)),
        (false, None) => None,
    }
}

/// What clips call the file at `path`: its path relative to `folder`, the
/// folder of the project's file, with `/` between names, if it's in there,
/// as the project format prefers; or else its absolute path.
pub(crate) fn clip_path(path: &Path, folder: Option<&Path>) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
    let relative = folder
        .and_then(|folder| std::path::absolute(folder).ok())
        .and_then(|folder| path.strip_prefix(folder).ok().map(Path::to_owned));
    match relative {
        Some(relative)
            if relative
                .components()
                .all(|c| matches!(c, Component::Normal(_))) =>
        {
            let names: Vec<_> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            names.join("/")
        }
        _ => path.to_string_lossy().into_owned(),
    }
}

fn header(decoder: &Decoder) -> AudioFile {
    let info = decoder.info();
    AudioFile {
        sample_rate: info.sample_rate,
        frames: info.frames,
        channels: info.channels,
    }
}

/// The thread: reads the headers asked for, then a waveform, then the
/// headers asked for meanwhile, and so on, until the [`AudioFiles`] goes.
fn read_files(jobs: &Receiver<Job>, read: &Sender<Read>, current: &AtomicU64) {
    let mut waveforms = VecDeque::new();
    loop {
        loop {
            let job = if waveforms.is_empty() {
                match jobs.recv() {
                    Ok(job) => job,
                    Err(_) => return,
                }
            } else {
                match jobs.try_recv() {
                    Ok(job) => job,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            };
            if job.project != current.load(Relaxed) {
                continue;
            }
            if job.header {
                let Ok(decoder) = Decoder::open(&job.path) else {
                    continue;
                };
                let header = Read::Header {
                    project: job.project,
                    name: job.name.clone(),
                    file: header(&decoder),
                };
                if read.send(header).is_err() {
                    return;
                }
            }
            waveforms.push_back(job);
        }
        let Some(job) = waveforms.pop_front() else {
            continue;
        };
        let Some(peaks) = waveform(&job.path, || job.project == current.load(Relaxed)) else {
            continue;
        };
        let waveform = Read::Waveform {
            project: job.project,
            name: job.name,
            peaks: Arc::new(peaks),
        };
        if read.send(waveform).is_err() {
            return;
        }
    }
}

/// The waveform of the file at `path`, or `None` if it can't be read, or
/// if `wanted` says it isn't wanted any more.
fn waveform(path: &Path, wanted: impl Fn() -> bool) -> Option<Peaks> {
    let mut decoder = Decoder::open(path).ok()?;
    let info = decoder.info();
    let mut peaks = PeaksBuilder::new(info.sample_rate, info.channels);
    let mut buffer = vec![0.0; CHUNK * info.channels];
    let mut at = 0;
    while at < info.frames {
        if !wanted() {
            return None;
        }
        let frames = decoder.read(at, &mut buffer).ok()?;
        if frames == 0 {
            break;
        }
        peaks.push(&buffer[..frames * info.channels]);
        at += frames as u64;
    }
    Some(peaks.finish())
}

#[cfg(test)]
mod tests {
    use musicable_io::audio::{SampleFormat, save_wav};
    use musicable_model::TrackKind;
    use musicable_model::clip::AudioClip;
    use musicable_model::time::{BeatRange, Beats};

    use super::*;

    #[test]
    fn clips_name_files_in_the_projects_folder_relative_to_it() {
        // Absolute on every system: on Windows, "/music" is on whichever
        // drive is current.
        let music = std::env::temp_dir().join("music");
        let folder = music.join("song");
        let name = |path: &Path| clip_path(path, Some(&folder));
        let kick = folder.join("kick.wav");
        let snare = folder.join("Samples").join("snare.wav");
        let hat = music.join("other").join("hat.wav");
        let absolute = |path: &Path| path.to_string_lossy().into_owned();
        assert_eq!(name(&kick), "kick.wav");
        assert_eq!(name(&snare), "Samples/snare.wav");
        assert_eq!(name(&hat), absolute(&hat));
        assert_eq!(clip_path(&kick, None), absolute(&kick));
        assert_eq!(locate("kick.wav", Some(&folder)), Some(kick));
        assert_eq!(locate(&absolute(&hat), None), Some(hat));
        assert_eq!(locate("kick.wav", None), None);
    }

    #[test]
    fn the_projects_files_are_read_on_the_thread() {
        let folder = std::env::temp_dir().join(format!("musicable-files-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let samples: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 / 100.0).sin() * 0.5)
            .collect();
        let path = folder.join("tone.wav");
        save_wav(&path, 48_000, SampleFormat::Float32, &[&samples]).unwrap();
        let mut project = Project::new();
        let track = project.add_track("Audio", TrackKind::Audio);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        for (name, file) in [("Tone", "tone.wav"), ("Gone", "gone.wav")] {
            let content = ClipContent::Audio(AudioClip::new(file));
            let clip = project.new_clip(name, range, content);
            project.track_mut(track).unwrap().clips.push(clip);
        }

        let mut files = AudioFiles::new();
        let mut editor = Editor::default();
        files.find(&project, Some(&folder));
        files.asked.remove("gone.wav");
        files.wait(&mut editor);
        let tone = AudioFile {
            sample_rate: 48_000,
            frames: 48_000,
            channels: 1,
        };
        assert_eq!(editor.files.get("tone.wav"), Some(&tone));
        assert!(!editor.files.contains_key("gone.wav"));
        let peaks = &files.waveforms()["tone.wav"];
        assert_eq!(peaks.frames(), 48_000);
        let all = peaks.range(0.0, 48_000.0).unwrap();
        assert!(all.max > 0.49 && all.min < -0.49, "{all:?}");

        // Another project's files are read again.
        files.forget();
        assert!(files.waveforms().is_empty());
        files.find(&project, Some(&folder));
        files.asked.remove("gone.wav");
        files.wait(&mut editor);
        assert!(files.waveforms().contains_key("tone.wav"));
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
