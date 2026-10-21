//! Audio graph engine.
//!
//! The project is compiled into a directed acyclic graph of [`Node`]s. This
//! crate starts with the single-threaded [`ReferenceRenderer`]: the parallel
//! scheduler that comes next must produce sample-identical output to it, and
//! the tests here are the harness for that.

pub mod graph;
pub mod reference;

pub use graph::{Graph, GraphError, NodeId};
pub use reference::ReferenceRenderer;

/// One processing unit: an instrument, an effect, a track's gain stage, a mix
/// bus. Implementations must be real-time safe in `process`: no allocation,
/// locking or I/O.
pub trait Node: Send {
    /// Mixes or transforms `inputs` into `output`. Every slice is one block
    /// long. Buffers are mono for now; multichannel comes with the bus model.
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]);
}

/// Sums its inputs. Used for buses and the master.
pub struct Sum;

impl Node for Sum {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        output.fill(0.0);
        for input in inputs {
            for (o, i) in output.iter_mut().zip(input.iter()) {
                *o += *i;
            }
        }
    }
}

/// Sums its inputs and applies a linear gain.
pub struct Gain(pub f32);

impl Node for Gain {
    fn process(&mut self, inputs: &[&[f32]], output: &mut [f32]) {
        Sum.process(inputs, output);
        for o in output.iter_mut() {
            *o *= self.0;
        }
    }
}

/// Outputs a constant value. Mostly useful in tests.
pub struct Constant(pub f32);

impl Node for Constant {
    fn process(&mut self, _inputs: &[&[f32]], output: &mut [f32]) {
        output.fill(self.0);
    }
}
