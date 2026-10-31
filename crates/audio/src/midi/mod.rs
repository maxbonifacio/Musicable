//! MIDI inputs, lined up with the output.
//!
//! A stream opens MIDI inputs alongside its audio inputs
//! ([`StreamRequest::midi_inputs`](crate::StreamRequest::midi_inputs)).
//! Each message an input brings is stamped with the engine frame heard as
//! it came in: the frame a player heard as they played it. Its
//! [`MidiBuffer`] holds the messages for a recording to read, and the
//! stream's audio thread plays them through the engine a fixed delay later
//! ([`ProcessContext::midi`](musicable_engine::ProcessContext::midi)), so
//! they keep the timing they were played with, however the wake-ups fall.
//! `docs/audio.md` describes how.
//!
//! Only Windows records MIDI so far, through WinMM: elsewhere a stream's
//! MIDI inputs fail to open, and only the tests use most of this.

#![cfg_attr(not(windows), allow(dead_code))]

pub mod agent;
mod bus;
mod ring;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod winmm;

use std::sync::Arc;

pub(crate) use bus::MidiBus;
pub use ring::{
    EventBuffer, EventReader, EventWriter, MidiBuffer, MidiEvent, MidiReader, MidiWriter,
};

use crate::input::Playback;

/// How many messages a MIDI input's buffer holds: seconds of the busiest
/// MPE playing, far more than a recording falls behind.
const CAPACITY: usize = 16_384;

/// A MIDI input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiInputDevice {
    /// Identifies the input across runs: give it in
    /// [`StreamRequest::midi_inputs`](crate::StreamRequest::midi_inputs) to
    /// open it. It's the device's name, with a number after the name for
    /// the second and later of devices with the same name.
    pub id: String,
    pub name: String,
}

/// A MIDI input a stream opened, or tried to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiInputInfo {
    /// As requested.
    pub id: String,
    pub name: String,
    /// Why it didn't open.
    pub error: Option<String>,
}

/// The MIDI inputs the system has, and on Windows, where Musicable
/// records MIDI, the one agents play ([`agent`]).
pub(crate) fn input_devices() -> Vec<MidiInputDevice> {
    let system = System::list();
    let mut devices: Vec<MidiInputDevice> = system
        .ids
        .into_iter()
        .zip(system.names)
        .map(|(id, name)| MidiInputDevice { id, name })
        .collect();
    if cfg!(windows) {
        devices.push(MidiInputDevice {
            id: agent::AGENT_INPUT.to_owned(),
            name: agent::AGENT_INPUT.to_owned(),
        });
    }
    devices
}

/// Each of `names`' ids: the name, or for the second and later of the same
/// name, the name and a number.
pub(crate) fn ids(names: &[String]) -> Vec<String> {
    names
        .iter()
        .enumerate()
        .map(
            |(i, name)| match names[..i].iter().filter(|&n| n == name).count() {
                0 => name.clone(),
                before => format!("{name} #{}", before + 1),
            },
        )
        .collect()
}

/// The MIDI inputs a stream opened, or tried to, in the order asked for.
/// Dropping it closes them.
#[derive(Default)]
pub(crate) struct MidiInputs {
    pub infos: Vec<MidiInputInfo>,
    /// `None` for an input that didn't open.
    pub buffers: Vec<Option<MidiBuffer>>,
    open: Vec<Open>,
    /// The input agents play, if it's one of them.
    agent: Option<agent::Input>,
}

impl MidiInputs {
    /// Opens the MIDI inputs `ids` names. Each stamps what it brings with
    /// the frame `playback` says is heard as it comes in.
    pub fn open(ids: &[String], playback: &Arc<Playback>) -> MidiInputs {
        let mut inputs = MidiInputs::default();
        let system = System::list();
        for id in ids {
            if cfg!(windows) && id == agent::AGENT_INPUT {
                let (buffer, input) = agent::open(CAPACITY, playback);
                inputs.agent = Some(input);
                inputs.infos.push(MidiInputInfo {
                    id: id.clone(),
                    name: id.clone(),
                    error: None,
                });
                inputs.buffers.push(Some(buffer));
                continue;
            }
            let (name, buffer, error) = match system.open(id, playback) {
                Ok((name, buffer, open)) => {
                    inputs.open.push(open);
                    (name, Some(buffer), None)
                }
                Err(error) => (id.clone(), None, Some(error)),
            };
            inputs.infos.push(MidiInputInfo {
                id: id.clone(),
                name,
                error,
            });
            inputs.buffers.push(buffer);
        }
        inputs
    }

    /// The bus that plays them through the engine.
    pub fn bus(&self) -> MidiBus {
        let opened = self.buffers.iter().enumerate();
        MidiBus::new(
            opened
                .filter_map(|(i, buffer)| Some((i, buffer.clone()?)))
                .collect(),
        )
    }
}

/// An input that opened, which stays open while it lives.
#[cfg(windows)]
type Open = winmm::Input;
#[cfg(not(windows))]
type Open = ();

/// The system's MIDI inputs, to open: their names, and their ids.
struct System {
    #[cfg(windows)]
    numbers: Vec<u32>,
    names: Vec<String>,
    ids: Vec<String>,
}

impl System {
    #[cfg(windows)]
    fn list() -> System {
        let (numbers, names): (Vec<u32>, Vec<String>) = winmm::devices().into_iter().unzip();
        let ids = ids(&names);
        System {
            numbers,
            names,
            ids,
        }
    }

    #[cfg(not(windows))]
    fn list() -> System {
        System {
            names: Vec::new(),
            ids: Vec::new(),
        }
    }

    /// Opens the input `id`: its name, its buffer and the input.
    #[cfg(windows)]
    fn open(
        &self,
        id: &str,
        playback: &Arc<Playback>,
    ) -> Result<(String, MidiBuffer, Open), String> {
        let i = (self.ids.iter())
            .position(|d| d == id)
            .ok_or_else(|| "it isn't plugged in".to_owned())?;
        let (buffer, writer) = MidiBuffer::new(CAPACITY);
        let open = winmm::Input::open(self.numbers[i], writer, playback.clone())?;
        Ok((self.names[i].clone(), buffer, open))
    }

    #[cfg(not(windows))]
    fn open(&self, _id: &str, _: &Arc<Playback>) -> Result<(String, MidiBuffer, Open), String> {
        Err("only Windows records MIDI so far".to_owned())
    }
}
