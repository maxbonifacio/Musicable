//! The Beat This! network (Foscarin, Schlüter and Widmer, 2024), which
//! finds beats and downbeats in music of any style: the beat-this crate's
//! port, on the pure-Rust rten runtime, on threads of its own.
//!
//! Its two model files aren't in the repository, as the checkpoints they
//! come from carry no licence yet: `scripts/fetch-models.sh` fetches them,
//! checked against pinned hashes, into the `models` folder.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use beat_this::{BeatStream, BeatThis, Tensor};
use rten::{NodeId, RunOptions, ThreadPool, ValueView};
use rten_tensor::Layout;

/// The model files, in the models folder: the log-mel spectrogram it
/// listens to, and the network itself.
pub const MEL_FILE: &str = "mel_spectrogram.onnx";
pub const BEAT_FILE: &str = "beat_this.onnx";
/// The environment variable that names the models folder, if it isn't
/// where [`models_folder`] would look.
pub const MODELS_VARIABLE: &str = "MUSICABLE_MODELS";

/// How many folders up from the program [`models_folder`] looks.
const LEVELS_UP: usize = 3;

/// The folder holding both model files: the one `MUSICABLE_MODELS` names,
/// or else a `models` folder beside the program or up to three folders
/// above it, where a build in the repository's `target` folder finds the
/// ones the fetch script fetched. `None` if none of them has both.
pub fn models_folder() -> Option<PathBuf> {
    let holds = |folder: &Path| folder.join(MEL_FILE).is_file() && folder.join(BEAT_FILE).is_file();
    if let Some(folder) = std::env::var_os(MODELS_VARIABLE).filter(|v| !v.is_empty()) {
        let folder = PathBuf::from(folder);
        return holds(&folder).then_some(folder);
    }
    let program = std::env::current_exe().ok()?;
    (program.ancestors().skip(1).take(LEVELS_UP + 1))
        .map(|folder| folder.join("models"))
        .find(|folder| holds(folder))
}

/// The network, loaded and ready to listen.
pub struct Network {
    tracker: BeatThis<Model>,
}

impl Network {
    /// Loads the model files from `folder`, to run on `threads` threads of
    /// its own, each of which runs `start` first, as to lower its priority.
    /// Once `stop` is set, as when the app closes, each run of a model fails
    /// before it starts, so a long sound's analysis stops soon.
    pub fn load(
        folder: &Path,
        threads: usize,
        start: impl Fn() + Sync,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, NetworkError> {
        let pool = Arc::new(ThreadPool::with_num_threads(threads.max(1)));
        pool.run(|| rayon::broadcast(|_| start()));
        let mel = Model::load(&folder.join(MEL_FILE), pool.clone(), stop.clone())?;
        let beat = Model::load(&folder.join(BEAT_FILE), pool, stop)?;
        Ok(Self {
            tracker: BeatThis::from_models(mel, beat),
        })
    }

    /// Starts listening to mono audio at `sample_rate`, which comes in
    /// chunks of any size.
    pub(crate) fn listen(
        &mut self,
        sample_rate: u32,
    ) -> Result<BeatStream<'_, Model>, NetworkError> {
        Ok(self.tracker.stream(sample_rate)?)
    }
}

impl fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Network").finish_non_exhaustive()
    }
}

/// What went wrong loading or running the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkError(pub String);

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NetworkError {}

impl From<anyhow::Error> for NetworkError {
    fn from(error: anyhow::Error) -> Self {
        Self(format!("{error:#}"))
    }
}

/// One of the network's models, run on the network's threads. It is
/// beat-this's own rten model, but for those threads: its own runs on
/// rten's, at normal priority, on every core.
pub(crate) struct Model {
    model: rten::Model,
    inputs: HashMap<String, NodeId>,
    outputs: Vec<(NodeId, String)>,
    pool: Arc<ThreadPool>,
    stop: Arc<AtomicBool>,
}

impl Model {
    fn load(
        path: &Path,
        pool: Arc<ThreadPool>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, NetworkError> {
        let model = rten::Model::load_file(path)
            .map_err(|error| NetworkError(format!("{}: {error}", path.display())))?;
        let name = |id: NodeId| Some(model.node_info(id)?.name()?.to_owned());
        let inputs = (model.input_ids().iter())
            .filter_map(|&id| Some((name(id)?, id)))
            .collect();
        let outputs = (model.output_ids().iter())
            .map(|&id| (id, name(id).unwrap_or_else(|| format!("output {id:?}"))))
            .collect();
        Ok(Self {
            model,
            inputs,
            outputs,
            pool,
            stop,
        })
    }
}

impl beat_this::Model for Model {
    fn run(&mut self, inputs: &[(&str, &Tensor)]) -> anyhow::Result<HashMap<String, Tensor>> {
        anyhow::ensure!(!self.stop.load(Relaxed), "stopped");
        let inputs = (inputs.iter())
            .map(|(name, tensor)| {
                let id = (self.inputs.get(*name))
                    .ok_or_else(|| anyhow::anyhow!("the model has no input {name}"))?;
                let view = ValueView::from_shape(tensor.shape.as_slice(), tensor.data.as_slice())?;
                Ok((*id, view.into()))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let ids: Vec<NodeId> = self.outputs.iter().map(|(id, _)| *id).collect();
        let options = RunOptions::default().with_thread_pool(Some(self.pool.clone()));
        let values = self.model.run(inputs, &ids, Some(options))?;
        (self.outputs.iter().zip(values))
            .map(|((_, name), value)| {
                let tensor = (value.into_tensor::<f32>())
                    .ok_or_else(|| anyhow::anyhow!("the model's {name} isn't floats"))?;
                let shape = tensor.shape().to_vec();
                Ok((
                    name.clone(),
                    Tensor {
                        shape,
                        data: tensor.into_data(),
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The models' folder, if they're there. In CI, they must be: the
    /// workflow fetches them. Elsewhere a test that needs them skips,
    /// saying so.
    pub(crate) fn models() -> Option<PathBuf> {
        let folder = models_folder();
        if folder.is_none() {
            assert!(
                std::env::var_os("CI").is_none(),
                "the Beat This! models are missing: run scripts/fetch-models.sh"
            );
            eprintln!("skipped: no Beat This! models (scripts/fetch-models.sh fetches them)");
        }
        folder
    }

    /// The network, if the models are there, as [`models`] says.
    pub(crate) fn network() -> Option<Network> {
        let folder = models()?;
        Some(Network::load(&folder, 2, || {}, Arc::default()).expect("the models load"))
    }

    #[test]
    fn a_folder_without_the_models_is_no_models_folder() {
        let error =
            Network::load(Path::new("no such folder"), 1, || {}, Arc::default()).unwrap_err();
        assert!(error.0.contains(MEL_FILE), "{error}");
    }
}
