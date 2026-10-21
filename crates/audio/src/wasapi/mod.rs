//! Our WASAPI backend. `docs/audio.md` describes how it works.
//!
//! All WASAPI calls happen on threads of ours in COM's multithreaded
//! apartment: the stream's audio thread, or a short-lived thread for
//! listing devices.

mod client;
mod com;
mod device;
mod stream;

pub(crate) use device::{capabilities, output_devices};
pub(crate) use stream::{OpenStream, Stream, open};
