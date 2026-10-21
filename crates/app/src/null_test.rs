//! The null test: a project played offline with the parallel scheduler and
//! with the reference renderer, side by side, checked sample for sample.
//! `musicable null-test` runs it on a project file, and
//! `tests/tier0_demo.rs` on the Tier 0 demo. `docs/app.md` describes it.

use std::time::{Duration, Instant};

use musicable_commands::{Keymap, transport};
use musicable_engine::{
    Engine, EngineConfig, ParallelMonitor, ParallelOptions, ParallelRenderer, ParallelStats, Stats,
};
use musicable_model::Project;

use crate::Session;

/// The app plays in stereo.
const CHANNELS: usize = 2;

/// Which renderer plays the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Render {
    /// The reference renderer: one node at a time, in order, on the calling
    /// thread. The oracle.
    Reference,
    /// The parallel scheduler as the app plays with it: see
    /// [`engine`](crate::engine).
    App,
    /// The parallel scheduler with other options.
    Parallel(ParallelOptions),
}

/// Plays a project offline, one buffer at a time, as the app plays it on an
/// audio device.
pub struct Player {
    engine: Engine,
    session: Session,
    monitor: Option<ParallelMonitor>,
    buffer: Vec<f32>,
    took: Duration,
    peak: f32,
}

impl Player {
    /// Opens `project` on an engine for `config` that renders with `render`,
    /// then presses Play: the engine gets what the app would send it.
    pub fn new(project: Project, render: &Render, config: EngineConfig) -> Self {
        let (engine, handle, monitor) = match render {
            Render::Reference => {
                let (engine, handle) = Engine::new(config);
                (engine, handle, None)
            }
            Render::App => {
                let (engine, handle, monitor) = crate::engine(config);
                (engine, handle, Some(monitor))
            }
            Render::Parallel(options) => {
                let renderer = ParallelRenderer::new(options.clone());
                let monitor = renderer.monitor();
                let (engine, handle) = Engine::with_renderer(config, Box::new(renderer));
                (engine, handle, Some(monitor))
            }
        };
        let mut session = Session::new(project, Keymap::default_keymap(), handle);
        session
            .run(&transport::PLAY)
            .expect("the app has a play command");
        // Everything waits for the graph, which compiles on its own thread.
        // Offline, there's no hurry: wait, so playing starts with the first
        // frame.
        session.wait_for_graph();
        Self {
            engine,
            session,
            monitor,
            buffer: vec![0.0; CHANNELS * config.max_block],
            took: Duration::ZERO,
            peak: 0.0,
        }
    }

    /// Renders the next `frames` frames, as a device callback would ask for
    /// them, and returns them: stereo, interleaved.
    ///
    /// # Panics
    ///
    /// If `frames` is more than the engine's block size.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        let buffer = &mut self.buffer[..CHANNELS * frames];
        let started = Instant::now();
        self.engine.process_interleaved(buffer, CHANNELS);
        self.took += started.elapsed();
        self.session.tick();
        self.peak = buffer.iter().fold(self.peak, |peak, s| peak.max(s.abs()));
        buffer
    }

    pub fn config(&self) -> EngineConfig {
        self.engine.config()
    }

    /// The time spent rendering so far.
    pub fn took(&self) -> Duration {
        self.took
    }

    /// The loudest sample so far.
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// The engine's counters: the playhead, whether it plays.
    pub fn stats(&self) -> Stats {
        self.session.stats()
    }

    /// What the parallel scheduler did, if it plays.
    pub fn parallel(&self) -> Option<ParallelStats> {
        self.monitor.as_ref().map(ParallelMonitor::stats)
    }
}

/// Where a player's output first differed from the reference's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Difference {
    /// Frames from the start.
    pub frame: u64,
    /// 0 is left, 1 is right.
    pub channel: usize,
    pub expected: f32,
    pub got: f32,
}

/// Plays `frames` frames on every player side by side, one buffer of the
/// engines' block size at a time, and checks each player's output against
/// the first player's, bit for bit. Returns where each player first
/// differed, if it did; the first player's entry is always `None`.
///
/// # Panics
///
/// If the players' engines have different configs.
pub fn compare(players: &mut [Player], frames: u64) -> Vec<Option<Difference>> {
    let mut differences = vec![None; players.len()];
    let Some((reference, others)) = players.split_first_mut() else {
        return differences;
    };
    let config = reference.config();
    assert!(
        others.iter().all(|player| player.config() == config),
        "the players render for different configs"
    );
    let mut done = 0;
    while done < frames {
        let block = (frames - done).min(config.max_block as u64) as usize;
        let expected = reference.render(block);
        for (player, difference) in others.iter_mut().zip(&mut differences[1..]) {
            let got = player.render(block);
            if difference.is_none() {
                *difference = first_difference(expected, got, done);
            }
        }
        done += block as u64;
    }
    differences
}

/// The first sample of `got` that isn't bit for bit the one in `expected`,
/// both starting at frame `start`.
fn first_difference(expected: &[f32], got: &[f32], start: u64) -> Option<Difference> {
    let i = expected
        .iter()
        .zip(got)
        .position(|(e, g)| e.to_bits() != g.to_bits())?;
    Some(Difference {
        frame: start + (i / CHANNELS) as u64,
        channel: i % CHANNELS,
        expected: expected[i],
        got: got[i],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn differences_are_found_to_the_bit() {
        let expected = [0.0, 0.5, 0.25, 0.125];
        assert_eq!(first_difference(&expected, &expected, 10), None);
        let got = [0.0, 0.5, 0.25, f32::from_bits(0.125f32.to_bits() + 1)];
        let found = first_difference(&expected, &got, 10).unwrap();
        assert_eq!((found.frame, found.channel), (11, 1));
        // A different zero is a different sample too.
        let got = [-0.0, 0.5, 0.25, 0.125];
        assert_eq!(first_difference(&expected, &got, 0).unwrap().frame, 0);
    }
}
