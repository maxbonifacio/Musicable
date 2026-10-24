//! The window's panels, by id: the arrangement, the mixer, the devices, the
//! piano roll and the browser. The UI docks them by these ids, the focus is
//! on one of them, and a keymap can bind keys in one of them only.

/// The arrangement's panel, in the middle of the window.
pub const ARRANGEMENT: &str = "arrangement";
/// The mixer's panel, under the arrangement.
pub const MIXER: &str = "mixer";
/// The devices panel, under the mixer.
pub const DEVICES: &str = "devices";
/// The piano roll's panel, in a tab beside the devices.
pub const PIANO_ROLL: &str = "piano_roll";
/// The browser's panel, on the left.
pub const BROWSER: &str = "browser";

/// Every panel's id.
pub const ALL: [&str; 5] = [ARRANGEMENT, MIXER, DEVICES, PIANO_ROLL, BROWSER];

/// The id `id` as [`ALL`] has it, if it's a panel's.
pub fn find(id: &str) -> Option<&'static str> {
    ALL.into_iter().find(|&panel| panel == id)
}
