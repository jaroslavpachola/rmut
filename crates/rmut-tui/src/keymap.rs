//! Default keybindings plus config remaps ([keys.index] / [keys.pager],
//! `action = "key"`). Key syntax: a single character, or `ctrl+x` /
//! `alt+x`, or a name: enter, esc, space, tab, backspace, up, down,
//! pgup, pgdn, home, end.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyPattern {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyPattern {
    fn plain(code: KeyCode) -> Self {
        KeyPattern {
            code,
            mods: KeyModifiers::NONE,
        }
    }

    fn ch(c: char) -> Self {
        Self::plain(KeyCode::Char(c))
    }

    fn ctrl(c: char) -> Self {
        KeyPattern {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }

    fn alt(c: char) -> Self {
        KeyPattern {
            code: KeyCode::Char(c),
            mods: KeyModifiers::ALT,
        }
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        self.code == key.code
            && key.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT) == self.mods
    }

    pub fn display(&self) -> String {
        let base = match self.code {
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            other => format!("{other:?}"),
        };
        if self.mods.contains(KeyModifiers::CONTROL) {
            format!("Ctrl+{base}")
        } else if self.mods.contains(KeyModifiers::ALT) {
            format!("Alt+{base}")
        } else {
            base
        }
    }
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

pub fn parse_key(input: &str) -> Option<KeyPattern> {
    let mut mods = KeyModifiers::NONE;
    let mut rest = input.trim();
    loop {
        if let Some(r) = strip_ci(rest, "ctrl+") {
            mods |= KeyModifiers::CONTROL;
            rest = r;
        } else if let Some(r) = strip_ci(rest, "alt+") {
            mods |= KeyModifiers::ALT;
            rest = r;
        } else {
            break;
        }
    }
    let code = match rest.to_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        _ => {
            let mut chars = rest.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    Some(KeyPattern { code, mods })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexAction {
    Quit,
    Abort,
    Down,
    Up,
    PageDown,
    PageUp,
    First,
    Last,
    View,
    Delete,
    Undelete,
    Flag,
    ToggleNew,
    Sync,
    Compose,
    Reply,
    GroupReply,
    Forward,
    Sort,
    Limit,
    Search,
    SearchNext,
    ChangeMailbox,
    Folders,
    Attachments,
    FoldThread,
    FoldAll,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagerAction {
    Back,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    NextMsg,
    PrevMsg,
    Delete,
    Headers,
    Attachments,
    Compose,
    Reply,
    GroupReply,
    Forward,
    Help,
}

impl IndexAction {
    pub fn name(self) -> &'static str {
        use IndexAction::*;
        match self {
            Quit => "quit",
            Abort => "abort",
            Down => "down",
            Up => "up",
            PageDown => "page-down",
            PageUp => "page-up",
            First => "first",
            Last => "last",
            View => "view",
            Delete => "delete",
            Undelete => "undelete",
            Flag => "flag",
            ToggleNew => "toggle-new",
            Sync => "sync",
            Compose => "compose",
            Reply => "reply",
            GroupReply => "group-reply",
            Forward => "forward",
            Sort => "sort",
            Limit => "limit",
            Search => "search",
            SearchNext => "search-next",
            ChangeMailbox => "change-mailbox",
            Folders => "folders",
            Attachments => "attachments",
            FoldThread => "fold-thread",
            FoldAll => "fold-all",
            Help => "help",
        }
    }

    pub fn describe(self) -> &'static str {
        use IndexAction::*;
        match self {
            Quit => "quit (asks about pending changes)",
            Abort => "quit without saving changes",
            Down => "next message",
            Up => "previous message",
            PageDown => "page down",
            PageUp => "page up",
            First => "first message",
            Last => "last message",
            View => "view message",
            Delete => "mark for deletion",
            Undelete => "unmark deletion",
            Flag => "toggle flagged mark",
            ToggleNew => "toggle read/unread",
            Sync => "write changes to the maildir",
            Compose => "compose a new message",
            Reply => "reply to sender",
            GroupReply => "reply to all",
            Forward => "forward message",
            Sort => "choose sort order",
            Limit => "limit index by pattern",
            Search => "search by pattern",
            SearchNext => "repeat last search",
            ChangeMailbox => "open a mailbox by path",
            Folders => "browse nearby mailboxes",
            Attachments => "list message parts",
            FoldThread => "fold/unfold current thread",
            FoldAll => "fold/unfold all threads",
            Help => "this help",
        }
    }

    fn all() -> &'static [IndexAction] {
        use IndexAction::*;
        &[
            Quit,
            Abort,
            Down,
            Up,
            PageDown,
            PageUp,
            First,
            Last,
            View,
            Delete,
            Undelete,
            Flag,
            ToggleNew,
            Sync,
            Compose,
            Reply,
            GroupReply,
            Forward,
            Sort,
            Limit,
            Search,
            SearchNext,
            ChangeMailbox,
            Folders,
            Attachments,
            FoldThread,
            FoldAll,
            Help,
        ]
    }

    fn from_name(name: &str) -> Option<IndexAction> {
        IndexAction::all()
            .iter()
            .copied()
            .find(|a| a.name() == name)
    }
}

impl PagerAction {
    pub fn name(self) -> &'static str {
        use PagerAction::*;
        match self {
            Back => "back",
            Down => "down",
            Up => "up",
            PageDown => "page-down",
            PageUp => "page-up",
            Top => "top",
            Bottom => "bottom",
            NextMsg => "next",
            PrevMsg => "previous",
            Delete => "delete",
            Headers => "headers",
            Attachments => "attachments",
            Compose => "compose",
            Reply => "reply",
            GroupReply => "group-reply",
            Forward => "forward",
            Help => "help",
        }
    }

    pub fn describe(self) -> &'static str {
        use PagerAction::*;
        match self {
            Back => "back to the index",
            Down => "scroll down one line",
            Up => "scroll up one line",
            PageDown => "page down",
            PageUp => "page up",
            Top => "jump to the top",
            Bottom => "jump to the bottom",
            NextMsg => "open next message",
            PrevMsg => "open previous message",
            Delete => "delete and advance",
            Headers => "toggle full headers",
            Attachments => "list message parts",
            Compose => "compose a new message",
            Reply => "reply to sender",
            GroupReply => "reply to all",
            Forward => "forward message",
            Help => "this help",
        }
    }

    fn all() -> &'static [PagerAction] {
        use PagerAction::*;
        &[
            Back,
            Down,
            Up,
            PageDown,
            PageUp,
            Top,
            Bottom,
            NextMsg,
            PrevMsg,
            Delete,
            Headers,
            Attachments,
            Compose,
            Reply,
            GroupReply,
            Forward,
            Help,
        ]
    }

    fn from_name(name: &str) -> Option<PagerAction> {
        PagerAction::all()
            .iter()
            .copied()
            .find(|a| a.name() == name)
    }
}

pub struct Keymap {
    pub index: Vec<(KeyPattern, IndexAction)>,
    pub pager: Vec<(KeyPattern, PagerAction)>,
}

fn index_defaults() -> Vec<(KeyPattern, IndexAction)> {
    use IndexAction::*;
    use KeyCode as K;
    vec![
        (KeyPattern::ch('q'), Quit),
        (KeyPattern::ch('x'), Abort),
        (KeyPattern::ch('j'), Down),
        (KeyPattern::plain(K::Down), Down),
        (KeyPattern::ch('k'), Up),
        (KeyPattern::plain(K::Up), Up),
        (KeyPattern::plain(K::PageDown), PageDown),
        (KeyPattern::ctrl('f'), PageDown),
        (KeyPattern::plain(K::PageUp), PageUp),
        (KeyPattern::ctrl('b'), PageUp),
        (KeyPattern::ch('='), First),
        (KeyPattern::plain(K::Home), First),
        (KeyPattern::ch('*'), Last),
        (KeyPattern::plain(K::End), Last),
        (KeyPattern::plain(K::Enter), View),
        (KeyPattern::ch('d'), Delete),
        (KeyPattern::ch('u'), Undelete),
        (KeyPattern::ch('F'), Flag),
        (KeyPattern::ch('N'), ToggleNew),
        (KeyPattern::ch('$'), Sync),
        (KeyPattern::ch('m'), Compose),
        (KeyPattern::ch('r'), Reply),
        (KeyPattern::ch('g'), GroupReply),
        (KeyPattern::ch('f'), Forward),
        (KeyPattern::ch('o'), Sort),
        (KeyPattern::ch('l'), Limit),
        (KeyPattern::ch('/'), Search),
        (KeyPattern::ch('n'), SearchNext),
        (KeyPattern::ch('c'), ChangeMailbox),
        (KeyPattern::ch('y'), Folders),
        (KeyPattern::ch('v'), Attachments),
        (KeyPattern::alt('v'), FoldThread),
        (KeyPattern::alt('V'), FoldAll),
        (KeyPattern::ch('?'), Help),
    ]
}

fn pager_defaults() -> Vec<(KeyPattern, PagerAction)> {
    use KeyCode as K;
    use PagerAction::*;
    vec![
        (KeyPattern::ch('q'), Back),
        (KeyPattern::ch('i'), Back),
        (KeyPattern::plain(K::Esc), Back),
        (KeyPattern::ch('j'), Down),
        (KeyPattern::plain(K::Down), Down),
        (KeyPattern::plain(K::Enter), Down),
        (KeyPattern::ch('k'), Up),
        (KeyPattern::plain(K::Up), Up),
        (KeyPattern::plain(K::Backspace), Up),
        (KeyPattern::ch(' '), PageDown),
        (KeyPattern::plain(K::PageDown), PageDown),
        (KeyPattern::ch('-'), PageUp),
        (KeyPattern::plain(K::PageUp), PageUp),
        (KeyPattern::plain(K::Home), Top),
        (KeyPattern::plain(K::End), Bottom),
        (KeyPattern::ch('J'), NextMsg),
        (KeyPattern::ch('K'), PrevMsg),
        (KeyPattern::ch('d'), Delete),
        (KeyPattern::ch('h'), Headers),
        (KeyPattern::ch('v'), Attachments),
        (KeyPattern::ch('m'), Compose),
        (KeyPattern::ch('r'), Reply),
        (KeyPattern::ch('g'), GroupReply),
        (KeyPattern::ch('f'), Forward),
        (KeyPattern::ch('?'), Help),
    ]
}

impl Keymap {
    /// Defaults with config remaps applied: a remap unbinds the action's
    /// default keys and whatever the new key was bound to.
    pub fn with_config(
        index_over: &HashMap<String, String>,
        pager_over: &HashMap<String, String>,
    ) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut index = index_defaults();
        for (action_name, key_str) in index_over {
            let (Some(action), Some(key)) =
                (IndexAction::from_name(action_name), parse_key(key_str))
            else {
                warnings.push(format!("bad index binding {action_name} = {key_str:?}"));
                continue;
            };
            index.retain(|(k, a)| *a != action && *k != key);
            index.push((key, action));
        }
        let mut pager = pager_defaults();
        for (action_name, key_str) in pager_over {
            let (Some(action), Some(key)) =
                (PagerAction::from_name(action_name), parse_key(key_str))
            else {
                warnings.push(format!("bad pager binding {action_name} = {key_str:?}"));
                continue;
            };
            pager.retain(|(k, a)| *a != action && *k != key);
            pager.push((key, action));
        }
        (Keymap { index, pager }, warnings)
    }

    pub fn lookup_index(&self, key: &KeyEvent) -> Option<IndexAction> {
        self.index
            .iter()
            .find(|(p, _)| p.matches(key))
            .map(|&(_, a)| a)
    }

    pub fn lookup_pager(&self, key: &KeyEvent) -> Option<PagerAction> {
        self.pager
            .iter()
            .find(|(p, _)| p.matches(key))
            .map(|&(_, a)| a)
    }

    /// Lines for the help screen, grouped and ordered by action.
    pub fn help_lines(&self) -> Vec<String> {
        let mut lines = vec!["Index keys".to_string(), String::new()];
        for &action in IndexAction::all() {
            let keys: Vec<String> = self
                .index
                .iter()
                .filter(|&&(_, a)| a == action)
                .map(|(k, _)| k.display())
                .collect();
            if !keys.is_empty() {
                lines.push(format!("  {:<16} {}", keys.join(" "), action.describe()));
            }
        }
        lines.extend([String::new(), "Pager keys".to_string(), String::new()]);
        for &action in PagerAction::all() {
            let keys: Vec<String> = self
                .pager
                .iter()
                .filter(|&&(_, a)| a == action)
                .map(|(k, _)| k.display())
                .collect();
            if !keys.is_empty() {
                lines.push(format!("  {:<16} {}", keys.join(" "), action.describe()));
            }
        }
        lines.extend(
            [
                "",
                "Patterns (limit/search)",
                "",
                "  ~f x   from contains x        ~N  new",
                "  ~s x   subject contains x     ~F  flagged",
                "  ~b x   body contains x        ~D  marked deleted",
                "  word   subject or from        ~U  unread",
                "  Terms AND together.",
            ]
            .map(String::from),
        );
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_forms() {
        assert_eq!(parse_key("x"), Some(KeyPattern::ch('x')));
        assert_eq!(parse_key("X"), Some(KeyPattern::ch('X')));
        assert_eq!(parse_key("ctrl+f"), Some(KeyPattern::ctrl('f')));
        assert_eq!(parse_key("Alt+v"), Some(KeyPattern::alt('v')));
        assert_eq!(parse_key("space"), Some(KeyPattern::ch(' ')));
        assert_eq!(
            parse_key("pgdn"),
            Some(KeyPattern::plain(KeyCode::PageDown))
        );
        assert_eq!(parse_key("enter"), Some(KeyPattern::plain(KeyCode::Enter)));
        assert!(parse_key("bogus-key").is_none());
    }

    #[test]
    fn remap_replaces_defaults_and_conflicts() {
        let mut over = HashMap::new();
        over.insert("sync".to_string(), "w".to_string());
        over.insert("delete".to_string(), "ctrl+d".to_string());
        let (map, warnings) = Keymap::with_config(&over, &HashMap::new());
        assert!(warnings.is_empty());
        let ev = |p: KeyPattern| KeyEvent::new(p.code, p.mods);
        assert_eq!(
            map.lookup_index(&ev(KeyPattern::ch('w'))),
            Some(IndexAction::Sync)
        );
        assert_eq!(map.lookup_index(&ev(KeyPattern::ch('$'))), None);
        assert_eq!(
            map.lookup_index(&ev(KeyPattern::ctrl('d'))),
            Some(IndexAction::Delete)
        );
        assert_eq!(map.lookup_index(&ev(KeyPattern::ch('d'))), None);
    }

    #[test]
    fn bad_bindings_warn_and_keep_defaults() {
        let mut over = HashMap::new();
        over.insert("frobnicate".to_string(), "z".to_string());
        let (map, warnings) = Keymap::with_config(&over, &HashMap::new());
        assert_eq!(warnings.len(), 1);
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(map.lookup_index(&ev), Some(IndexAction::Quit));
    }

    #[test]
    fn shift_in_event_does_not_block_match() {
        // Terminals report 'F' as Char('F') + SHIFT.
        let ev = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT);
        let (map, _) = Keymap::with_config(&HashMap::new(), &HashMap::new());
        assert_eq!(map.lookup_index(&ev), Some(IndexAction::Flag));
    }
}
