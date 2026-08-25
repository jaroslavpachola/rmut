//! Default keybindings plus config remaps ([keys.index] / [keys.pager],
//! `action = "key"`). Key syntax: a single character, or `ctrl+x` /
//! `alt+x`, or a name: enter, esc, space, tab, backspace, up, down,
//! left, right, pgup, pgdn, home, end.

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
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
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
    ListReply,
    Forward,
    Sort,
    Limit,
    Search,
    SearchNext,
    NextNew,
    PrevNew,
    ChangeMailbox,
    Folders,
    Attachments,
    FoldThread,
    FoldAll,
    Print,
    Tag,
    TagPrefix,
    Undo,
    DeletePattern,
    UndeletePattern,
    TagPattern,
    UntagPattern,
    FetchMail,
    Save,
    Copy,
    Pipe,
    Bounce,
    Resend,
    Edit,
    SidebarToggle,
    SidebarNext,
    SidebarPrev,
    SidebarOpen,
    CreateAlias,
    Query,
    Notmuch,
    EnterCommand,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagerAction {
    Back,
    Down,
    Up,
    PageDown,
    PageUp,
    HalfDown,
    HalfUp,
    Top,
    Bottom,
    ToggleQuoted,
    SkipQuoted,
    NextMsg,
    PrevMsg,
    NextUndeleted,
    PrevUndeleted,
    Delete,
    Headers,
    Search,
    SearchNext,
    SearchPrev,
    Attachments,
    Compose,
    Reply,
    GroupReply,
    ListReply,
    Forward,
    Print,
    Save,
    Copy,
    Pipe,
    Bounce,
    Resend,
    Edit,
    CreateAlias,
    EnterCommand,
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
            ListReply => "list-reply",
            Forward => "forward",
            Sort => "sort",
            Limit => "limit",
            Search => "search",
            SearchNext => "search-next",
            NextNew => "next-new",
            PrevNew => "previous-new",
            ChangeMailbox => "change-mailbox",
            Folders => "folders",
            Attachments => "attachments",
            FoldThread => "fold-thread",
            FoldAll => "fold-all",
            Print => "print",
            Tag => "tag",
            TagPrefix => "tag-prefix",
            Undo => "undo",
            DeletePattern => "delete-pattern",
            UndeletePattern => "undelete-pattern",
            TagPattern => "tag-pattern",
            UntagPattern => "untag-pattern",
            FetchMail => "fetch-mail",
            Save => "save",
            Copy => "copy",
            Pipe => "pipe",
            Bounce => "bounce",
            Resend => "resend",
            Edit => "edit",
            SidebarToggle => "sidebar-toggle",
            SidebarNext => "sidebar-next",
            SidebarPrev => "sidebar-prev",
            SidebarOpen => "sidebar-open",
            CreateAlias => "create-alias",
            Query => "query",
            Notmuch => "notmuch",
            EnterCommand => "enter-command",
            Help => "help",
        }
    }

    pub fn describe(self) -> &'static str {
        use IndexAction::*;
        match self {
            Quit => "quit (writes changes; asks before purging deletions)",
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
            ListReply => "reply to the mailing list only",
            Forward => "forward message",
            Sort => "choose sort order",
            Limit => "limit index by pattern",
            Search => "search messages by pattern (the pager / searches its text)",
            SearchNext => "repeat last search",
            NextNew => "jump to the next new or unread message",
            PrevNew => "jump to the previous new or unread message",
            ChangeMailbox => "open a mailbox by path",
            Folders => "browse nearby mailboxes",
            Attachments => "list message parts",
            FoldThread => "fold/unfold current thread",
            FoldAll => "fold/unfold all threads",
            Print => "pipe message to the print command",
            Tag => "toggle the tag on this message",
            TagPrefix => "apply the next function to tagged messages",
            Undo => "undo the last delete/flag/tag/save (until changes are written)",
            DeletePattern => "delete every message matching a pattern",
            UndeletePattern => "undelete every message matching a pattern",
            TagPattern => "tag every message matching a pattern",
            UntagPattern => "untag every message matching a pattern",
            FetchMail => "check for new mail now",
            Save => "save (copy + mark deleted) to a mailbox",
            Copy => "copy to a mailbox (original stays)",
            Pipe => "pipe raw message to a shell command",
            Bounce => "bounce (resend) message to new recipients",
            Resend => "edit the message as a new draft",
            Edit => "edit the raw message and replace it",
            SidebarToggle => "show/hide the mailbox sidebar",
            SidebarNext => "highlight the next sidebar mailbox",
            SidebarPrev => "highlight the previous sidebar mailbox",
            SidebarOpen => "open the highlighted sidebar mailbox",
            CreateAlias => "add the sender to the alias file",
            Query => "look up addresses with query_command",
            Notmuch => "notmuch search into a read-only view",
            EnterCommand => "run a config command (set/bind/macro/color/...)",
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
            ListReply,
            Forward,
            Sort,
            Limit,
            Search,
            SearchNext,
            NextNew,
            PrevNew,
            ChangeMailbox,
            Folders,
            Attachments,
            FoldThread,
            FoldAll,
            Print,
            Tag,
            TagPrefix,
            Undo,
            DeletePattern,
            UndeletePattern,
            TagPattern,
            UntagPattern,
            FetchMail,
            Save,
            Copy,
            Pipe,
            Bounce,
            Resend,
            Edit,
            SidebarToggle,
            SidebarNext,
            SidebarPrev,
            SidebarOpen,
            CreateAlias,
            Query,
            Notmuch,
            EnterCommand,
            Help,
        ]
    }

    pub fn from_name(name: &str) -> Option<IndexAction> {
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
            HalfDown => "half-down",
            HalfUp => "half-up",
            Top => "top",
            Bottom => "bottom",
            ToggleQuoted => "toggle-quoted",
            SkipQuoted => "skip-quoted",
            NextMsg => "next",
            PrevMsg => "previous",
            NextUndeleted => "next-undeleted",
            PrevUndeleted => "previous-undeleted",
            Delete => "delete",
            Headers => "headers",
            Search => "search",
            SearchNext => "search-next",
            SearchPrev => "search-prev",
            Attachments => "attachments",
            Compose => "compose",
            Reply => "reply",
            GroupReply => "group-reply",
            ListReply => "list-reply",
            Forward => "forward",
            Print => "print",
            Save => "save",
            Copy => "copy",
            Pipe => "pipe",
            Bounce => "bounce",
            Resend => "resend",
            Edit => "edit",
            CreateAlias => "create-alias",
            EnterCommand => "enter-command",
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
            HalfDown => "scroll down half a page",
            HalfUp => "scroll up half a page",
            Top => "jump to the top",
            Bottom => "jump to the bottom",
            ToggleQuoted => "show/hide quoted text",
            SkipQuoted => "skip past the quoted text below",
            NextMsg => "open next message",
            PrevMsg => "open previous message",
            NextUndeleted => "open next undeleted message",
            PrevUndeleted => "open previous undeleted message",
            Delete => "delete and advance",
            Headers => "toggle full headers",
            Search => "search the displayed text (unlike the index /, which matches messages)",
            SearchNext => "next match of the pager search",
            SearchPrev => "previous match of the pager search",
            Attachments => "list message parts",
            Compose => "compose a new message",
            Reply => "reply to sender",
            GroupReply => "reply to all",
            ListReply => "reply to the mailing list only",
            Forward => "forward message",
            Print => "pipe message to the print command",
            Save => "save (copy + mark deleted) to a mailbox",
            Copy => "copy to a mailbox (original stays)",
            Pipe => "pipe raw message to a shell command",
            Bounce => "bounce (resend) message to new recipients",
            Resend => "edit the message as a new draft",
            Edit => "edit the raw message and replace it",
            CreateAlias => "add the sender to the alias file",
            EnterCommand => "run a config command (set/bind/macro/color/...)",
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
            HalfDown,
            HalfUp,
            Top,
            Bottom,
            ToggleQuoted,
            SkipQuoted,
            NextMsg,
            PrevMsg,
            NextUndeleted,
            PrevUndeleted,
            Delete,
            Headers,
            Search,
            SearchNext,
            SearchPrev,
            Attachments,
            Compose,
            Reply,
            GroupReply,
            ListReply,
            Forward,
            Print,
            Save,
            Copy,
            Pipe,
            Bounce,
            Resend,
            Edit,
            CreateAlias,
            EnterCommand,
            Help,
        ]
    }

    pub fn from_name(name: &str) -> Option<PagerAction> {
        PagerAction::all()
            .iter()
            .copied()
            .find(|a| a.name() == name)
    }
}

/// A key sequence for macros: literal characters plus key names in
/// angle brackets (`<enter>`, `<esc>`, `<ctrl+x>`, everything
/// `parse_key` accepts). None on an unknown name or an unclosed `<`.
pub fn parse_sequence(input: &str) -> Option<Vec<KeyEvent>> {
    let mut out = Vec::new();
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('>') => break,
                    Some(c) => name.push(c),
                    None => return None,
                }
            }
            let p = parse_key(&name)?;
            out.push(KeyEvent::new(p.code, p.mods));
        } else {
            out.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }
    Some(out)
}

pub struct Keymap {
    pub index: Vec<(KeyPattern, IndexAction)>,
    pub pager: Vec<(KeyPattern, PagerAction)>,
    /// Macros: trigger → (replayed events, the sequence as written,
    /// kept for the help screen). Checked before the action bindings,
    /// so a macro shadows a binding on the same key (like mutt).
    pub macros_index: Vec<(KeyPattern, Vec<KeyEvent>, String)>,
    pub macros_pager: Vec<(KeyPattern, Vec<KeyEvent>, String)>,
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
        (KeyPattern::ch('L'), ListReply),
        (KeyPattern::ch('f'), Forward),
        (KeyPattern::ch('o'), Sort),
        (KeyPattern::ch('l'), Limit),
        (KeyPattern::ch('/'), Search),
        (KeyPattern::ch('n'), SearchNext),
        (KeyPattern::plain(K::Tab), NextNew),
        (
            KeyPattern {
                code: K::Tab,
                mods: KeyModifiers::ALT,
            },
            PrevNew,
        ),
        (KeyPattern::ch('c'), ChangeMailbox),
        (KeyPattern::ch('y'), Folders),
        (KeyPattern::ch('v'), Attachments),
        (KeyPattern::alt('v'), FoldThread),
        (KeyPattern::alt('V'), FoldAll),
        (KeyPattern::ch('p'), Print),
        (KeyPattern::ch('t'), Tag),
        (KeyPattern::ch(';'), TagPrefix),
        (KeyPattern::ch('z'), Undo),
        (KeyPattern::ch('D'), DeletePattern),
        (KeyPattern::ch('U'), UndeletePattern),
        (KeyPattern::ch('T'), TagPattern),
        (KeyPattern::ctrl('t'), UntagPattern),
        (KeyPattern::ch('G'), FetchMail),
        (KeyPattern::ch('s'), Save),
        (KeyPattern::ch('C'), Copy),
        (KeyPattern::ch('|'), Pipe),
        (KeyPattern::ch('b'), Bounce),
        (KeyPattern::ch('e'), Edit),
        (KeyPattern::alt('e'), Resend),
        (KeyPattern::ch('B'), SidebarToggle),
        (KeyPattern::ctrl('n'), SidebarNext),
        (KeyPattern::ctrl('p'), SidebarPrev),
        (KeyPattern::ctrl('o'), SidebarOpen),
        (KeyPattern::ch('a'), CreateAlias),
        (KeyPattern::ch('Q'), Query),
        (KeyPattern::ch('X'), Notmuch),
        (KeyPattern::ch(':'), EnterCommand),
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
        // mutt's pager: Enter/Backspace scroll one line; j/k and the
        // arrows move between messages (next-/previous-undeleted).
        (KeyPattern::plain(K::Enter), Down),
        (KeyPattern::plain(K::Backspace), Up),
        (KeyPattern::ch('j'), NextUndeleted),
        (KeyPattern::plain(K::Down), NextUndeleted),
        (KeyPattern::plain(K::Right), NextUndeleted),
        (KeyPattern::ch('k'), PrevUndeleted),
        (KeyPattern::plain(K::Up), PrevUndeleted),
        (KeyPattern::plain(K::Left), PrevUndeleted),
        (KeyPattern::ch(' '), PageDown),
        (KeyPattern::plain(K::PageDown), PageDown),
        (KeyPattern::ch('-'), PageUp),
        (KeyPattern::plain(K::PageUp), PageUp),
        (KeyPattern::ctrl('d'), HalfDown),
        (KeyPattern::ctrl('u'), HalfUp),
        (KeyPattern::plain(K::Home), Top),
        (KeyPattern::plain(K::End), Bottom),
        (KeyPattern::ch('T'), ToggleQuoted),
        (KeyPattern::ch('S'), SkipQuoted),
        (KeyPattern::ch('J'), NextMsg),
        (KeyPattern::ch('K'), PrevMsg),
        (KeyPattern::ch('d'), Delete),
        (KeyPattern::ch('h'), Headers),
        (KeyPattern::ch('/'), Search),
        (KeyPattern::ch('n'), SearchNext),
        (KeyPattern::ch('N'), SearchPrev),
        (KeyPattern::ch('v'), Attachments),
        (KeyPattern::ch('m'), Compose),
        (KeyPattern::ch('r'), Reply),
        (KeyPattern::ch('g'), GroupReply),
        (KeyPattern::ch('L'), ListReply),
        (KeyPattern::ch('f'), Forward),
        (KeyPattern::ch('p'), Print),
        (KeyPattern::ch('s'), Save),
        (KeyPattern::ch('C'), Copy),
        (KeyPattern::ch('|'), Pipe),
        (KeyPattern::ch('b'), Bounce),
        (KeyPattern::ch('e'), Edit),
        (KeyPattern::alt('e'), Resend),
        (KeyPattern::ch('a'), CreateAlias),
        (KeyPattern::ch(':'), EnterCommand),
        (KeyPattern::ch('?'), Help),
    ]
}

impl Keymap {
    /// Defaults with config remaps applied: a remap unbinds the action's
    /// default keys and whatever the new key was bound to. Macros come
    /// from [macros.index]/[macros.pager], `key = "sequence"`.
    pub fn with_config(
        index_over: &HashMap<String, String>,
        pager_over: &HashMap<String, String>,
        macros_index: &HashMap<String, String>,
        macros_pager: &HashMap<String, String>,
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
        let mut macros = |table: &HashMap<String, String>, menu: &str| {
            let mut out = Vec::new();
            for (key_str, seq_str) in table {
                let (Some(key), Some(seq)) = (parse_key(key_str), parse_sequence(seq_str)) else {
                    warnings.push(format!("bad {menu} macro {key_str} = {seq_str:?}"));
                    continue;
                };
                out.push((key, seq, seq_str.clone()));
            }
            out
        };
        let macros_index = macros(macros_index, "index");
        let macros_pager = macros(macros_pager, "pager");
        (
            Keymap {
                index,
                pager,
                macros_index,
                macros_pager,
            },
            warnings,
        )
    }

    /// The macro sequence bound to this key, if any.
    pub fn lookup_index_macro(&self, key: &KeyEvent) -> Option<&[KeyEvent]> {
        self.macros_index
            .iter()
            .find(|(p, _, _)| p.matches(key))
            .map(|(_, seq, _)| seq.as_slice())
    }

    pub fn lookup_pager_macro(&self, key: &KeyEvent) -> Option<&[KeyEvent]> {
        self.macros_pager
            .iter()
            .find(|(p, _, _)| p.matches(key))
            .map(|(_, seq, _)| seq.as_slice())
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
        for (title, table) in [
            ("Index macros", &self.macros_index),
            ("Pager macros", &self.macros_pager),
        ] {
            if !table.is_empty() {
                lines.extend([String::new(), title.to_string(), String::new()]);
                for (key, _, raw) in table {
                    lines.push(format!("  {:<16} {raw}", key.display()));
                }
            }
        }
        lines.extend(
            [
                "",
                "Patterns (limit/search)",
                "",
                "  ~f x  from       ~s x  subject     ~b x  body",
                "  ~t x  to         ~c x  cc          ~C x  to or cc",
                "  ~e x  sender     ~d spec  date     word  subject or from",
                "  ~N new   ~U unread   ~F flagged   ~D deleted   ~T tagged",
                "  ~p addressed to me",
                "",
                "  x is a case-insensitive regex; \"quotes\" keep spaces.",
                "  ~d: 24/12/2026, 1/6/2026-30/6/2026, 24/12-, <1w, >2d, =3d",
                "  Terms AND; ! negates, | ORs, () groups:",
                "    !~D (~f jane | ~t jane) ~d <1m",
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
        let (map, warnings) =
            Keymap::with_config(&over, &HashMap::new(), &HashMap::new(), &HashMap::new());
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
        let (map, warnings) =
            Keymap::with_config(&over, &HashMap::new(), &HashMap::new(), &HashMap::new());
        assert_eq!(warnings.len(), 1);
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(map.lookup_index(&ev), Some(IndexAction::Quit));
    }

    #[test]
    fn parse_sequence_forms() {
        let seq = parse_sequence("l~f jane<enter>").unwrap();
        assert_eq!(seq.len(), 9);
        assert_eq!(seq[0].code, KeyCode::Char('l'));
        assert_eq!(seq[2].code, KeyCode::Char('f'));
        assert_eq!(seq[3].code, KeyCode::Char(' '));
        assert_eq!(seq[8].code, KeyCode::Enter);
        let seq = parse_sequence("<ctrl+x><Esc>").unwrap();
        assert_eq!(seq[0].code, KeyCode::Char('x'));
        assert!(seq[0].modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(seq[1].code, KeyCode::Esc);
        assert!(parse_sequence("<bogus>").is_none());
        assert!(parse_sequence("<unclosed").is_none());
        assert!(parse_sequence("").unwrap().is_empty());
    }

    #[test]
    fn macros_parse_shadow_and_warn() {
        let mut macros_index = HashMap::new();
        macros_index.insert("d".to_string(), "l~f jane<enter>".to_string());
        macros_index.insert("Z".to_string(), "<bogus>".to_string());
        let (map, warnings) = Keymap::with_config(
            &HashMap::new(),
            &HashMap::new(),
            &macros_index,
            &HashMap::new(),
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bad index macro Z"), "{warnings:?}");
        let ev = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        // The macro exists on d; the app checks it before the delete
        // binding, so it shadows.
        assert_eq!(map.lookup_index_macro(&ev).unwrap().len(), 9);
        assert!(
            map.lookup_pager_macro(&ev).is_none(),
            "index macro must not leak into the pager"
        );
        // Help lists the macro with its raw sequence.
        let help = map.help_lines().join("\n");
        assert!(help.contains("Index macros"), "{help}");
        assert!(help.contains("l~f jane<enter>"), "{help}");
    }

    #[test]
    fn shift_in_event_does_not_block_match() {
        // Terminals report 'F' as Char('F') + SHIFT.
        let ev = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT);
        let (map, _) = Keymap::with_config(
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(map.lookup_index(&ev), Some(IndexAction::Flag));
    }
}
