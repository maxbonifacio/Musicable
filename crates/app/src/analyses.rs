//! Analysis of the open project's audio files, for warping. Each file the
//! clips play is analysed once, in the background on the library's queue,
//! and what analysis finds is kept in the analysis cache, so a file is
//! analysed only the first time any project uses it. What it finds goes to
//! the editor, by the path the clips give, as the files' headers do.
//!
//! The files are analysed one at a time, each a job that can wait, which
//! adds the next when it's done, so the queue's other jobs go between them.
//! The Beat This! network loads the first time a file needs it, and runs on
//! threads of its own, as many as the queue has, at the queue's priority.
//! Fast analysis leaves it out. `docs/analysis.md` describes it.

use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use musicable_analysis::cache::Missing;
use musicable_analysis::{Analysis, AnalysisError, Cache, File, Found, Network, analyse};
use musicable_commands::Editor;
use musicable_library::{Adder, Priority, Queue, lower_priority};
use musicable_model::Project;
use musicable_model::clip::ClipContent;
use musicable_plugins::Waker;

use crate::audio_files::locate;

/// The analyses of the open project's audio files.
pub(crate) struct Analyses {
    /// What the jobs share: `None` while analysis is off, as it is until
    /// the app starts it, and in tests that don't.
    shared: Option<Arc<Shared>>,
    /// Keeps the queue until the jobs have been told to stop, so closing
    /// doesn't wait for a long file's analysis.
    _queue: Option<Arc<Queue>>,
    done: Receiver<Done>,
    /// Counts the projects opened: what's found for an older one is
    /// dropped.
    project: u64,
    /// The files asked for, as the clips name them, so each is analysed
    /// once.
    asked: HashSet<String>,
    /// Those still being analysed.
    analysing: BTreeSet<String>,
}

/// What the jobs share.
struct Shared {
    cache: Option<Cache>,
    /// The folder of the network's model files, if they're there.
    models: Option<PathBuf>,
    /// The network, loaded by the first job that needs it: `None` if it
    /// didn't load.
    network: OnceLock<Option<Mutex<Network>>>,
    /// How many threads the network runs on.
    threads: usize,
    /// Whether analysis leaves the network out, as Fast analysis does.
    fast: AtomicBool,
    /// Set when the app closes, which stops the analysis under way.
    stop: Arc<AtomicBool>,
    waiting: Mutex<Waiting>,
    adder: Adder,
    sent: Sender<Done>,
    waker: Option<Waker>,
}

/// The files waiting to be analysed, first first.
#[derive(Default)]
struct Waiting {
    files: VecDeque<Wanted>,
    /// Whether a job is on the queue to analyse them.
    running: bool,
}

struct Wanted {
    project: u64,
    /// The file as the clips name it, and where it is.
    name: String,
    path: PathBuf,
}

/// What a job sends back.
enum Done {
    Analysed {
        project: u64,
        name: String,
        analysis: Arc<Analysis>,
    },
    Failed {
        project: u64,
        name: String,
    },
    /// Something the user should hear about once, such as the network not
    /// loading.
    Problem(String),
}

impl Analyses {
    /// Analyses that are off: [`find`](Self::find) finds nothing to
    /// analyse.
    pub(crate) fn off() -> Self {
        Self {
            shared: None,
            _queue: None,
            done: mpsc::channel().1,
            project: 0,
            asked: HashSet::new(),
            analysing: BTreeSet::new(),
        }
    }

    /// Analyses on `queue`, kept in `cache` if there's a folder for it,
    /// with the network whose model files are in `models`, if they're
    /// there. `waker` wakes the window when a file's analysed.
    pub(crate) fn new(
        queue: Arc<Queue>,
        cache: Option<PathBuf>,
        models: Option<PathBuf>,
        waker: Option<Waker>,
    ) -> Self {
        let (sent, done) = mpsc::channel();
        let shared = Shared {
            cache: cache.map(Cache::new),
            models,
            network: OnceLock::new(),
            threads: queue.threads(),
            fast: AtomicBool::new(false),
            stop: Arc::default(),
            waiting: Mutex::default(),
            adder: queue.adder(),
            sent,
            waker,
        };
        Self {
            shared: Some(Arc::new(shared)),
            _queue: Some(queue),
            done,
            project: 0,
            asked: HashSet::new(),
            analysing: BTreeSet::new(),
        }
    }

    /// Whether analysis leaves the Beat This! network out from now on,
    /// finding beats with the classic tracker alone, as Fast analysis does
    /// on slower computers. What the cache has is good for either.
    pub(crate) fn set_fast(&mut self, fast: bool) {
        if let Some(shared) = &self.shared {
            shared.fast.store(fast, Relaxed);
        }
    }

    /// Forgets the files of the project that was open, for another one.
    pub(crate) fn forget(&mut self) {
        self.project += 1;
        self.asked.clear();
        self.analysing.clear();
        if let Some(shared) = &self.shared {
            shared.waiting().files.clear();
        }
    }

    /// Asks for the analyses of `project`'s audio files that haven't been
    /// asked for yet. Relative paths start from `folder`, the folder of the
    /// project's file; without one they lead nowhere.
    pub(crate) fn find(&mut self, project: &Project, folder: Option<&Path>) {
        let Some(shared) = &self.shared else {
            return;
        };
        let mut wanted = Vec::new();
        for clip in project.tracks.iter().flat_map(|track| &track.clips) {
            let ClipContent::Audio(audio) = &clip.content else {
                continue;
            };
            if !self.asked.insert(audio.file.clone()) {
                continue;
            }
            let Some(path) = locate(&audio.file, folder) else {
                continue;
            };
            self.analysing.insert(audio.file.clone());
            wanted.push(Wanted {
                project: self.project,
                name: audio.file.clone(),
                path,
            });
        }
        if !wanted.is_empty() {
            shared.ask(wanted);
        }
    }

    /// Takes in what's been analysed into `editor`, and the problems into
    /// `messages`. Returns whether there was anything.
    pub(crate) fn receive(&mut self, editor: &mut Editor, messages: &mut Vec<String>) -> bool {
        let mut any = false;
        while let Ok(done) = self.done.try_recv() {
            match done {
                Done::Analysed {
                    project,
                    name,
                    analysis,
                } if project == self.project => {
                    self.analysing.remove(&name);
                    editor.analyses.insert(name, analysis);
                }
                Done::Failed { project, name } if project == self.project => {
                    self.analysing.remove(&name);
                }
                Done::Problem(problem) => messages.push(problem),
                _ => continue,
            }
            any = true;
        }
        any
    }

    /// Whether the file the clips call `file` is still to be analysed: it's
    /// being analysed, or hasn't been asked for yet. A file that can't be
    /// found, or analysed, isn't, and nothing is while analysis is off.
    pub(crate) fn waits_for(&self, file: &str) -> bool {
        self.shared.is_some() && (self.analysing.contains(file) || !self.asked.contains(file))
    }

    /// The audio files being analysed, by their names in the project.
    pub(crate) fn analysing(&self) -> Vec<String> {
        self.analysing.iter().cloned().collect()
    }

    /// What the status bar says while files are being analysed.
    pub(crate) fn status(&self) -> Option<String> {
        match self.analysing.len() {
            0 => None,
            1 => {
                let name = self.analysing.first()?;
                let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
                Some(format!("Analysing {name}"))
            }
            n => Some(format!("Analysing {n} audio files")),
        }
    }

    /// Waits until every file asked for is analysed, taking them into
    /// `editor`. For tests. Analysis runs at a lower priority than they do,
    /// so while other tests keep every core busy it can wait minutes for
    /// one: the five minutes it has are only so that a hang fails.
    #[cfg(test)]
    pub(crate) fn wait(&mut self, editor: &mut Editor) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
        let mut messages = Vec::new();
        while !self.analysing.is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the files weren't analysed"
            );
            self.receive(editor, &mut messages);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(messages.is_empty(), "{messages:?}");
    }
}

impl Drop for Analyses {
    fn drop(&mut self) {
        // Before the queue goes, which waits for the jobs running.
        if let Some(shared) = &self.shared {
            shared.stop.store(true, Relaxed);
        }
    }
}

impl Shared {
    fn waiting(&self) -> MutexGuard<'_, Waiting> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `wanted` to the files waiting, and a job to analyse them if
    /// none is on the queue.
    fn ask(self: &Arc<Self>, wanted: Vec<Wanted>) {
        let mut waiting = self.waiting();
        waiting.files.extend(wanted);
        if !waiting.running {
            waiting.running = true;
            let shared = self.clone();
            self.adder
                .add(Priority::Later, move || shared.analyse_next());
        }
    }

    /// The job: analyses the first file waiting, then adds itself again
    /// for the next.
    fn analyse_next(self: Arc<Self>) {
        let Some(wanted) = self.waiting().files.pop_front() else {
            self.waiting().running = false;
            return;
        };
        let pace = self.adder.pace();
        let stop = self.stop.clone();
        let mut step = || {
            pace.step();
            !stop.load(Relaxed)
        };
        let done = match self.analyse(&wanted.path, &mut step) {
            Ok(analysis) => Done::Analysed {
                project: wanted.project,
                name: wanted.name,
                analysis: Arc::new(analysis),
            },
            Err(AnalysisError::Stopped) => return,
            Err(error) => {
                // The arrangement already says when a file can't be read.
                log::warn!("couldn't analyse {}: {error}", wanted.path.display());
                Done::Failed {
                    project: wanted.project,
                    name: wanted.name,
                }
            }
        };
        self.send(done);
        let next = self.clone();
        self.adder.add(Priority::Later, move || next.analyse_next());
    }

    /// What the cache has for the file at `path`, or else its analysis,
    /// which the cache then keeps.
    fn analyse(
        &self,
        path: &Path,
        step: &mut dyn FnMut() -> bool,
    ) -> Result<Analysis, AnalysisError> {
        let fast = self.fast.load(Relaxed);
        let network = !fast && self.models.is_some() && !matches!(self.network.get(), Some(None));
        let missing: Option<Missing> = match &self.cache {
            Some(cache) => match cache.find(path, network, step)? {
                Found::Kept(analysis) => return Ok(*analysis),
                Found::Missing(missing) => Some(missing),
            },
            None => None,
        };
        let mut file = File::open(path)?;
        // Fast analysis doesn't load the network at all.
        let network = match fast {
            true => None,
            false => self.network(),
        };
        let (analysis, failed) = match network {
            Some(network) => {
                let mut network = network.lock().unwrap_or_else(PoisonError::into_inner);
                analyse(&mut file, Some(&mut network), step)?
            }
            None => analyse(&mut file, None, step)?,
        };
        if let Some(error) = failed {
            log::warn!("the beat tracker failed on {}: {error}", path.display());
        }
        if let (Some(cache), Some(missing)) = (&self.cache, missing)
            && let Err(error) = cache.keep(path, missing, &analysis)
        {
            log::warn!("couldn't keep the analysis of {}: {error}", path.display());
        }
        Ok(analysis)
    }

    /// The network, loading it the first time.
    fn network(&self) -> Option<&Mutex<Network>> {
        let models = self.models.as_deref()?;
        let network = self.network.get_or_init(|| {
            let stop = self.stop.clone();
            match Network::load(models, self.threads, lower_priority, stop) {
                Ok(network) => Some(Mutex::new(network)),
                Err(error) => {
                    self.send(Done::Problem(format!(
                        "The beat tracker didn't load, so analysis finds beats without it: {error}"
                    )));
                    None
                }
            }
        });
        network.as_ref()
    }

    fn send(&self, done: Done) {
        let _ = self.sent.send(done);
        if let Some(waker) = &self.waker {
            waker();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use musicable_io::audio::{SampleFormat, save_wav};
    use musicable_model::TrackKind;
    use musicable_model::clip::AudioClip;
    use musicable_model::time::{BeatRange, Beats};

    use super::*;

    /// Clicks on `beats` beats at `bpm`, starting on the first.
    fn clicks(bpm: f64, beats: usize) -> Vec<f32> {
        let period = (60.0 / bpm * 44_100.0) as usize;
        let mut samples = vec![0.0; period * beats];
        for beat in 0..beats {
            for (i, sample) in samples[beat * period..].iter_mut().take(400).enumerate() {
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                *sample = 0.8 * (-(i as f32) / 60.0).exp() * sign;
            }
        }
        samples
    }

    /// A project with a clip of the file that clips call `file`.
    fn project_playing(file: &str) -> Project {
        let mut project = Project::new();
        let track = project.add_track("Audio", TrackKind::Audio);
        let range = BeatRange::new(Beats::ZERO, Beats::new(16));
        let clip = project.new_clip("Clicks", range, ClipContent::Audio(AudioClip::new(file)));
        project.track_mut(track).unwrap().clips.push(clip);
        project
    }

    #[test]
    fn the_projects_files_are_analysed_once_and_kept() {
        let folder =
            std::env::temp_dir().join(format!("musicable-analyses-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("Clicks.wav");
        save_wav(&path, 44_100, SampleFormat::Float32, &[&clicks(120.0, 16)]).unwrap();
        let project = project_playing("Clicks.wav");
        let cache = folder.join("cache");
        let mut editor = Editor::default();
        let queue = Arc::new(Queue::new(1));
        let mut analyses = Analyses::new(queue.clone(), Some(cache.clone()), None, None);
        analyses.find(&project, Some(&folder));
        assert_eq!(analyses.status().as_deref(), Some("Analysing Clicks.wav"));
        analyses.wait(&mut editor);
        assert_eq!(analyses.status(), None);
        let analysis = editor.analyses["Clicks.wav"].clone();
        assert!(analysis.pulse);
        let steady = analysis.steady.unwrap();
        assert!((steady.bpm - 120.0).abs() < 0.5, "{steady:?}");
        // Asked again, it isn't analysed again.
        analyses.find(&project, Some(&folder));
        assert_eq!(analyses.status(), None);

        // Another project, with the file moved, finds it in the cache.
        fs::rename(&path, folder.join("Moved.wav")).unwrap();
        analyses.forget();
        let mut editor = Editor::default();
        analyses.find(&project_playing("Moved.wav"), Some(&folder));
        analyses.wait(&mut editor);
        assert_eq!(editor.analyses["Moved.wav"], analysis);
        // A file that isn't there is left out.
        analyses.find(&project_playing("Gone.wav"), Some(&folder));
        analyses.wait(&mut editor);
        assert!(!editor.analyses.contains_key("Gone.wav"));
        drop(analyses);
        drop(queue);
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn the_network_analyses_and_closing_stops_it() {
        let Some(models) = musicable_analysis::models_folder() else {
            assert!(
                std::env::var_os("CI").is_none(),
                "the Beat This! models are missing"
            );
            return;
        };
        let folder = std::env::temp_dir().join(format!("musicable-network-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        save_wav(
            &folder.join("Short.wav"),
            44_100,
            SampleFormat::Int16,
            &[&clicks(120.0, 32)],
        )
        .unwrap();
        // Three minutes, which the network takes a while over.
        save_wav(
            &folder.join("Long.wav"),
            44_100,
            SampleFormat::Int16,
            &[&clicks(100.0, 300)],
        )
        .unwrap();
        let queue = Arc::new(Queue::new(2));
        let mut analyses = Analyses::new(queue.clone(), None, Some(models), None);
        let mut editor = Editor::default();
        analyses.find(&project_playing("Short.wav"), Some(&folder));
        analyses.wait(&mut editor);
        let analysis = &editor.analyses["Short.wav"];
        assert_eq!(analysis.tracker, musicable_analysis::Tracker::Network);
        assert!(analysis.pulse);
        // Closing stops the long one soon, rather than waiting for it.
        analyses.find(&project_playing("Long.wav"), Some(&folder));
        std::thread::sleep(std::time::Duration::from_millis(500));
        let closing = std::time::Instant::now();
        drop(analyses);
        drop(queue);
        let took = closing.elapsed();
        assert!(
            took < std::time::Duration::from_secs(3),
            "closing took {took:?}"
        );
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn analyses_that_are_off_find_nothing() {
        let mut analyses = Analyses::off();
        analyses.find(&project_playing("/gone.wav"), None);
        assert_eq!(analyses.status(), None);
    }
}
