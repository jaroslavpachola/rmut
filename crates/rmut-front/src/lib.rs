//! What every rmut front end needs and no toolkit provides: the key
//! vocabulary, the keymap over [`rmut_session::Function`], and (as
//! the rounds move it here) the layout and formatting that turn a
//! session into rows of text. Nothing in here draws.

pub mod editor;
pub mod key;
pub mod keymap;
pub mod pager;
pub mod status;
pub mod style;
pub mod theme;

pub use key::{KeyCode, KeyEvent, KeyModifiers};
pub use keymap::{KeyPattern, Keymap, PagerAction, parse_key, parse_sequence};
