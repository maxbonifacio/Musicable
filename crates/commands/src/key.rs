//! Keys, modifiers and key chords, and how keymap files write them.

use std::fmt;
use std::ops::BitOr;
use std::str::FromStr;

macro_rules! keys {
    ($($key:ident => $name:literal,)*) => {
        /// A key on the keyboard.
        ///
        /// Key chords write letters, digits and the named keys by name: `A`,
        /// `7`, `F5`, `Space`, `PageUp`. Punctuation keys are named after the
        /// character they type without Shift on a US layout, such as `Minus`
        /// or `Slash`, and can be written as that character too. The UI layer
        /// maps other keyboard layouts onto these keys.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Key {
            $($key,)*
        }

        impl Key {
            /// Every key.
            pub const ALL: &'static [Key] = &[$(Key::$key),*];

            /// The key's name in key chords.
            pub const fn name(self) -> &'static str {
                match self {
                    $(Key::$key => $name,)*
                }
            }
        }
    };
}

keys! {
    A => "A", B => "B", C => "C", D => "D", E => "E", F => "F", G => "G", H => "H", I => "I",
    J => "J", K => "K", L => "L", M => "M", N => "N", O => "O", P => "P", Q => "Q", R => "R",
    S => "S", T => "T", U => "U", V => "V", W => "W", X => "X", Y => "Y", Z => "Z",
    Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
    Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
    F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5", F6 => "F6",
    F7 => "F7", F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
    F13 => "F13", F14 => "F14", F15 => "F15", F16 => "F16", F17 => "F17", F18 => "F18",
    F19 => "F19", F20 => "F20", F21 => "F21", F22 => "F22", F23 => "F23", F24 => "F24",
    Space => "Space", Enter => "Enter", Tab => "Tab", Backspace => "Backspace",
    Escape => "Escape", Insert => "Insert", Delete => "Delete",
    Home => "Home", End => "End", PageUp => "PageUp", PageDown => "PageDown",
    Up => "Up", Down => "Down", Left => "Left", Right => "Right",
    Minus => "Minus", Equals => "Equals", OpenBracket => "OpenBracket",
    CloseBracket => "CloseBracket", Backslash => "Backslash", Semicolon => "Semicolon",
    Quote => "Quote", Backtick => "Backtick", Comma => "Comma", Period => "Period",
    Slash => "Slash",
    Numpad0 => "Numpad0", Numpad1 => "Numpad1", Numpad2 => "Numpad2", Numpad3 => "Numpad3",
    Numpad4 => "Numpad4", Numpad5 => "Numpad5", Numpad6 => "Numpad6", Numpad7 => "Numpad7",
    Numpad8 => "Numpad8", Numpad9 => "Numpad9", NumpadAdd => "NumpadAdd",
    NumpadSubtract => "NumpadSubtract", NumpadMultiply => "NumpadMultiply",
    NumpadDivide => "NumpadDivide", NumpadDecimal => "NumpadDecimal",
    NumpadEnter => "NumpadEnter",
}

/// Other spellings key chords accept.
const KEY_ALIASES: &[(&str, Key)] = &[
    ("Esc", Key::Escape),
    ("Return", Key::Enter),
    ("Del", Key::Delete),
    ("PgUp", Key::PageUp),
    ("PgDn", Key::PageDown),
    ("ArrowUp", Key::Up),
    ("ArrowDown", Key::Down),
    ("ArrowLeft", Key::Left),
    ("ArrowRight", Key::Right),
    ("-", Key::Minus),
    ("=", Key::Equals),
    ("[", Key::OpenBracket),
    ("]", Key::CloseBracket),
    ("\\", Key::Backslash),
    (";", Key::Semicolon),
    ("'", Key::Quote),
    ("`", Key::Backtick),
    (",", Key::Comma),
    (".", Key::Period),
    ("/", Key::Slash),
];

impl Key {
    /// The key a chord names, ignoring case: its [`name`](Self::name), or
    /// another common spelling such as `Esc` or `.`.
    pub fn from_name(name: &str) -> Option<Key> {
        Key::ALL
            .iter()
            .map(|&key| (key.name(), key))
            .chain(KEY_ALIASES.iter().copied())
            .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
            .map(|(_, key)| key)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.name())
    }
}

/// Modifier keys held with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Command on macOS, the Windows key on Windows.
    pub cmd: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        cmd: false,
    };
    pub const CTRL: Self = Self {
        ctrl: true,
        ..Self::NONE
    };
    pub const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };
    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };
    pub const CMD: Self = Self {
        cmd: true,
        ..Self::NONE
    };

    /// Each modifier's name in key chords, in the order chords write them.
    const NAMES: [(Modifiers, &'static str); 4] = [
        (Self::CTRL, "Ctrl"),
        (Self::ALT, "Alt"),
        (Self::SHIFT, "Shift"),
        (Self::CMD, "Cmd"),
    ];

    /// Other spellings key chords accept.
    const ALIASES: [(Modifiers, &'static str); 5] = [
        (Self::CTRL, "Control"),
        (Self::ALT, "Option"),
        (Self::CMD, "Command"),
        (Self::CMD, "Super"),
        (Self::CMD, "Win"),
    ];

    /// The modifier called `name`, such as `Ctrl`, `Control` or `ctrl`.
    pub fn from_name(name: &str) -> Option<Modifiers> {
        Self::NAMES
            .iter()
            .chain(&Self::ALIASES)
            .find(|(_, spelling)| spelling.eq_ignore_ascii_case(name))
            .map(|&(mods, _)| mods)
    }

    /// Reads the modifiers in front of the last part of `text`, such as the
    /// Ctrl and Shift of `Ctrl+Shift+M`, and returns them with that part,
    /// trimmed. Mouse maps write gestures this way too: `Alt+Drag`.
    pub fn split_off(text: &str) -> Result<(Modifiers, &str), ChordError> {
        let Some((names, last)) = text.rsplit_once('+') else {
            return Ok((Self::NONE, text.trim()));
        };
        let mut mods = Self::NONE;
        for name in names.split('+').map(str::trim) {
            if name.is_empty() {
                return Err(ChordError::EmptyName);
            }
            let modifier = Self::from_name(name)
                .ok_or_else(|| ChordError::UnknownModifier(name.to_owned()))?;
            if mods.overlaps(modifier) {
                return Err(ChordError::RepeatedModifier(name.to_owned()));
            }
            mods = mods | modifier;
        }
        Ok((mods, last.trim()))
    }

    fn overlaps(self, other: Modifiers) -> bool {
        (self.ctrl && other.ctrl)
            || (self.alt && other.alt)
            || (self.shift && other.shift)
            || (self.cmd && other.cmd)
    }
}

impl BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self {
            ctrl: self.ctrl || other.ctrl,
            alt: self.alt || other.alt,
            shift: self.shift || other.shift,
            cmd: self.cmd || other.cmd,
        }
    }
}

/// A key pressed with modifiers, such as Ctrl+Shift+M.
///
/// Keymap files write a chord as its modifiers and its key, joined by `+`:
/// `Space`, `Ctrl+L`, `Ctrl+Shift+Up`. The modifiers are `Ctrl`, `Alt`,
/// `Shift` and `Cmd`, in any order, and case doesn't matter. Displaying a
/// chord writes it that way, with the modifiers in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub mods: Modifiers,
    pub key: Key,
}

impl KeyChord {
    pub const fn new(key: Key, mods: Modifiers) -> Self {
        Self { mods, key }
    }
}

impl From<Key> for KeyChord {
    /// The key on its own.
    fn from(key: Key) -> Self {
        Self::new(key, Modifiers::NONE)
    }
}

impl fmt::Display for Modifiers {
    /// The modifiers as key chords write them, joined by `+` in the order
    /// chords write them: `Ctrl+Shift`. No modifiers write nothing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = Self::NAMES
            .iter()
            .filter(|&&(mods, _)| self.overlaps(mods))
            .map(|&(_, name)| name)
            .collect();
        f.pad(&names.join("+"))
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut chord = self.mods.to_string();
        if !chord.is_empty() {
            chord.push('+');
        }
        chord.push_str(self.key.name());
        f.pad(&chord)
    }
}

impl FromStr for KeyChord {
    type Err = ChordError;

    fn from_str(text: &str) -> Result<Self, ChordError> {
        if text.trim().is_empty() {
            return Err(ChordError::Empty);
        }
        let (mods, key) = Modifiers::split_off(text)?;
        if key.is_empty() || Modifiers::from_name(key).is_some() {
            return Err(ChordError::MissingKey);
        }
        let key = Key::from_name(key).ok_or_else(|| ChordError::UnknownKey(key.to_owned()))?;
        Ok(Self::new(key, mods))
    }
}

/// Why a key chord didn't parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChordError {
    /// Nothing to parse.
    Empty,
    /// Modifiers with no key after them, such as `Ctrl+Shift`.
    MissingKey,
    /// A `+` with nothing before it, such as `Ctrl++`.
    EmptyName,
    UnknownKey(String),
    UnknownModifier(String),
    /// A modifier given twice, such as `Ctrl+Ctrl+M`.
    RepeatedModifier(String),
}

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("the chord is empty"),
            Self::MissingKey => f.write_str("there's no key after the modifiers"),
            Self::EmptyName => {
                f.write_str("a + has nothing before it (the + key is Equals or NumpadAdd)")
            }
            Self::UnknownKey(key) => write!(f, "there's no key called \"{key}\""),
            Self::UnknownModifier(name) => write!(
                f,
                "\"{name}\" isn't a modifier (they are Ctrl, Alt, Shift and Cmd)"
            ),
            Self::RepeatedModifier(name) => write!(f, "{name} is there twice"),
        }
    }
}

impl std::error::Error for ChordError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    #[test]
    fn chords_parse_in_any_case_and_order() {
        let expected = KeyChord::new(Key::M, Modifiers::CTRL | Modifiers::SHIFT);
        for text in [
            "Ctrl+Shift+M",
            "shift+ctrl+m",
            "CTRL + SHIFT + M",
            "Control+Shift+M",
        ] {
            assert_eq!(chord(text), expected, "{text}");
        }
        assert_eq!(chord("Space"), Key::Space.into());
        assert_eq!(chord("Esc"), Key::Escape.into());
        assert_eq!(chord("Ctrl+."), KeyChord::new(Key::Period, Modifiers::CTRL));
        assert_eq!(
            chord("Option+Command+5"),
            KeyChord::new(Key::Digit5, Modifiers::ALT | Modifiers::CMD)
        );
    }

    #[test]
    fn chords_display_as_they_parse() {
        let all_mods = [
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::SHIFT,
            Modifiers::CMD,
        ];
        for &key in Key::ALL {
            for bits in 0..16 {
                let mods = (0..4)
                    .filter(|i| bits & (1 << i) != 0)
                    .fold(Modifiers::NONE, |mods, i| mods | all_mods[i]);
                let original = KeyChord::new(key, mods);
                assert_eq!(chord(&original.to_string()), original, "{original}");
            }
        }
        assert_eq!(
            chord("shift+alt+cmd+ctrl+f5").to_string(),
            "Ctrl+Alt+Shift+Cmd+F5"
        );
        assert_eq!(chord("ctrl+=").to_string(), "Ctrl+Equals");
    }

    #[test]
    fn modifiers_read_and_write_as_chords_do() {
        assert_eq!(Modifiers::NONE.to_string(), "");
        let mods = Modifiers::SHIFT | Modifiers::CTRL;
        assert_eq!(mods.to_string(), "Ctrl+Shift");
        assert_eq!(
            Modifiers::split_off(" Drag "),
            Ok((Modifiers::NONE, "Drag"))
        );
        assert_eq!(
            Modifiers::split_off("shift + ctrl+Drag"),
            Ok((mods, "Drag"))
        );
        assert_eq!(
            Modifiers::split_off("Hyper+Drag"),
            Err(ChordError::UnknownModifier("Hyper".into()))
        );
    }

    #[test]
    fn key_names_are_unique() {
        for (i, a) in Key::ALL.iter().enumerate() {
            for b in &Key::ALL[i + 1..] {
                assert!(!a.name().eq_ignore_ascii_case(b.name()), "{a:?} and {b:?}");
            }
            assert!(Modifiers::from_name(a.name()).is_none(), "{a:?}");
        }
        for (alias, _) in KEY_ALIASES {
            assert!(
                Key::ALL
                    .iter()
                    .all(|k| !k.name().eq_ignore_ascii_case(alias)),
                "{alias}"
            );
        }
    }

    #[test]
    fn bad_chords_say_what_is_wrong() {
        let cases = [
            ("", ChordError::Empty),
            ("  ", ChordError::Empty),
            ("Ctrl+", ChordError::MissingKey),
            ("Ctrl+Shift", ChordError::MissingKey),
            ("Ctrl++", ChordError::EmptyName),
            ("+M", ChordError::EmptyName),
            ("Ctrl+Spce", ChordError::UnknownKey("Spce".into())),
            ("Ctl+M", ChordError::UnknownModifier("Ctl".into())),
            (
                "Ctrl+Control+M",
                ChordError::RepeatedModifier("Control".into()),
            ),
        ];
        for (text, error) in cases {
            assert_eq!(text.parse::<KeyChord>(), Err(error), "{text:?}");
        }
    }
}
