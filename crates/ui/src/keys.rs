//! Key presses as keymaps name them, and handing them to the keymap before
//! egui sees them.

use std::collections::HashSet;

use egui::{Event, RawInput, ViewportCommand};
use musicable_commands::{Key, KeyChord, Modifiers};

/// The key chord a key press makes, or `None` for a key keymaps have no name
/// for, such as a modifier on its own.
///
/// `key` and `physical` are what egui reports: the key as the keyboard layout
/// has it, and the key in the same place on a US keyboard. Keymaps name keys
/// after a US keyboard, so a key press is named:
///
/// - On the digit row (and the numeric keypad), by its place: the digit it
///   has on a US keyboard, even where the layout needs Shift to type it, as
///   AZERTY does.
/// - Otherwise by what the layout types, when keymaps have a name for it:
///   Ctrl+Z is the key labelled Z, wherever the layout puts it, and a key
///   that types `-` is Minus.
/// - Otherwise by its place on a US keyboard. That covers letters on layouts
///   without Latin letters, and characters a US keyboard has no key for, such
///   as `ö`. Shift changes what a key types, so a key pressed with Shift is
///   named this way too: Shift+1 types `!`, and is Shift+1.
///
/// egui reports the numeric keypad's keys as the main keyboard's, so the
/// keypad's 5 is the 5 key. Only NumpadAdd comes through as itself.
pub fn chord(
    key: egui::Key,
    physical: Option<egui::Key>,
    modifiers: egui::Modifiers,
) -> Option<KeyChord> {
    let key = if key == egui::Key::Plus && physical == Some(egui::Key::Plus) {
        Key::NumpadAdd
    } else if let Some(digit) = physical.and_then(digit) {
        digit
    } else {
        named(key).or_else(|| physical.and_then(named))?
    };
    let mods = Modifiers {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        cmd: modifiers.mac_cmd,
    };
    Some(KeyChord::new(key, mods))
}

fn digit(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    Some(match key {
        E::Num0 => Key::Digit0,
        E::Num1 => Key::Digit1,
        E::Num2 => Key::Digit2,
        E::Num3 => Key::Digit3,
        E::Num4 => Key::Digit4,
        E::Num5 => Key::Digit5,
        E::Num6 => Key::Digit6,
        E::Num7 => Key::Digit7,
        E::Num8 => Key::Digit8,
        E::Num9 => Key::Digit9,
        _ => return None,
    })
}

/// The keymap key with the same name as an egui key, if there is one.
fn named(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    if let Some(digit) = digit(key) {
        return Some(digit);
    }
    Some(match key {
        E::A => Key::A,
        E::B => Key::B,
        E::C => Key::C,
        E::D => Key::D,
        E::E => Key::E,
        E::F => Key::F,
        E::G => Key::G,
        E::H => Key::H,
        E::I => Key::I,
        E::J => Key::J,
        E::K => Key::K,
        E::L => Key::L,
        E::M => Key::M,
        E::N => Key::N,
        E::O => Key::O,
        E::P => Key::P,
        E::Q => Key::Q,
        E::R => Key::R,
        E::S => Key::S,
        E::T => Key::T,
        E::U => Key::U,
        E::V => Key::V,
        E::W => Key::W,
        E::X => Key::X,
        E::Y => Key::Y,
        E::Z => Key::Z,
        E::F1 => Key::F1,
        E::F2 => Key::F2,
        E::F3 => Key::F3,
        E::F4 => Key::F4,
        E::F5 => Key::F5,
        E::F6 => Key::F6,
        E::F7 => Key::F7,
        E::F8 => Key::F8,
        E::F9 => Key::F9,
        E::F10 => Key::F10,
        E::F11 => Key::F11,
        E::F12 => Key::F12,
        E::F13 => Key::F13,
        E::F14 => Key::F14,
        E::F15 => Key::F15,
        E::F16 => Key::F16,
        E::F17 => Key::F17,
        E::F18 => Key::F18,
        E::F19 => Key::F19,
        E::F20 => Key::F20,
        E::F21 => Key::F21,
        E::F22 => Key::F22,
        E::F23 => Key::F23,
        E::F24 => Key::F24,
        E::Space => Key::Space,
        E::Enter => Key::Enter,
        E::Tab => Key::Tab,
        E::Backspace => Key::Backspace,
        E::Escape => Key::Escape,
        E::Insert => Key::Insert,
        E::Delete => Key::Delete,
        E::Home => Key::Home,
        E::End => Key::End,
        E::PageUp => Key::PageUp,
        E::PageDown => Key::PageDown,
        E::ArrowUp => Key::Up,
        E::ArrowDown => Key::Down,
        E::ArrowLeft => Key::Left,
        E::ArrowRight => Key::Right,
        E::Minus => Key::Minus,
        E::Equals => Key::Equals,
        E::OpenBracket => Key::OpenBracket,
        E::CloseBracket => Key::CloseBracket,
        E::Backslash => Key::Backslash,
        E::Semicolon => Key::Semicolon,
        E::Quote => Key::Quote,
        E::Backtick => Key::Backtick,
        E::Comma => Key::Comma,
        E::Period => Key::Period,
        E::Slash => Key::Slash,
        _ => return None,
    })
}

/// Hands key presses to the keymap before egui sees them.
///
/// Call [`take`](Self::take) on each frame's input before egui runs. A key
/// press the keymap binds runs its command and never reaches egui, so a
/// focused button can't act on Space as well. While a text field has focus,
/// every key press goes to the text field instead: typing a space in a name
/// types a space. While the Settings window's Keys page waits for the keys
/// to give a command, [`capture`](Self::capture) takes the next chord
/// instead.
#[derive(Debug, Default)]
pub struct KeyPresses {
    /// Keys held down, by egui's name for them, to tell a key's first press
    /// from the repeats the system sends while it's held.
    held: HashSet<egui::Key>,
    /// Keys held down whose first press the keymap or a capture took. Their
    /// repeats go nowhere, so egui never sees a key it didn't see pressed.
    taken: HashSet<egui::Key>,
}

impl KeyPresses {
    pub fn new() -> Self {
        Self::default()
    }

    /// Goes through the key presses in `input`. Unless a text field has focus,
    /// it asks `keymap` about each one, with the chord and whether the press
    /// repeats one held down. `keymap` returns whether a command is bound to
    /// the chord, and the press is taken out of `input` if one is. So are the
    /// repeats of a key taken, without asking again.
    ///
    /// The app turns off eframe's own clipboard shortcuts, so that Ctrl+C,
    /// Ctrl+X and Ctrl+V reach the keymap like any other chord. In a text
    /// field, this turns them into the copy, cut and paste egui expects.
    pub fn take(
        &mut self,
        ctx: &egui::Context,
        input: &mut RawInput,
        mut keymap: impl FnMut(KeyChord, bool) -> bool,
    ) {
        let text_field = ctx.text_edit_focused();
        input.events.retain_mut(|event| match *event {
            Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } => {
                let repeat = !self.held.insert(key);
                if repeat && self.taken.contains(&key) {
                    return false;
                }
                if text_field {
                    match clipboard(key, modifiers) {
                        Some(Clipboard::Cut) => *event = Event::Cut,
                        Some(Clipboard::Copy) => *event = Event::Copy,
                        Some(Clipboard::Paste) => {
                            if !repeat {
                                ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
                            }
                            return false;
                        }
                        None => {}
                    }
                    return true;
                }
                let Some(chord) = chord(key, physical_key, modifiers) else {
                    return true;
                };
                let taken = keymap(chord, repeat);
                if taken {
                    self.taken.insert(key);
                }
                !taken
            }
            Event::Key {
                key,
                pressed: false,
                ..
            } => {
                self.let_go(key);
                true
            }
            // Keys let go of while the window is in the background never say so.
            Event::WindowFocused(false) => {
                self.held.clear();
                self.taken.clear();
                true
            }
            _ => true,
        });
    }

    /// Takes every key press out of `input`, for the Settings window's Keys
    /// page while it waits for the keys to give a command, and returns the
    /// first chord pressed. Any key can be part of it, Escape, Enter and Tab
    /// too. Repeats, a modifier pressed on its own and keys keymaps have no
    /// name for are skipped. Text goes nowhere either, so a text field with
    /// focus doesn't type the keys.
    pub fn capture(&mut self, input: &mut RawInput) -> Option<KeyChord> {
        let mut captured = None;
        input.events.retain(|event| match *event {
            Event::Key {
                key,
                physical_key,
                pressed: true,
                modifiers,
                ..
            } => {
                let repeat = !self.held.insert(key);
                if !repeat && captured.is_none() {
                    captured = chord(key, physical_key, modifiers);
                }
                self.taken.insert(key);
                false
            }
            Event::Key {
                key,
                pressed: false,
                ..
            } => {
                self.let_go(key);
                true
            }
            Event::Text(_) => false,
            Event::WindowFocused(false) => {
                self.held.clear();
                self.taken.clear();
                true
            }
            _ => true,
        });
        captured
    }

    fn let_go(&mut self, key: egui::Key) {
        self.held.remove(&key);
        self.taken.remove(&key);
    }
}

/// Takes the first press of `key` without modifiers out of the input, if it
/// isn't a repeat. A window that closes on Escape takes it so: holding the
/// key down closes that window, and not the one under it as well.
pub fn first_press(input: &mut egui::InputState, key: egui::Key) -> bool {
    let at = input.events.iter().position(|event| {
        matches!(event, Event::Key { key: k, pressed: true, repeat: false, modifiers, .. }
            if *k == key && modifiers.is_none())
    });
    at.map(|at| input.events.remove(at)).is_some()
}

enum Clipboard {
    Cut,
    Copy,
    Paste,
}

/// The clipboard shortcuts text fields have, as eframe would recognise them.
fn clipboard(key: egui::Key, modifiers: egui::Modifiers) -> Option<Clipboard> {
    use egui::Key as E;
    let windows = cfg!(windows);
    let command = modifiers.command_only();
    match key {
        E::Cut => Some(Clipboard::Cut),
        E::Copy => Some(Clipboard::Copy),
        E::Paste => Some(Clipboard::Paste),
        E::X if command => Some(Clipboard::Cut),
        E::C if command => Some(Clipboard::Copy),
        E::V if command => Some(Clipboard::Paste),
        E::Delete if windows && modifiers.shift => Some(Clipboard::Cut),
        E::Insert if windows && modifiers.ctrl => Some(Clipboard::Copy),
        E::Insert if windows && modifiers.shift => Some(Clipboard::Paste),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: egui::Modifiers = egui::Modifiers::NONE;

    fn named(text: &str) -> Option<KeyChord> {
        Some(text.parse().unwrap())
    }

    #[test]
    fn a_us_keyboard_gives_the_chords_keymaps_write() {
        use egui::Key as E;
        let ctrl_shift = egui::Modifiers::CTRL | egui::Modifiers::SHIFT;
        let cases = [
            (E::Space, Some(E::Space), NONE, "Space"),
            (
                E::Space,
                Some(E::Space),
                egui::Modifiers::SHIFT,
                "Shift+Space",
            ),
            (E::L, Some(E::L), egui::Modifiers::CTRL, "Ctrl+L"),
            (
                E::Period,
                Some(E::Period),
                egui::Modifiers::CTRL,
                "Ctrl+Period",
            ),
            (E::ArrowUp, Some(E::ArrowUp), ctrl_shift, "Ctrl+Shift+Up"),
            (E::Home, Some(E::Home), NONE, "Home"),
            (E::F13, Some(E::F13), egui::Modifiers::ALT, "Alt+F13"),
            // With Shift, egui reports what the key types: !, +, ?, :.
            (
                E::Exclamationmark,
                Some(E::Num1),
                egui::Modifiers::SHIFT,
                "Shift+1",
            ),
            (
                E::Plus,
                Some(E::Equals),
                egui::Modifiers::SHIFT,
                "Shift+Equals",
            ),
            (
                E::Questionmark,
                Some(E::Slash),
                egui::Modifiers::SHIFT,
                "Shift+Slash",
            ),
            (
                E::Colon,
                Some(E::Semicolon),
                ctrl_shift,
                "Ctrl+Shift+Semicolon",
            ),
            (E::Plus, Some(E::Plus), NONE, "NumpadAdd"),
        ];
        for (key, physical, mods, expected) in cases {
            assert_eq!(chord(key, physical, mods), named(expected), "{expected}");
        }
    }

    #[test]
    fn other_layouts_follow_the_labels_on_the_keys() {
        use egui::Key as E;
        let ctrl = egui::Modifiers::CTRL;
        let cases = [
            // German: Z and Y swap places, and Ctrl+Z is the key labelled Z.
            (E::Z, Some(E::Y), ctrl, "Ctrl+Z"),
            // German: - is where a US keyboard has /.
            (E::Minus, Some(E::Slash), NONE, "Minus"),
            // German: ö has no name, so it's named after its place.
            (E::Semicolon, Some(E::Semicolon), NONE, "Semicolon"),
            // AZERTY: A is where a US keyboard has Q.
            (E::A, Some(E::Q), ctrl, "Ctrl+A"),
            // AZERTY: the digit row types & é " ' ( - without Shift. egui
            // falls back to the place for &, but reports ' and - as such.
            (E::Num1, Some(E::Num1), ctrl, "Ctrl+1"),
            (E::Quote, Some(E::Num4), ctrl, "Ctrl+4"),
            (E::Minus, Some(E::Num6), NONE, "6"),
            // Russian: egui falls back to the place of Cyrillic letters.
            (E::F, Some(E::F), ctrl, "Ctrl+F"),
        ];
        for (key, physical, mods, expected) in cases {
            assert_eq!(chord(key, physical, mods), named(expected), "{expected}");
        }
    }

    #[test]
    fn modifiers_on_their_own_are_not_chords() {
        assert_eq!(
            chord(egui::Key::ShiftLeft, Some(egui::Key::ShiftLeft), NONE),
            None
        );
        assert_eq!(chord(egui::Key::F30, Some(egui::Key::F30), NONE), None);
    }

    #[test]
    fn mac_command_is_cmd() {
        let cmd = egui::Modifiers::MAC_CMD;
        let expected = KeyChord::new(Key::S, Modifiers::CMD);
        assert_eq!(chord(egui::Key::S, Some(egui::Key::S), cmd), Some(expected));
    }

    fn key(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn input(events: Vec<Event>) -> RawInput {
        RawInput {
            events,
            ..RawInput::default()
        }
    }

    #[test]
    fn bound_keys_go_to_the_keymap_and_the_rest_to_egui() {
        use egui::Key as E;
        let ctx = egui::Context::default();
        let mut keys = KeyPresses::new();
        let space = |pressed| key(E::Space, pressed, NONE);
        let q = key(E::Q, true, NONE);
        let mut input = input(vec![space(true), space(true), space(false), q.clone()]);
        let mut asked = Vec::new();
        keys.take(&ctx, &mut input, |chord, repeat| {
            asked.push((chord.to_string(), repeat));
            chord == Key::Space.into()
        });
        // Space's repeat goes nowhere, and Space isn't held any more once it's
        // let go.
        assert_eq!(
            asked,
            [("Space".to_owned(), false), ("Q".to_owned(), false)]
        );
        assert_eq!(input.events, [space(false), q]);

        // A key held while the window loses focus is never let go of.
        let mut input = self::input(vec![space(true), Event::WindowFocused(false)]);
        keys.take(&ctx, &mut input, |_, _| true);
        let mut input = self::input(vec![space(true)]);
        let mut repeats = Vec::new();
        keys.take(&ctx, &mut input, |_, repeat| {
            repeats.push(repeat);
            true
        });
        assert_eq!(repeats, [false]);
    }

    #[test]
    fn a_capture_takes_the_next_chord_and_every_key_press() {
        use egui::Key as E;
        let mut keys = KeyPresses::new();
        let ctrl = egui::Modifiers::CTRL;
        let mut input = input(vec![
            key(E::ShiftLeft, true, egui::Modifiers::SHIFT),
            key(E::Escape, true, egui::Modifiers::SHIFT),
            Event::Text("l".to_owned()),
            key(E::L, true, ctrl),
            key(E::Escape, false, NONE),
            Event::PointerGone,
        ]);
        // A modifier on its own isn't a chord, and only the first chord counts.
        assert_eq!(keys.capture(&mut input), named("Shift+Escape"));
        assert_eq!(
            input.events,
            [key(E::Escape, false, NONE), Event::PointerGone]
        );

        // L is still held, so its repeats go nowhere, captured or not.
        let mut input = self::input(vec![key(E::L, true, ctrl)]);
        assert_eq!(keys.capture(&mut input), None);
        assert_eq!(input.events, []);
        let ctx = egui::Context::default();
        let mut input = self::input(vec![key(E::L, true, ctrl)]);
        keys.take(&ctx, &mut input, |chord, _| {
            panic!("{chord} went to the keymap")
        });
        assert_eq!(input.events, []);

        // Once it's let go, it's a chord again.
        let mut input = self::input(vec![key(E::L, false, ctrl), key(E::L, true, ctrl)]);
        assert_eq!(keys.capture(&mut input), named("Ctrl+L"));
        assert_eq!(input.events, [key(E::L, false, ctrl)]);
    }

    #[test]
    fn a_text_field_with_focus_gets_every_key() {
        use egui::Key as E;
        let ctx = egui::Context::default();
        let mut text = String::new();
        let mut frame = |input: RawInput| {
            let mut output = ctx.run_ui(input, |ui| {
                ui.text_edit_singleline(&mut text).request_focus();
            });
            // No renderer takes the font textures.
            output.textures_delta.clear();
            output
        };
        frame(RawInput::default());
        frame(RawInput::default());
        assert!(ctx.text_edit_focused());

        let mut keys = KeyPresses::new();
        // Ctrl as egui reports it on Windows and Linux: as the command key.
        let ctrl = egui::Modifiers::CTRL | egui::Modifiers::COMMAND;
        let space = key(E::Space, true, NONE);
        let mut input = input(vec![
            space.clone(),
            Event::Text(" ".to_owned()),
            key(E::C, true, ctrl),
            key(E::V, true, ctrl),
        ]);
        keys.take(&ctx, &mut input, |chord, _| {
            panic!("{chord} went to the keymap");
        });
        // Ctrl+C is egui's copy. Ctrl+V asks the system for the clipboard,
        // which comes back as a paste event.
        assert_eq!(
            input.events,
            [space, Event::Text(" ".to_owned()), Event::Copy]
        );
        let output = frame(input);
        assert_eq!(text, " ");
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(
            commands.contains(&ViewportCommand::RequestPaste),
            "{commands:?}"
        );
    }
}
