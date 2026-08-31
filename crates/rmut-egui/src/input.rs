//! egui's input as the front-end key vocabulary. Printable text
//! arrives as `Event::Text`; named keys and Ctrl/Alt chords as
//! `Event::Key`. The split keeps a key from landing twice.

use eframe::egui;
use rmut_front::{KeyCode, KeyEvent, KeyModifiers};

/// The presses in this frame, oldest first.
pub fn keys(events: &[egui::Event]) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    for event in events {
        match event {
            egui::Event::Text(text) => {
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
                } else if modifiers.ctrl || modifiers.alt {
                    // A chord letter never comes through Event::Text.
                    let name = key.name();
                    let mut chars = name.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        let c = if modifiers.shift {
                            c.to_ascii_uppercase()
                        } else {
                            c.to_ascii_lowercase()
                        };
                        out.push(KeyEvent::new(KeyCode::Char(c), mods));
                    }
                }
            }
            _ => {}
        }
    }
    out
}
