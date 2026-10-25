//! The window's panels, by id: the arrangement, the mixer, the devices, the
//! piano roll, the Sample Editor, the browser and the history. The UI docks
//! them by these ids, the focus is on one of them, and a keymap can bind
//! keys in one of them only.

/// The arrangement's panel, in the middle of the window.
pub const ARRANGEMENT: &str = "arrangement";
/// The mixer's panel, under the arrangement.
pub const MIXER: &str = "mixer";
/// The devices panel, under the mixer.
pub const DEVICES: &str = "devices";
/// The piano roll's panel, in a tab beside the devices.
pub const PIANO_ROLL: &str = "piano_roll";
/// The Sample Editor's panel, in a tab beside the devices and the piano
/// roll: it edits audio clips as the piano roll edits MIDI clips.
pub const SAMPLE_EDITOR: &str = "sample_editor";
/// The browser's panel, on the left.
pub const BROWSER: &str = "browser";
/// The History panel, which lists the steps of the undo history, in a tab
/// beside the browser.
pub const HISTORY: &str = "history";

/// Every panel's id.
pub const ALL: [&str; 7] = [
    ARRANGEMENT,
    MIXER,
    DEVICES,
    PIANO_ROLL,
    SAMPLE_EDITOR,
    BROWSER,
    HISTORY,
];

/// The id `id` as [`ALL`] has it, if it's a panel's.
pub fn find(id: &str) -> Option<&'static str> {
    ALL.into_iter().find(|&panel| panel == id)
}
