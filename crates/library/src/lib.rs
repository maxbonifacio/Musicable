//! The browser's library: the folders of samples the user added, indexed
//! file by file, with room for what analysis will add; finding files in it
//! by name; the presets; and the queue that does file work in the
//! background.
//!
//! The app keeps the [`Index`] in a file in the config folder, scans its
//! places on the [`Queue`] with [`scan()`], and searches it with a
//! [`Searcher`]. `docs/app.md` describes the browser built on it.

pub mod index;
pub mod presets;
pub mod queue;
pub mod scan;
pub mod search;

pub use index::{
    AUDIO_EXTENSIONS, AudioFacts, FileId, Format, Index, IndexError, Measured, Place, Record, Tag,
    TagSource, is_audio_file,
};
pub use presets::{Loaded, Preset, PresetError, PresetKind};
pub use queue::{Adder, Busy, Pace, Priority, Queue};
pub use scan::scan;
pub use search::{Found, Hit, Prepared, Query, Searcher};
