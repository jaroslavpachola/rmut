//! egui's input as the front-end key vocabulary. Printable text
//! arrives as `Event::Text`; named keys and Ctrl/Alt chords as
//! `Event::Key`. The split keeps a key from landing twice: egui-winit
//! already withholds the text of a Ctrl chord, but not of an Alt one,
//! so the text an Alt chord brings along is dropped here.

use eframe::egui;
use rmut_front::{KeyCode, KeyEvent, KeyModifiers};

/// The presses in this frame, oldest first.
pub fn keys(events: &[egui::Event]) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    // The character of the Alt chord just read: egui-winit follows it
    // with the same character as text, which would run the unmodified
    // key as well (Alt+v folding the thread, then v opening the
    // attachment menu). Only an exact match is dropped, so a layout
    // whose AltGr is reported as Alt keeps the character it typed.
    let mut chord_text: Option<char> = None;
    for event in events {
        match event {
            egui::Event::Text(text) => {
                let mut chars = text.chars();
                if let (Some(c), None) = (chars.next(), chars.next())
                    && chord_text.take() == Some(c)
                {
                    continue;
                }
                for c in text.chars() {
                    out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                }
            }
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                chord_text = None;
                let mut mods = KeyModifiers::NONE;
                if modifiers.ctrl {
                    mods |= KeyModifiers::CONTROL;
                }
                if modifiers.alt {
                    mods |= KeyModifiers::ALT;
                }
                if modifiers.shift {
                    mods |= KeyModifiers::SHIFT;
                }
                let named = match key {
                    egui::Key::Enter => Some(KeyCode::Enter),
                    egui::Key::Escape => Some(KeyCode::Esc),
                    egui::Key::Tab => Some(KeyCode::Tab),
                    egui::Key::Backspace => Some(KeyCode::Backspace),
                    egui::Key::Delete => Some(KeyCode::Delete),
                    egui::Key::ArrowUp => Some(KeyCode::Up),
                    egui::Key::ArrowDown => Some(KeyCode::Down),
                    egui::Key::ArrowLeft => Some(KeyCode::Left),
                    egui::Key::ArrowRight => Some(KeyCode::Right),
                    egui::Key::PageUp => Some(KeyCode::PageUp),
                    egui::Key::PageDown => Some(KeyCode::PageDown),
                    egui::Key::Home => Some(KeyCode::Home),
                    egui::Key::End => Some(KeyCode::End),
                    _ => None,
                };
                if let Some(code) = named {
                    out.push(KeyEvent::new(code, mods));
                } else if (modifiers.ctrl || modifiers.alt)
                    && let Some(c) = chord_char(*key, modifiers.shift)
                {
                    out.push(KeyEvent::new(KeyCode::Char(c), mods));
                    if modifiers.alt && !modifiers.ctrl {
                        chord_text = Some(c);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The character a chord's key stands for. egui names punctuation in
/// words (`Key::Slash` is "Slash"), so the name only serves letters
/// and digits; the symbol is the character itself, except that egui
/// gives the minus key as U+2212, the typographic minus.
fn chord_char(key: egui::Key, shift: bool) -> Option<char> {
    if key == egui::Key::Minus {
        return Some('-');
    }
    let symbol = key.symbol_or_name();
    let mut chars = symbol.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    Some(if !c.is_ascii_alphabetic() {
        c
    } else if shift {
        c.to_ascii_uppercase()
    } else {
        c.to_ascii_lowercase()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn text(s: &str) -> egui::Event {
        egui::Event::Text(s.into())
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn alt_punctuation_is_a_chord() {
        // egui-winit sends the chord and then its character as text.
        let got = keys(&[press(egui::Key::Slash, egui::Modifiers::ALT), text("/")]);
        assert_eq!(got, vec![alt('/')]);
        let got = keys(&[press(egui::Key::Minus, egui::Modifiers::ALT), text("-")]);
        assert_eq!(got, vec![alt('-')]);
        let got = keys(&[press(egui::Key::Comma, egui::Modifiers::ALT), text(",")]);
        assert_eq!(got, vec![alt(',')]);
    }

    #[test]
    fn an_alt_letter_lands_once() {
        let got = keys(&[press(egui::Key::V, egui::Modifiers::ALT), text("v")]);
        assert_eq!(got, vec![alt('v')]);
        let shifted = egui::Modifiers::ALT | egui::Modifiers::SHIFT;
        let got = keys(&[press(egui::Key::V, shifted), text("V")]);
        assert_eq!(
            got,
            vec![KeyEvent::new(
                KeyCode::Char('V'),
                KeyModifiers::ALT | KeyModifiers::SHIFT
            )]
        );
    }

    #[test]
    fn only_the_chords_own_text_is_dropped() {
        // No text after the chord: the next press's text stays.
        let got = keys(&[
            press(egui::Key::Slash, egui::Modifiers::ALT),
            press(egui::Key::V, egui::Modifiers::NONE),
            text("v"),
        ]);
        assert_eq!(got, vec![alt('/'), plain('v')]);
        // Text that is not the chord's character (AltGr reported as
        // Alt, say) is typed as it is.
        let got = keys(&[press(egui::Key::V, egui::Modifiers::ALT), text("@")]);
        assert_eq!(got, vec![alt('v'), plain('@')]);
        // A Ctrl chord: egui-winit sends no text, and nothing is held.
        let got = keys(&[press(egui::Key::R, egui::Modifiers::CTRL), text("r")]);
        assert_eq!(
            got,
            vec![
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                plain('r')
            ]
        );
    }

    #[test]
    fn plain_text_and_named_keys_pass_through() {
        let got = keys(&[
            text("ab"),
            press(egui::Key::Enter, egui::Modifiers::NONE),
            press(egui::Key::Escape, egui::Modifiers::NONE),
            text("/"),
        ]);
        assert_eq!(
            got,
            vec![
                plain('a'),
                plain('b'),
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                plain('/'),
            ]
        );
    }
}
