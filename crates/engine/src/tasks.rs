//! A node's own work, split into tasks that can run at once, as a plugin
//! using CLAP's thread pool splits its voices.

use std::panic::{self, AssertUnwindSafe};

/// Calls `task` with each of `0..tasks`, and returns once every call has
/// returned. In a block that the [`ParallelRenderer`](crate::ParallelRenderer)
/// shares between threads, the calls run on whichever of its threads are
/// free, this one included, several at once. Elsewhere they run here, one
/// after another. A block that the renderer runs on the audio thread alone
/// has the blocks after it shared, if they take enough work, so the tasks
/// can be.
///
/// Call it from a node's [`process`](crate::Node::process). The tasks run
/// on real-time threads, by the same rules as `process`. A task that calls
/// `run` runs its own tasks itself. Every task runs, even when one panics:
/// a panic on this thread comes out of `run` once they're done, and one on
/// another thread at the end of the block.
pub fn run(tasks: u32, task: &(dyn Fn(u32) + Sync)) {
    if tasks > 1 && crate::parallel::run_tasks(tasks, task) {
        return;
    }
    let mut failure = None;
    for index in 0..tasks {
        if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| task(index))) {
            failure.get_or_insert(payload);
        }
    }
    if let Some(payload) = failure {
        panic::resume_unwind(payload);
    }
}
