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
}
