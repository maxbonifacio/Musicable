//! ASIO output, in builds with the `asio` feature. `docs/audio.md` describes
//! how it works, and why those builds are distributed under the GPL.
//!
//! An ASIO driver is a COM class the registry lists. A stream loads its
//! driver on a thread of its own, which makes every call to it but the
//! real-time ones, and the driver calls back on its own thread to have each
//! buffer half filled. Elsewhere than on Windows only the tests build this,
//! with a fake driver.

#![cfg_attr(not(windows), allow(dead_code))]

mod context;
mod convert;
mod detector;
mod driver;
#[cfg(test)]
mod fake;
mod platform;
#[cfg(windows)]
mod registry;
mod render;
mod stream;
mod sys;
#[cfg(test)]
mod tests;

#[cfg(windows)]
pub(crate) use registry::{capabilities, open, output_devices};
#[cfg(windows)]
pub(crate) use stream::{OpenStream, Stream};
