//! Project files and audio/MIDI import and export.
//!
//! [`project`] saves and loads Musicable's own project files, and [`audio`]
//! reads WAV, AIFF and FLAC files and writes WAV and AIFF. Planned: MIDI
//! files, DAWproject, and eventually Ableton `.als` import and export.

pub mod audio;
pub mod project;
