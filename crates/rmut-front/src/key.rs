//! Keys as a front end reports them, with no toolkit's type in the
//! signature. Shaped like crossterm's so the terminal front end maps
//! one to the other at its read site and nothing else notices.

/// Which key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    /// Anything a front end reports that rmut has no name for. Never
    /// bound, never matched.
    Other,
}

/// Which modifiers were held: a set of the three rmut cares about.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    pub const NONE: KeyModifiers = KeyModifiers(0);
    pub const SHIFT: KeyModifiers = KeyModifiers(1);
    pub const CONTROL: KeyModifiers = KeyModifiers(2);
    pub const ALT: KeyModifiers = KeyModifiers(4);

    pub fn contains(self, other: KeyModifiers) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = KeyModifiers;
    fn bitor(self, rhs: KeyModifiers) -> KeyModifiers {
        KeyModifiers(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: KeyModifiers) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for KeyModifiers {
    type Output = KeyModifiers;
    fn bitand(self, rhs: KeyModifiers) -> KeyModifiers {
        KeyModifiers(self.0 & rhs.0)
    }
}

/// One key press.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent { code, modifiers }
    }
}

/// mutt's Esc: the first key of a two-key sequence rather than a key
/// of its own. mutt spells every Alt binding `Esc x`, and its keymap
/// waits after an Esc for the key that finishes the sequence
/// (`km_dokey`), so Esc, a pause, then `/` is search-reverse there
/// however slowly it is typed. A terminal sends Alt+/ as the same two
/// bytes, but only when they arrive together does the key reader
/// merge them; typed apart they reached rmut as a bare Esc and a
/// plain `/`. This merges them again: an Esc is held, and the next
/// key comes out with Alt added. A second Esc gives the Esc itself
/// back, so whatever Esc is bound to on its own still has a spelling.
#[derive(Default, Debug)]
pub struct EscPrefix {
    pending: bool,
}

impl EscPrefix {
    /// The key to dispatch, or None while an Esc waits for its second
    /// half.
    pub fn apply(&mut self, key: KeyEvent) -> Option<KeyEvent> {
        let bare_esc = key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE;
        if std::mem::take(&mut self.pending) {
            if bare_esc {
                return Some(key);
            }
            return Some(KeyEvent::new(key.code, key.modifiers | KeyModifiers::ALT));
        }
        if bare_esc {
            self.pending = true;
            return None;
        }
        Some(key)
    }

    /// Drop a held Esc: the menu it was typed in is gone.
    pub fn clear(&mut self) {
        self.pending = false;
    }

    pub fn is_pending(&self) -> bool {
        self.pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_sets() {
        let m = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert!(m.contains(KeyModifiers::CONTROL));
        assert!(m.contains(KeyModifiers::SHIFT));
        assert!(!m.contains(KeyModifiers::ALT));
        assert!(m.contains(KeyModifiers::NONE));
        assert_eq!(
            m & (KeyModifiers::CONTROL | KeyModifiers::ALT),
            KeyModifiers::CONTROL
        );
        let mut n = KeyModifiers::NONE;
        n |= KeyModifiers::ALT;
        assert_eq!(n, KeyModifiers::ALT);
    }

    #[test]
    fn esc_then_a_key_is_that_key_with_alt() {
        let mut esc = EscPrefix::default();
        let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let bare_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(esc.apply(bare_esc), None);
        assert!(esc.is_pending());
        assert_eq!(esc.apply(plain('/')), Some(alt('/')));
        // Only the one key after it: the next one is plain again.
        assert_eq!(esc.apply(plain('/')), Some(plain('/')));
        // Already Alt stays Alt.
        assert_eq!(esc.apply(alt('/')), Some(alt('/')));
        // Esc Esc is the Esc itself.
        assert_eq!(esc.apply(bare_esc), None);
        assert_eq!(esc.apply(bare_esc), Some(bare_esc));
        // A held Esc can be dropped.
        assert_eq!(esc.apply(bare_esc), None);
        esc.clear();
        assert_eq!(esc.apply(plain('a')), Some(plain('a')));
        // Tab keeps its own modifiers and gains Alt (mutt's Esc Tab).
        esc.apply(bare_esc);
        assert_eq!(
            esc.apply(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT))
        );
    }
}
