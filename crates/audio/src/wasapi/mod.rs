//! Our WASAPI backend. `docs/audio.md` describes how it works.
//!
//! All WASAPI calls happen on threads of ours in COM's multithreaded
//! apartment: the stream's audio thread, each input's capture thread, or a
//! short-lived thread for listing devices.

mod capture;
mod client;
mod com;
mod device;
mod stream;

// For inputs that record through the system with an ASIO output.
pub(crate) use capture::{Capture, Output, open as open_inputs};
pub(crate) use device::{capabilities, input_devices, output_devices};
pub(crate) use stream::{OpenStream, Stream, open};
