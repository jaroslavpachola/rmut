//! The window's half of rmut: a [`Session`] driven by the shared
//! keymap, with the modes a reader needs. Writing and sending are
//! later rounds; what they would do says so instead of doing it.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui;
use rmut_core::notice::{Notice, NoticeSink};
use rmut_core::pattern::Pattern;
use rmut_front::editor::{Complete, Edit, History, LineEdit};
use rmut_front::pager::{PagerStyle, pager_line_count};
use rmut_front::status;
use rmut_front::style::{Style, rule_style};
use rmut_front::theme::Theme;
use rmut_front::{KeyCode, KeyEvent, Keymap, PagerAction, parse_sequence};
use rmut_session::{
    Answer, Ask, AskKind, Compose, ComposeKind, FrontOp, Function, Key, Outcome, PageSpot, Request,
    Session, SidebarOp, Wants, default_from, draft_full, header_value, write_draft,
};

pub struct Pager {
    pub view: rmut_core::message::MessageView,
    pub scroll: usize,
    pub full_headers: bool,
    pub hide_quoted: bool,
    /// Set when this pager shows a single attachment part: the
    /// attachment menu to restore on q (mutt returns to the menu).
    pub back: Option<Box<Mode>>,
}

pub enum Mode {
    Index,
    Pager(Pager),
    Help {
        lines: Vec<String>,
        scroll: usize,
    },
    Folders {
        dirs: Vec<(String, usize)>,
        sel: usize,
    },
    /// mutt's attachment menu (v).
    Attach {
        msg_path: std::path::PathBuf,
        parts: Vec<rmut_core::message::Part>,
        sel: usize,
        /// Pager to return to when the menu was opened from there.
        back: Option<Pager>,
    },
    /// An image part, decoded by egui's loaders.
    Image {
        uri: String,
        bytes: std::sync::Arc<[u8]>,
        back: Box<Mode>,
    },
    /// mutt's compose menu: the draft's headers and attachment list,
    /// reviewed between the editor and y (send).
    Compose {
        sel: usize,
    },
    /// Picking one of several postponed drafts to recall.
    Postponed {
        drafts: Vec<(std::path::PathBuf, String)>,
        sel: usize,
    },
    /// query_command results (Q); Enter composes to the pick.
    Query {
        results: Vec<String>,
        sel: usize,
    },
    /// The built-in editor ([gui] editor = "builtin"): the same file
    /// contract as $EDITOR, in an egui text box.
    Edit {
        text: String,
        target: EditTarget,
    },
    /// Embedded Neovim ([gui] editor = "nvim"): the grid lives in
    /// [`Gui::nvim`], every key goes to it, :wq brings the flow back.
    NvimEdit,
}

/// One inline-drawable image: its loader URI and decoded bytes.
pub type PagerImage = (String, std::sync::Arc<[u8]>);

/// Which editor hosts a draft, from `[gui] editor`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    External,
    Builtin,
    Nvim,
}

/// What the built-in editor was editing, so Done knows where the
/// text goes - the mirror of [`PendingEdit`] minus the shell.
pub enum EditTarget {
    /// The draft: written back, then the compose menu.
    Draft(Compose),
    /// A plain file (new-mime); the compose menu returns.
    File(std::path::PathBuf),
    /// The message's own bytes; a change replaces the original.
    Raw {
        orig: std::path::PathBuf,
        original: String,
    },
}

/// What the terminal child that just closed was doing, so its end
/// picks the flow back up.
pub enum PendingEdit {
    /// $EDITOR on the draft: the compose menu comes back after.
    Draft(Compose),
    /// $EDITOR on a plain file (mutt's new-mime); the menu returns.
    File,
    /// mutt's `e`: the message's own bytes; a change replaces it.
    Raw {
        orig: std::path::PathBuf,
        temp: std::path::PathBuf,
        original: Vec<u8>,
    },
    /// A shell or viewer; nothing to do but say it ended.
    Shell(String),
    /// A mailcap viewer on a decoded part; the temp file goes when
    /// it closes.
    View { temp: std::path::PathBuf },
}

#[derive(Clone)]
pub enum LineKind {
    Ask {
        what: AskKind,
        wants: Wants,
    },
    /// Save the selected attachment part to this file.
    SavePart,
    /// Pipe the selected attachment part to this command.
    PipePart,
    ChangeDir {
        read_only: bool,
    },
    /// The pager's text search (`/` inside a message).
    PagerSearch,
    /// The `Q` query menu's search term.
    Query,
    /// The notmuch query (`X`).
    Notmuch,
    EnterCommand,
}

impl LineKind {
    fn history_bucket(&self) -> &'static str {
        match self {
            LineKind::Ask { wants, .. } => match wants {
                Wants::Mailbox => "mailbox",
                Wants::Pattern => "pattern",
                Wants::Address => "address",
                Wants::Command => "command",
                Wants::Other => "other",
            },
            LineKind::ChangeDir { .. } => "mailbox",
            LineKind::PagerSearch => "pattern",
            LineKind::Query => "other",
            LineKind::Notmuch => "notmuch",
            LineKind::SavePart => "file",
            LineKind::PipePart => "command",
            LineKind::EnterCommand => "command",
        }
    }
}

pub enum KeyKind {
    /// A one-key question the session asked.
    Ask(AskKind),
    /// Confirm printing the selected attachment part.
    PrintPart,
    /// mutt's $recall = ask: (n)ew message or (r)ecall postponed?
    Recall,
}

pub enum Prompt {
    Line {
        label: String,
        edit: LineEdit,
        kind: LineKind,
    },
    Key {
        label: String,
        kind: KeyKind,
    },
}

/// The message line, exactly the TUI's: one notice at a time, shared
/// between the app and its session.
#[derive(Default)]
struct LineState {
    latest: Option<Notice>,
}

#[derive(Clone, Default)]
struct MessageLine(std::sync::Arc<std::sync::Mutex<LineState>>);

impl NoticeSink for MessageLine {
    fn notice(&mut self, notice: Notice) {
        self.0.lock().unwrap().latest = Some(notice);
    }

    fn latest(&self) -> Option<&Notice> {
        None
    }

    fn clear(&mut self) {
        self.0.lock().unwrap().latest = None;
    }
}

pub struct Gui {
    pub session: Session,
    pub mode: Mode,
    pub prompt: Option<Prompt>,
    notices: MessageLine,
    pub theme: Theme,
    pub keymap: Keymap,
    pub index_rules: Vec<(Vec<Pattern>, Style)>,
    pub body_rules: Vec<(regex_lite::Regex, Style)>,
    pub index_offset: usize,
    pub sidebar: Vec<(String, usize)>,
    pub sidebar_sel: usize,
    pub sidebar_open: Option<usize>,
    pub sidebar_visible: bool,
    /// Rows and columns of the last content draw, for page motion.
    pub view_size: (usize, usize),
    history: History,
    pending_keys: VecDeque<KeyEvent>,
    tag_next: bool,
    what_key: bool,
    /// The "not in the GUI yet" notices already said once.
    said: std::collections::HashSet<&'static str>,
    /// Keys were handled this frame, so the index recenters on the
    /// cursor; wheel scrolling moves the view without it.
    pub keys_this_frame: bool,
    /// Wheel remainder, in points, until a whole row is crossed.
    pub scroll_px: f32,
    /// The pager's text search, kept across messages so n/N carry
    /// over; the pager highlights its hits.
    pub pager_search: Option<rmut_core::pattern::Matcher>,
    /// mutt's search-toggle (`\`): hide the highlighting without
    /// forgetting the pattern.
    pub pager_search_off: bool,
    /// Its raw text, prefilling the next Search for: prompt.
    pager_search_text: String,
    /// Address completion state at a prompt (Tab cycles).
    complete: Option<Complete>,
    /// The zoom factor as last saved, so a change is written once.
    last_zoom: f32,
    /// The last new-mail text sent as a desktop notification.
    last_notified: String,
    /// How long the visible list was at the last draw, so an arrival
    /// below a view already showing the tail scrolls into sight.
    pub index_len: usize,
    /// A terminal child ($EDITOR, `!`) and what it was doing; keys
    /// wait until it closes.
    editing: Option<(std::process::Child, PendingEdit)>,
    /// The About overlay is up.
    pub about: bool,
    /// The Preferences dialog and its half-edited values.
    pub prefs: Option<Prefs>,
    /// The open message's image parts, decoded once per message:
    /// the bytes of each image/* leaf, in leaf order (None where the
    /// decode failed). The path says which message they belong to.
    pager_images: Option<(std::path::PathBuf, Vec<Option<PagerImage>>)>,
    /// The last pager frame drew inline images, so the wheel may
    /// scroll past the text-row arithmetic to reach them.
    pub pager_drew_images: bool,
    /// The embedded nvim and the flow its exit resumes.
    pub nvim: Option<(crate::nvim::Embedded, PendingEdit)>,
    /// The egui context, for waking the frame loop from threads.
    pub ctx: Option<eframe::egui::Context>,
    last_poll: Instant,
    pub quit: bool,
}

impl Gui {
    pub fn new(session: Session, mut warnings: Vec<String>, read_only: bool) -> Gui {
        let config = &session.config;
        let (theme, mut all) = Theme::from_config(&gui_colored(config));
        let (keymap, key_warnings) = Keymap::with_config(
            &config.keys.index,
            &config.keys.pager,
            &config.macros.index,
            &config.macros.pager,
        );
        all.extend(key_warnings);
        let mut index_rules = Vec::new();
        for rule in &config.color_index {
            match rmut_core::pattern::parse(&rule.pattern) {
                Ok(p) => index_rules.push((p, rule_style(rule, "color_index", &mut all))),
                Err(err) => all.push(format!("bad color_index pattern {:?}: {err}", rule.pattern)),
            }
        }
        let mut body_rules = Vec::new();
        for rule in &config.color_body {
            match regex_lite::Regex::new(&rule.pattern) {
                Ok(re) => body_rules.push((re, rule_style(rule, "color_body", &mut all))),
                Err(err) => all.push(format!("bad color_body regex {:?}: {err}", rule.pattern)),
            }
        }
        all.append(&mut warnings);
        let sidebar_visible = config.sidebar.visible;
        let notices = MessageLine::default();
        let mut gui = Gui {
            session,
            mode: Mode::Index,
            prompt: None,
            notices: notices.clone(),
            theme,
            keymap,
            index_rules,
            body_rules,
            index_offset: 0,
            sidebar: Vec::new(),
            sidebar_sel: 0,
            sidebar_open: None,
            sidebar_visible,
            view_size: (24, 80),
            history: History::default(),
            pending_keys: VecDeque::new(),
            tag_next: false,
            what_key: false,
            said: Default::default(),
            keys_this_frame: false,
            scroll_px: 0.0,
            pager_search: None,
            pager_search_off: false,
            pager_search_text: String::new(),
            complete: None,
            last_zoom: saved_zoom().unwrap_or(1.0),
            editing: None,
            last_notified: String::new(),
            index_len: 0,
            about: false,
            prefs: None,
            pager_images: None,
            pager_drew_images: false,
            nvim: None,
            ctx: None,
            last_poll: Instant::now(),
            quit: false,
        };
        // -R: the whole session stays read-only, exactly the TUI's.
        gui.session.read_only = read_only;
        gui.session.read_only_session = read_only;
        gui.session.install_notices(Box::new(notices));
        if let Some(path) = gui.history_path() {
            gui.history.load(&path);
        }
        gui.refresh_sidebar();
        gui.session.maybe_backfill();
        gui.session.run_folder_hooks();
        if !all.is_empty() {
            gui.note(all.join("; "));
        }
        gui
    }

    pub fn note(&mut self, msg: impl Into<String>) {
        self.session.note(msg);
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.session.error(msg);
    }

    pub fn notice(&self) -> Option<Notice> {
        self.notices.0.lock().unwrap().latest.clone()
    }

    /// A round-boundary: the function exists, this round does not do
    /// it. Said once per session, mutt's way of refusing quietly.
    fn not_yet(&mut self, what: &'static str) {
        if self.said.insert(what) {
            self.error(format!("{what}: not in the GUI yet (use rmut)"));
        }
    }

    fn history_path(&self) -> Option<std::path::PathBuf> {
        self.session
            .config
            .ui
            .history_file
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .map(rmut_session::expand_tilde)
    }

    /// New mail below a view already at the tail stays visible: the
    /// window follows the growth, the cursor staying put (mutt keeps
    /// the fold; a window can do better). The first draw only takes
    /// note, so the open still lands on first-new.
    pub fn follow_tail(&mut self, rows: usize) {
        let len = self.session.visible.len();
        if self.index_len > 0 && len > self.index_len && self.index_offset + rows >= self.index_len
        {
            self.index_offset = len.saturating_sub(rows);
        }
        self.index_len = len;
    }

    pub fn refresh_sidebar(&mut self) {
        if !self.sidebar_visible {
            return;
        }
        let keep = self.sidebar.get(self.sidebar_sel).map(|e| e.0.clone());
        let mut entries: Vec<(String, usize)> = Vec::new();
        for spec in self.session.config.mail.mailboxes.clone() {
            let count = self.session.unseen_count(&spec);
            entries.push((spec, count));
        }
        let (title, dir) = (self.session.title.clone(), self.session.dir.clone());
        let open = |e: &(String, usize)| e.0 == title || rmut_session::expand_tilde(&e.0) == dir;
        if !entries.iter().any(&open) {
            entries.insert(0, (self.session.title.clone(), self.session.new_count()));
        }
        self.sidebar_open = entries.iter().position(&open);
        self.sidebar_sel = keep
            .and_then(|k| entries.iter().position(|e| e.0 == k))
            .or(self.sidebar_open)
            .unwrap_or(0);
        self.sidebar = entries;
    }

    // ---- the session's requests and questions ----

    fn run_requests(&mut self) {
        let mut warnings = Vec::new();
        while let Some(request) = self.session.take_request() {
            match request {
                Request::Quit => self.quit = true,
                Request::ConfigChanged => warnings.extend(self.recompile_ui()),
                Request::MailboxesChanged => self.refresh_sidebar(),
                Request::ShowMessage(view) => {
                    self.mode = Mode::Pager(Pager {
                        view: *view,
                        scroll: 0,
                        full_headers: false,
                        hide_quoted: false,
                        back: None,
                    });
                }
                Request::Command(cmd) => match cmd {
                    rmut_core::command::Command::Exec(name) => self.exec_function(&name),
                    _ => self.not_yet("key bindings from ':'"),
                },
                Request::Editor(compose) => self.start_editor(compose),
                Request::ShowDraft => self.open_compose_menu(),
                Request::Mailto(mailto) => self.start_mailto(&mailto),
                Request::EditFile(path) => self.start_file_edit(&path),
                Request::Shell(command) => self.start_shell(&command),
                Request::Suspend => self.not_yet("suspend (minimize the window)"),
            }
        }
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
    }

    /// A `:set` moved the config: recompile what the window derives
    /// from it, warnings collected rather than fatal.
    fn recompile_ui(&mut self) -> Vec<String> {
        let mut warnings = self.session.recompile();
        let config = &self.session.config;
        let (theme, mut more) = Theme::from_config(&gui_colored(config));
        let (keymap, key_warnings) = Keymap::with_config(
            &config.keys.index,
            &config.keys.pager,
            &config.macros.index,
            &config.macros.pager,
        );
        more.extend(key_warnings);
        let mut index_rules = Vec::new();
        for rule in &config.color_index {
            match rmut_core::pattern::parse(&rule.pattern) {
                Ok(p) => index_rules.push((p, rule_style(rule, "color_index", &mut more))),
                Err(err) => more.push(format!("bad color_index pattern {:?}: {err}", rule.pattern)),
            }
        }
        let mut body_rules = Vec::new();
        for rule in &config.color_body {
            match regex_lite::Regex::new(&rule.pattern) {
                Ok(re) => body_rules.push((re, rule_style(rule, "color_body", &mut more))),
                Err(err) => more.push(format!("bad color_body regex {:?}: {err}", rule.pattern)),
            }
        }
        self.theme = theme;
        self.keymap = keymap;
        self.index_rules = index_rules;
        self.body_rules = body_rules;
        warnings.extend(more);
        warnings
    }

    fn exec_function(&mut self, name: &str) {
        match Function::from_name(name) {
            Some(f) => self.run_index_action(f, false),
            None => self.error(format!("unknown function {name}")),
        }
    }

    /// A question's answer, with mutt's pager-resolve on top: when
    /// the finished answer moved the selection while the pager is
    /// open (a save's advance), the pager follows it (mutt's
    /// pager.c OP_SAVE: `rc = OP_MAIN_NEXT_UNDELETED`).
    fn answer_ask(&mut self, what: AskKind, answer: Answer) {
        let before = self.session.selected_path();
        let next = self.session.answer(what, answer);
        self.open_ask(next);
        if self.prompt.is_none() && matches!(&self.mode, Mode::Pager(p) if p.back.is_none()) {
            if self.session.selected_path() != before {
                self.open_selected();
            } else if self
                .session
                .visible
                .get(self.session.sel)
                .is_some_and(|&i| self.session.msgs[i].env.file.flags.deleted)
            {
                // mutt's pager on a last-message save: next-undeleted
                // finds nothing and falls out to the index.
                self.mode = Mode::Index;
            }
        }
    }

    fn open_ask(&mut self, ask: Option<Ask>) {
        match ask {
            Some(Ask::Line {
                label,
                prefill,
                wants,
                what,
            }) => {
                self.prompt = Some(Prompt::Line {
                    label,
                    edit: LineEdit::new(prefill),
                    kind: LineKind::Ask { what, wants },
                });
            }
            Some(Ask::Key { label, what }) => {
                self.prompt = Some(Prompt::Key {
                    label,
                    kind: KeyKind::Ask(what),
                });
            }
            None => {}
        }
        self.run_requests();
    }

    // ---- keys ----

    pub fn handle_keys(&mut self, keys: Vec<KeyEvent>) {
        for key in keys {
            self.pending_keys.push_back(key);
        }
        while let Some(key) = self.pending_keys.pop_front() {
            self.session.clear_notice();
            self.handle_key(key);
        }
    }

    /// A click, spelled as the keys it stands for (`"r"`,
    /// `"<alt+v>"`): queued for the top of the next frame, so a menu
    /// item can never do anything the keyboard cannot.
    pub fn click(&mut self, keys: &str) {
        if let Some(seq) = parse_sequence(keys) {
            self.pending_keys.extend(seq);
        }
    }

    /// Select by index-row position (a click on the row).
    pub fn click_row(&mut self, vi: usize) {
        self.session.select(vi);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.about {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter) {
                self.about = false;
            }
            return;
        }
        if self.prefs.is_some() {
            if key.code == KeyCode::Esc {
                self.prefs = None;
            }
            return;
        }
        if self.what_key {
            if key.code == KeyCode::Char('g')
                && key.modifiers.contains(rmut_front::KeyModifiers::CONTROL)
            {
                self.what_key = false;
                self.note("what-key ended");
            } else {
                let pattern = rmut_front::KeyPattern {
                    code: key.code,
                    mods: key.modifiers,
                };
                self.note(format!("Char = {}", pattern.display()));
            }
            return;
        }
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
            return;
        }
        match &self.mode {
            Mode::NvimEdit => {
                if let Some((nvim, _)) = &mut self.nvim
                    && let Some(keys) = crate::nvim::notation(&key)
                {
                    nvim.input(&keys);
                }
            }
            Mode::Edit { .. } => {}
            Mode::Index => self.handle_index_key(key),
            Mode::Pager(_) => self.handle_pager_key(key),
            Mode::Help { .. } => self.handle_help_key(key),
            Mode::Folders { .. } => self.handle_folders_key(key),
            Mode::Attach { .. } => self.handle_attach_key(key),
            Mode::Compose { .. } => self.handle_compose_key(key),
            Mode::Postponed { .. } => self.handle_postponed_key(key),
            Mode::Query { .. } => self.handle_query_key(key),
            Mode::Image { .. } => {
                if matches!(
                    key.code,
                    KeyCode::Char('q') | KeyCode::Char('i') | KeyCode::Esc
                ) && let Mode::Image { back, .. } =
                    std::mem::replace(&mut self.mode, Mode::Index)
                {
                    self.mode = *back;
                }
            }
        }
    }

    fn handle_attach_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Attach { parts, sel, .. } = &mut self.mode {
                    *sel = (*sel + 1).min(parts.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Attach { sel, .. } = &mut self.mode {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Char('q') | KeyCode::Char('i') | KeyCode::Esc => {
                let back = match &mut self.mode {
                    Mode::Attach { back, .. } => back.take(),
                    _ => None,
                };
                self.mode = match back {
                    Some(pager) => Mode::Pager(pager),
                    None => Mode::Index,
                };
            }
            KeyCode::Enter => self.view_part(),
            KeyCode::Char('m') => self.view_part_mailcap(),
            KeyCode::Char('T') => self.view_part_text(),
            KeyCode::Char('|') => {
                self.prompt = Some(Prompt::Line {
                    label: "Pipe part to command: ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::PipePart,
                });
            }
            KeyCode::Char('p') => {
                self.prompt = Some(Prompt::Key {
                    label: "Print part? (y/n): ".into(),
                    kind: KeyKind::PrintPart,
                });
            }
            KeyCode::Char('s') => {
                if let Mode::Attach { parts, sel, .. } = &self.mode {
                    let default = parts[*sel]
                        .filename
                        .clone()
                        .unwrap_or_else(|| format!("part-{}.bin", *sel + 1));
                    self.prompt = Some(Prompt::Line {
                        label: "Save to file: ".into(),
                        edit: LineEdit::new(default),
                        kind: LineKind::SavePart,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn open_attachments(&mut self) {
        let Some(&i) = self.session.visible.get(self.session.sel) else {
            return;
        };
        let msg_path = self.session.msgs[i].env.file.path.clone();
        match rmut_core::message::parts(&msg_path) {
            Ok(parts) if !parts.is_empty() => {
                let back = match std::mem::replace(&mut self.mode, Mode::Index) {
                    Mode::Pager(pager) => Some(pager),
                    _ => None,
                };
                self.mode = Mode::Attach {
                    msg_path,
                    parts,
                    sel: 0,
                    back,
                };
            }
            Ok(_) => self.error("message has no parts"),
            Err(err) => self.error(format!("cannot list parts: {err:#}")),
        }
    }

    /// Enter on the attachment menu: text and filtered parts in a
    /// pager, images decoded in a view of their own, the rest said.
    fn view_part(&mut self) {
        let (msg_path, index, is_text, mimetype) = match &self.mode {
            Mode::Attach {
                msg_path,
                parts,
                sel,
                ..
            } => {
                let part = &parts[*sel];
                (msg_path.clone(), *sel, part.is_text, part.mimetype.clone())
            }
            _ => return,
        };
        if mimetype.starts_with("image/") {
            match rmut_core::message::part_bytes(&msg_path, index) {
                Ok(bytes) => {
                    let uri = format!("bytes://{}#{index}", msg_path.display());
                    let back = std::mem::replace(&mut self.mode, Mode::Index);
                    self.mode = Mode::Image {
                        uri,
                        bytes: bytes.into(),
                        back: Box::new(back),
                    };
                }
                Err(err) => self.error(format!("cannot decode part: {err:#}")),
            }
            return;
        }
        let body = if is_text {
            rmut_core::message::part_text(&msg_path, index)
                .map_err(|err| format!("cannot decode part: {err:#}"))
        } else {
            match self.session.display.filters.get(&mimetype).cloned() {
                Some(command) => rmut_core::message::filter_part(&msg_path, index, &command)
                    .map_err(|err| format!("filter failed: {err:#}")),
                None => {
                    // mutt's view-attach on a type that needs mailcap
                    // goes through it; with no entry, mutt says so
                    // and shows the bytes as text.
                    let entries = rmut_core::mailcap::load();
                    if rmut_core::mailcap::viewer_for(&entries, &mimetype).is_some() {
                        self.view_part_mailcap();
                    } else {
                        self.error("no matching mailcap entry found, viewing as text");
                        self.view_part_text();
                    }
                    return;
                }
            }
        };
        match body {
            Ok(body) => self.part_pager(mimetype, body),
            Err(err) => self.error(err),
        }
    }

    /// A decoded part (or a viewer's text over it) in a pager that
    /// knows its way back to the attachment menu.
    fn part_pager(&mut self, mimetype: String, body: String) {
        let headers = vec![("Content-Type".to_string(), mimetype)];
        let menu = std::mem::replace(&mut self.mode, Mode::Index);
        self.mode = Mode::Pager(Pager {
            view: rmut_core::message::MessageView {
                brief: headers.clone(),
                all: headers,
                body,
            },
            scroll: 0,
            full_headers: false,
            hide_quoted: false,
            back: Some(Box::new(menu)),
        });
    }

    /// mutt's view-mailcap (m): the part decoded to a temp file, its
    /// mailcap viewer over it - a copiousoutput viewer's text in a
    /// part pager, an interactive one in the terminal; the temp file
    /// goes when the view ends.
    fn view_part_mailcap(&mut self) {
        let (msg_path, index, mimetype, filename) = match &self.mode {
            Mode::Attach {
                msg_path,
                parts,
                sel,
                ..
            } => {
                let part = &parts[*sel];
                (
                    msg_path.clone(),
                    *sel,
                    part.mimetype.clone(),
                    part.filename.clone(),
                )
            }
            _ => return,
        };
        let entries = rmut_core::mailcap::load();
        let Some(viewer) = rmut_core::mailcap::viewer_for(&entries, &mimetype) else {
            self.error(format!("no mailcap entry for {mimetype}"));
            return;
        };
        let bytes = match rmut_core::message::part_bytes(&msg_path, index) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.error(format!("cannot decode part: {err:#}"));
                return;
            }
        };
        // %s wants a file: the part's own name (basename only, like
        // mutt's sanitizer) in a directory of ours, shaped by the
        // entry's nametemplate - a browser handed an extensionless
        // file sniffs it as text and shows source.
        let name = filename
            .as_deref()
            .and_then(|n| std::path::Path::new(n).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("part-{}", index + 1));
        let name = rmut_core::mailcap::apply_nametemplate(viewer.nametemplate.as_deref(), &name);
        let dir = std::env::temp_dir().join(format!("rmut-egui-{}", std::process::id()));
        let temp = dir.join(name);
        if let Err(err) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&temp, &bytes))
        {
            self.error(format!("cannot write {}: {err}", temp.display()));
            return;
        }
        let quoted = format!("'{}'", temp.display().to_string().replace('\'', "'\\''"));
        let command = viewer.command.replace("%s", &quoted);
        if viewer.copious {
            let shown = run_file_filter(&command, &temp);
            let _ = std::fs::remove_file(&temp);
            match shown {
                Ok(text) => self.part_pager(mimetype, text),
                Err(err) => self.error(format!("viewer failed: {err:#}")),
            }
        } else {
            self.spawn_terminal(&command, &[], PendingEdit::View { temp });
        }
    }

    /// mutt's view-text (T): the decoded bytes as text, whatever the
    /// type claims.
    fn view_part_text(&mut self) {
        let (msg_path, index, mimetype) = match &self.mode {
            Mode::Attach {
                msg_path,
                parts,
                sel,
                ..
            } => (msg_path.clone(), *sel, parts[*sel].mimetype.clone()),
            _ => return,
        };
        match rmut_core::message::part_bytes(&msg_path, index) {
            Ok(bytes) => self.part_pager(mimetype, String::from_utf8_lossy(&bytes).into_owned()),
            Err(err) => self.error(format!("cannot decode part: {err:#}")),
        }
    }

    /// The selected attachment's decoded bytes, from the menu state.
    fn selected_part_bytes(&mut self) -> Option<Vec<u8>> {
        let (msg_path, index) = match &self.mode {
            Mode::Attach { msg_path, sel, .. } => (msg_path.clone(), *sel),
            _ => return None,
        };
        match rmut_core::message::part_bytes(&msg_path, index) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                self.error(format!("cannot decode part: {err:#}"));
                None
            }
        }
    }

    fn save_part(&mut self, input: &str) {
        let (msg_path, index) = match &self.mode {
            Mode::Attach { msg_path, sel, .. } => (msg_path.clone(), *sel),
            _ => return,
        };
        if input.is_empty() {
            self.error("no filename given");
            return;
        }
        let target = rmut_session::expand_tilde(input);
        if target.exists() {
            self.error(format!("{} exists, not overwriting", target.display()));
            return;
        }
        let result = rmut_core::message::part_bytes(&msg_path, index).and_then(|bytes| {
            std::fs::write(&target, &bytes)?;
            Ok(bytes.len())
        });
        match result {
            Ok(n) => self.note(format!("saved {n} bytes to {}", target.display())),
            Err(err) => self.error(format!("save failed: {err:#}")),
        }
    }

    // ---- the terminal, and everything that runs in it ----

    /// The terminal emulator that hosts $EDITOR and `!`:
    /// `[gui] terminal`, $TERMINAL, then the usual suspects.
    fn terminal_program(&self) -> Option<String> {
        if let Some(term) = &self.session.config.gui.terminal {
            return Some(term.clone());
        }
        if let Ok(term) = std::env::var("TERMINAL")
            && !term.trim().is_empty()
        {
            return Some(term);
        }
        [
            "foot",
            "alacritty",
            "kitty",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "terminator",
            "xterm",
            "x-terminal-emulator",
        ]
        .iter()
        .find(|t| {
            std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {t}"))
                .stdout(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .map(|t| t.to_string())
    }

    /// Run `sh -c SCRIPT args...` inside the terminal, without
    /// blocking the window; `what` says how the exit continues.
    fn spawn_terminal(&mut self, script: &str, args: &[&std::ffi::OsStr], what: PendingEdit) {
        let Some(term) = self.terminal_program() else {
            self.error("no terminal found (set [gui] terminal or $TERMINAL)");
            return;
        };
        let mut cmd = std::process::Command::new(&term);
        for flag in terminal_invocation(&term) {
            cmd.arg(flag);
        }
        cmd.arg("sh").arg("-c").arg(script).arg("rmut-egui");
        cmd.args(args);
        match cmd.spawn() {
            Ok(child) => self.editing = Some((child, what)),
            Err(err) => self.error(format!("cannot run {term}: {err}")),
        }
    }

    fn editor_program(&self) -> String {
        self.session.config.mail.editor.clone().unwrap_or_else(|| {
            std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into())
        })
    }

    /// The editor is a choice, never a surprise: mutt's identity is
    /// $EDITOR in a terminal, and the window honours that unless the
    /// config names the built-in box or embedded nvim.
    fn editor_mode(&self) -> EditorMode {
        match self.session.config.gui.editor.as_deref() {
            Some("builtin") => EditorMode::Builtin,
            Some("nvim") => EditorMode::Nvim,
            _ => EditorMode::External,
        }
    }

    fn builtin_editor(&self) -> bool {
        self.editor_mode() == EditorMode::Builtin
    }

    /// Real nvim on this file, the window as its terminal. Trouble
    /// falls back to the external editor rather than losing the flow.
    fn start_nvim(&mut self, path: &std::path::Path, what: PendingEdit) {
        let (rows, cols) = self.view_size;
        let wake = self.ctx.clone();
        match crate::nvim::Embedded::start(path, cols.max(20), rows.max(5), move || {
            if let Some(ctx) = &wake {
                ctx.request_repaint();
            }
        }) {
            Ok(nvim) => {
                self.nvim = Some((nvim, what));
                self.mode = Mode::NvimEdit;
            }
            Err(err) => {
                self.error(format!("cannot start nvim ({err}); using the terminal"));
                match what {
                    PendingEdit::Draft(compose) => {
                        let editor = self.editor_program();
                        let path = compose.path.clone();
                        self.spawn_terminal(
                            &format!("{editor} \"$1\""),
                            &[path.as_os_str()],
                            PendingEdit::Draft(compose),
                        );
                    }
                    other => {
                        let editor = self.editor_program();
                        let path = path.to_path_buf();
                        self.spawn_terminal(
                            &format!("{editor} \"$1\""),
                            &[path.as_os_str()],
                            other,
                        );
                    }
                }
            }
        }
    }

    /// The draft into an editor: the window's own text box when
    /// configured, else $EDITOR in the terminal; the compose menu
    /// comes back either way.
    fn start_editor(&mut self, compose: Compose) {
        if self.editor_mode() == EditorMode::Nvim {
            let path = compose.path.clone();
            self.start_nvim(&path, PendingEdit::Draft(compose));
            return;
        }
        if self.builtin_editor() {
            match std::fs::read_to_string(&compose.path) {
                Ok(text) => {
                    self.mode = Mode::Edit {
                        text,
                        target: EditTarget::Draft(compose),
                    };
                }
                Err(err) => self.error(format!("cannot read draft: {err}")),
            }
            return;
        }
        let editor = self.editor_program();
        let path = compose.path.clone();
        self.spawn_terminal(
            &format!("{editor} \"$1\""),
            &[path.as_os_str()],
            PendingEdit::Draft(compose),
        );
    }

    /// Ctrl+Enter (or the Done button) in the built-in editor.
    pub fn finish_edit(&mut self, save: bool) {
        let Mode::Edit { text, target } = std::mem::replace(&mut self.mode, Mode::Index) else {
            return;
        };
        match target {
            EditTarget::Draft(compose) => {
                if save && let Err(err) = std::fs::write(&compose.path, &text) {
                    self.error(format!("cannot write draft: {err}"));
                    return;
                }
                if save {
                    self.session.set_draft(compose);
                    self.open_compose_menu();
                } else {
                    self.error(format!(
                        "edit abandoned; draft kept at {}",
                        compose.path.display()
                    ));
                }
            }
            EditTarget::File(path) => {
                if save && let Err(err) = std::fs::write(&path, &text) {
                    self.error(format!("cannot write {}: {err}", path.display()));
                }
                self.open_compose_menu();
            }
            EditTarget::Raw { orig, original } => {
                if !save || text == original {
                    self.note("message unchanged");
                } else {
                    self.session.store_edited(&orig, text.as_bytes());
                    self.refresh_sidebar();
                }
            }
        }
    }

    /// A plain file into the editor (mutt's new-mime), the compose
    /// menu back afterwards.
    fn start_file_edit(&mut self, path: &std::path::Path) {
        if self.editor_mode() == EditorMode::Nvim {
            self.start_nvim(path, PendingEdit::File);
            return;
        }
        if self.builtin_editor() {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    self.mode = Mode::Edit {
                        text,
                        target: EditTarget::File(path.to_path_buf()),
                    };
                }
                Err(err) => self.error(format!("cannot read {}: {err}", path.display())),
            }
            return;
        }
        let editor = self.editor_program();
        self.spawn_terminal(
            &format!("{editor} \"$1\""),
            &[path.as_os_str()],
            PendingEdit::File,
        );
    }

    /// mutt's `!`: the command (or an interactive $shell) in the
    /// terminal, the window waiting politely.
    fn start_shell(&mut self, command: &str) {
        let label = if command.trim().is_empty() {
            "shell".to_string()
        } else {
            command.to_string()
        };
        let script = if command.trim().is_empty() {
            self.session
                .config
                .mail
                .shell
                .clone()
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "sh".into())
        } else {
            command.to_string()
        };
        self.spawn_terminal(&script, &[], PendingEdit::Shell(label));
    }

    /// mutt's edit function (`e`): the raw bytes through $EDITOR; a
    /// changed result replaces the original.
    fn start_raw_edit(&mut self) {
        if self.session.deny_readonly() {
            return;
        }
        if self.session.mbox.is_some() {
            self.error("editing in place is not supported for mbox spools");
            return;
        }
        if self.session.full_message_bytes().is_none() {
            return;
        }
        let Some(orig) = self.session.selected_path() else {
            return;
        };
        let original = match std::fs::read(&orig) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.error(format!("cannot read message: {err}"));
                return;
            }
        };
        if self.editor_mode() == EditorMode::Nvim {
            let temp = match write_draft("").and_then(|p| {
                std::fs::write(&p, &original)?;
                Ok(p)
            }) {
                Ok(p) => p,
                Err(err) => {
                    self.error(format!("cannot write edit copy: {err:#}"));
                    return;
                }
            };
            self.start_nvim(
                &temp.clone(),
                PendingEdit::Raw {
                    orig,
                    temp,
                    original,
                },
            );
            return;
        }
        // The built-in editor is a text box: only clean UTF-8 can go
        // through it unharmed; anything else keeps the terminal.
        if self.builtin_editor()
            && let Ok(text) = String::from_utf8(original.clone())
        {
            self.mode = Mode::Edit {
                target: EditTarget::Raw {
                    orig,
                    original: text.clone(),
                },
                text,
            };
            return;
        }
        let temp = match write_draft("").and_then(|p| {
            std::fs::write(&p, &original)?;
            Ok(p)
        }) {
            Ok(p) => p,
            Err(err) => {
                self.error(format!("cannot write edit copy: {err:#}"));
                return;
            }
        };
        let editor = self.editor_program();
        let temp_os = temp.clone();
        self.spawn_terminal(
            &format!("{editor} \"$1\""),
            &[temp_os.as_os_str()],
            PendingEdit::Raw {
                orig,
                temp,
                original,
            },
        );
    }

    /// The terminal child closed: pick the flow back up.
    fn editor_done(&mut self, what: PendingEdit, success: bool) {
        match what {
            PendingEdit::Draft(compose) => {
                if success {
                    self.session.set_draft(compose);
                    self.open_compose_menu();
                } else {
                    self.error(format!(
                        "editor failed; draft kept at {}",
                        compose.path.display()
                    ));
                }
            }
            PendingEdit::File => {
                if !success {
                    self.error("editor failed");
                }
                self.open_compose_menu();
            }
            PendingEdit::Raw {
                orig,
                temp,
                original,
            } => {
                let edited = std::fs::read(&temp).unwrap_or_default();
                let _ = std::fs::remove_file(&temp);
                if !success {
                    self.error("editor failed, message unchanged");
                } else if edited == original {
                    self.note("message unchanged");
                } else {
                    self.session.store_edited(&orig, &edited);
                    self.refresh_sidebar();
                }
            }
            PendingEdit::Shell(label) => {
                if success {
                    self.note(format!("{label} finished"));
                } else {
                    self.error(format!("{label} failed"));
                }
                self.run_requests();
            }
            PendingEdit::View { temp } => {
                let _ = std::fs::remove_file(&temp);
                if !success {
                    self.error("viewer failed");
                }
            }
        }
    }

    // ---- composing ----

    /// mutt asks before a new message when there are postponed
    /// drafts, since recalling one is a menu rather than an answer.
    fn start_compose(&mut self, kind: ComposeKind) {
        if kind == ComposeKind::New && self.session.has_postponed() {
            match self
                .session
                .config
                .mail
                .recall
                .as_deref()
                .unwrap_or("ask-yes")
                .trim()
                .to_lowercase()
                .as_str()
            {
                "no" => {}
                "yes" => {
                    self.recall_postponed();
                    return;
                }
                _ => {
                    self.prompt = Some(Prompt::Key {
                        label: "(n)ew message or (r)ecall postponed? ".into(),
                        kind: KeyKind::Recall,
                    });
                    return;
                }
            }
        }
        let ask = self.session.start_compose(kind);
        self.open_ask(ask);
    }

    /// One of the draft's headers, edited from the compose menu.
    fn ask_header(&mut self, name: &str) {
        let ask = self.session.ask_header(name);
        self.open_ask(ask);
    }

    /// mutt's compose menu: entered after the editor, and again after
    /// every sub-prompt, until y sends, P/q postpones, or q discards.
    fn open_compose_menu(&mut self) {
        if self.session.draft().is_some() {
            let sel = match self.mode {
                Mode::Compose { sel } => sel,
                _ => 0,
            };
            self.mode = Mode::Compose { sel };
        }
    }

    /// Header lines the compose menu shows; From falls back to the
    /// identity that send would use.
    pub fn compose_header_lines(&self) -> Vec<(&'static str, String)> {
        let head = self.session.draft_head();
        let get = |n: &str| header_value(&head, n).unwrap_or_default();
        let from = match header_value(&head, "From") {
            Some(f) => f,
            None => self
                .session
                .current_identity(&[])
                .from_line()
                .unwrap_or_else(|| default_from(&rmut_core::maildir::hostname())),
        };
        let security = self
            .session
            .draft()
            .map(|c| c.security.label())
            .filter(|l| !l.is_empty())
            .unwrap_or("none");
        let fcc = match self.session.draft().and_then(|c| c.fcc.clone()) {
            Some(fcc) if fcc.is_empty() => "(no copy)".into(),
            Some(fcc) => fcc,
            None if self.session.config.mail.copy == Some(false) => "(no copy)".into(),
            None => {
                let default = self.session.default_fcc(self.session.draft());
                if default.is_empty() {
                    "(nearby Sent maildir)".into()
                } else {
                    default
                }
            }
        };
        vec![
            ("From", from),
            ("To", get("To")),
            ("Cc", get("Cc")),
            ("Bcc", get("Bcc")),
            ("Subject", get("Subject")),
            ("Fcc", fcc),
            ("Security", security.to_string()),
        ]
    }

    /// Attachment-table entries: the body, the forwarded original,
    /// then every Attach: file, in detach order.
    pub fn compose_entries(&self) -> Vec<String> {
        let Some(c) = self.session.draft() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let body_size = std::fs::metadata(&c.path).map(|m| m.len()).unwrap_or(0);
        out.push(format!(
            "{:<28} {:>8}  text/plain",
            "(message body)",
            rmut_front::pager::humanize_size(body_size)
        ));
        if let Some(orig) = &c.attach {
            let size = std::fs::metadata(orig).map(|m| m.len()).unwrap_or(0);
            out.push(format!(
                "{:<28} {:>8}  message/rfc822  forwarded original",
                orig.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                rmut_front::pager::humanize_size(size)
            ));
        }
        let text = draft_full(c).unwrap_or_default();
        for a in rmut_core::compose::extract_attachments(&text).1 {
            let size = match std::fs::metadata(&a.path) {
                Ok(m) => rmut_front::pager::humanize_size(m.len()),
                Err(_) => "missing!".into(),
            };
            let mut marks = String::new();
            if a.inline {
                marks += " [inline]";
            }
            if a.unlink {
                marks += " [unlink]";
            }
            if let Some(name) = &a.name {
                marks += &format!(" as {name}");
            }
            out.push(format!(
                "{:<28} {:>8}  {}{}{}",
                a.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                size,
                a.mime
                    .as_deref()
                    .unwrap_or_else(|| rmut_core::compose::content_type(&a.path)),
                marks,
                a.description
                    .as_deref()
                    .map(|d| format!("  ({d})"))
                    .unwrap_or_default(),
            ));
        }
        out
    }

    fn handle_compose_key(&mut self, key: KeyEvent) {
        let entries = self.compose_entries().len();
        let Mode::Compose { sel } = &mut self.mode else {
            return;
        };
        let ctrl = key.modifiers.contains(rmut_front::KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => *sel = (*sel + 1).min(entries.saturating_sub(1)),
            KeyCode::Char('k') | KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Char('y') => {
                self.mode = Mode::Index;
                let ask = self.session.send_draft();
                self.open_ask(ask);
            }
            KeyCode::Char('e') => {
                self.mode = Mode::Index;
                if let Some(c) = self.session.take_draft() {
                    self.start_editor(c);
                }
            }
            KeyCode::Char('t') if ctrl => {
                let sel = *sel;
                let ask = self.session.ask_attach_field(sel, true);
                self.open_ask(ask);
            }
            KeyCode::Char('t') => self.ask_header("To"),
            KeyCode::Char('c') => self.ask_header("Cc"),
            KeyCode::Char('b') => self.ask_header("Bcc"),
            KeyCode::Char('s') => self.ask_header("Subject"),
            KeyCode::Char('F') => self.ask_header("From"),
            KeyCode::Char('r') => self.ask_header("Reply-To"),
            KeyCode::Char('d') if ctrl => {
                let sel = *sel;
                self.session.toggle_disposition(sel);
            }
            KeyCode::Char('d') => {
                let sel = *sel;
                let ask = self.session.ask_attach_field(sel, false);
                self.open_ask(ask);
            }
            KeyCode::Char('f') => {
                let ask = self.session.ask_fcc();
                self.open_ask(ask);
            }
            KeyCode::Char('a') => {
                let ask = self.session.ask_attach_file();
                self.open_ask(ask);
            }
            KeyCode::Char('A') => self.session.attach_messages(),
            KeyCode::Char('o') if ctrl => {
                let sel = *sel;
                let ask = self.session.ask_rename_attachment(sel);
                self.open_ask(ask);
            }
            KeyCode::Char('u') => {
                let sel = *sel;
                self.session.toggle_unlink(sel);
            }
            KeyCode::Char('n') => {
                let ask = self.session.ask_new_mime();
                self.open_ask(ask);
            }
            KeyCode::Char('K') | KeyCode::Char('J') => {
                let up = key.code == KeyCode::Char('K');
                let at = *sel;
                if self.session.move_attachment(at, up)
                    && let Mode::Compose { sel } = &mut self.mode
                {
                    *sel = if up { at - 1 } else { at + 1 };
                }
            }
            KeyCode::Char('w') => {
                let ask = self.session.ask_write_fcc();
                self.open_ask(ask);
            }
            KeyCode::Char('i') => {
                if let Some(command) = self.session.ispell_command() {
                    self.start_shell(&command);
                }
            }
            KeyCode::Char('V') => self.view_compose_entry_with(View::Mailcap),
            KeyCode::Char('v') if key.modifiers.contains(rmut_front::KeyModifiers::ALT) => {
                self.view_compose_entry_with(View::Text)
            }
            KeyCode::Enter => self.view_compose_entry(),
            KeyCode::Char('D') => {
                let sel = *sel;
                self.session.detach(sel);
                let len = self.compose_entries().len();
                if let Mode::Compose { sel } = &mut self.mode {
                    *sel = (*sel).min(len.saturating_sub(1));
                }
            }
            KeyCode::Char('p') => {
                let ask = self.session.ask_security();
                self.open_ask(ask);
            }
            KeyCode::Char('P') => {
                self.mode = Mode::Index;
                if let Some(draft) = self.session.take_draft() {
                    self.session.postpone_draft(draft);
                }
            }
            KeyCode::Char('q') => {
                self.mode = Mode::Index;
                let ask = self.session.ask_postpone();
                self.open_ask(ask);
            }
            _ => {}
        }
    }

    /// Enter in the compose menu: the selected entry as text - the
    /// draft body, the forwarded original, or an attached file
    /// (text directly, other types through their [filters] command).
    fn view_compose_entry(&mut self) {
        self.view_compose_entry_with(View::Filter);
    }

    /// mutt's view-mailcap (V) and view-text (Esc v) on a compose-menu
    /// file: a copiousoutput viewer's text in the window, an
    /// interactive viewer in the terminal (like `!`), or the bytes as
    /// text whatever the type.
    fn view_compose_entry_with(&mut self, how: View) {
        let Mode::Compose { sel } = self.mode else {
            return;
        };
        if how != View::Filter {
            let Some(k) = self.session.attach_index(sel) else {
                return;
            };
            let Some(a) = self.session.attachments().into_iter().nth(k) else {
                return;
            };
            let mimetype = a
                .mime
                .clone()
                .unwrap_or_else(|| rmut_core::compose::content_type(&a.path).to_string());
            let name = a.path.display().to_string();
            match how {
                View::Text => match std::fs::read(&a.path) {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let mut lines = vec![name, String::new()];
                        lines.extend(text.lines().map(String::from));
                        self.mode = Mode::Help { lines, scroll: 0 };
                    }
                    Err(err) => self.error(format!("cannot read {name}: {err}")),
                },
                _ => {
                    let entries = rmut_core::mailcap::load();
                    match rmut_core::mailcap::viewer_for(&entries, &mimetype) {
                        Some(viewer) => {
                            let quoted = format!("'{}'", name.replace('\'', "'\\''"));
                            let command = viewer.command.replace("%s", &quoted);
                            if viewer.copious {
                                match run_file_filter(&command, &a.path) {
                                    Ok(text) => {
                                        let mut lines = vec![name, String::new()];
                                        lines.extend(text.lines().map(String::from));
                                        self.mode = Mode::Help { lines, scroll: 0 };
                                    }
                                    Err(err) => self.error(format!("viewer failed: {err:#}")),
                                }
                            } else {
                                let label = command.clone();
                                self.spawn_terminal(&command, &[], PendingEdit::Shell(label));
                            }
                        }
                        None => self.error(format!("no mailcap entry for {mimetype}")),
                    }
                }
            }
            return;
        }
        let Some(c) = self.session.draft() else {
            return;
        };
        let has_orig = c.attach.is_some();
        let (title, text) = if sel == 0 {
            let content = std::fs::read_to_string(&c.path).unwrap_or_default();
            let body = match &c.hidden_head {
                Some(_) => content,
                None => match content.split_once("\n\n") {
                    Some((_, b)) => b.to_string(),
                    None => content,
                },
            };
            ("Message body".to_string(), body)
        } else if has_orig && sel == 1 {
            let path = c.attach.clone().unwrap_or_default();
            (
                "Forwarded original".to_string(),
                rmut_core::message::body_text(&path).unwrap_or_default(),
            )
        } else {
            let k = sel - 1 - usize::from(has_orig);
            let full = draft_full(c).unwrap_or_default();
            let Some(a) = rmut_core::compose::extract_attachments(&full)
                .1
                .into_iter()
                .nth(k)
            else {
                return;
            };
            let mimetype = a
                .mime
                .clone()
                .unwrap_or_else(|| rmut_core::compose::content_type(&a.path).to_string());
            let name = a.path.display().to_string();
            match self.session.display.filters.get(mimetype.as_str()).cloned() {
                Some(command) => match run_file_filter(&command, &a.path) {
                    Ok(text) => (name, text),
                    Err(err) => {
                        self.error(format!("filter failed: {err:#}"));
                        return;
                    }
                },
                None if mimetype.starts_with("text/") => match std::fs::read_to_string(&a.path) {
                    Ok(text) => (name, text),
                    Err(err) => {
                        self.error(format!("cannot read {name}: {err}"));
                        return;
                    }
                },
                None => {
                    self.note(format!("no [filters] entry for {mimetype}"));
                    return;
                }
            }
        };
        let mut lines = vec![title, String::new()];
        lines.extend(text.lines().map(String::from));
        self.mode = Mode::Help { lines, scroll: 0 };
    }

    /// Recall a postponed draft: straight into the editor when there
    /// is one, a picker when there are several.
    fn recall_postponed(&mut self) {
        let mut files = self
            .session
            .postponed_dir()
            .and_then(|dir| rmut_core::maildir::scan(&dir).ok())
            .unwrap_or_default();
        if files.is_empty() {
            self.error("no postponed messages");
            return;
        }
        if files.len() == 1 {
            let file = files.remove(0);
            if let Some(compose) = self.session.recall_file(file.path) {
                self.start_editor(compose);
            }
            return;
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.path.metadata().and_then(|m| m.modified()).ok()));
        let drafts = files
            .into_iter()
            .map(|f| {
                let label = match rmut_core::message::envelope(f.clone()) {
                    Ok(env) => format!(
                        "{}  {}",
                        rmut_core::message::format_index_date(env.date),
                        env.subject
                    ),
                    Err(_) => f.path.display().to_string(),
                };
                (f.path, label)
            })
            .collect();
        self.mode = Mode::Postponed { drafts, sel: 0 };
    }

    /// `-p`: straight into the postponed picker.
    pub fn open_postponed(&mut self) {
        self.recall_postponed();
    }

    fn handle_postponed_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Postponed { drafts, sel } = &mut self.mode {
                    *sel = (*sel + 1).min(drafts.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Postponed { sel, .. } = &mut self.mode {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
            KeyCode::Enter => {
                let path = match &self.mode {
                    Mode::Postponed { drafts, sel } => drafts.get(*sel).map(|d| d.0.clone()),
                    _ => None,
                };
                if let Some(path) = path {
                    self.mode = Mode::Index;
                    if let Some(compose) = self.session.recall_file(path) {
                        self.start_editor(compose);
                    }
                }
            }
            _ => {}
        }
    }

    fn run_query(&mut self, input: &str) {
        if input.is_empty() {
            return;
        }
        let Some(command) = self.session.config.mail.query_command.clone() else {
            return;
        };
        let results = rmut_core::alias::query(&command, input);
        if results.is_empty() {
            self.note("query returned nothing");
            return;
        }
        self.mode = Mode::Query { results, sel: 0 };
    }

    fn handle_query_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Query { results, sel } = &mut self.mode {
                    *sel = (*sel + 1).min(results.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Query { sel, .. } = &mut self.mode {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
            KeyCode::Enter | KeyCode::Char('m') => {
                let addr = match &self.mode {
                    Mode::Query { results, sel } => results.get(*sel).cloned(),
                    _ => None,
                };
                let Some(addr) = addr else { return };
                self.mode = Mode::Index;
                // The normal compose chain, with To prefilled.
                self.start_compose(ComposeKind::New);
                if let Some(Prompt::Line { edit, .. }) = &mut self.prompt {
                    edit.set(&addr);
                }
            }
            _ => {}
        }
    }

    fn notmuch_search(&mut self, query: &str) {
        let Some((dir, count)) = self.session.notmuch_mirror(query) else {
            return;
        };
        self.open_mailbox_spec(&dir.display().to_string());
        if self.session.dir == dir {
            // The virtual mailbox: never write through the symlinks.
            self.session.read_only = true;
            self.session.title = format!("notmuch: {query}");
            self.note(format!("{count} matching message(s)"));
        }
    }

    pub fn start_mailto(&mut self, m: &rmut_core::mailto::Mailto) {
        let from = self.session.compose_from(None, &m.to);
        let text = rmut_core::compose::draft_text(
            &rmut_core::compose::DraftHeaders {
                from,
                to: m.to.clone(),
                cc: m.cc.clone(),
                subject: m.subject.clone(),
                in_reply_to: None,
                references: None,
            },
            &m.body,
        );
        let text = match m.bcc.as_deref().filter(|b| !b.trim().is_empty()) {
            Some(bcc) => match text.split_once("\n\n") {
                Some((head, body)) => format!("{head}\nBcc: {bcc}\n\n{body}"),
                None => text,
            },
            None => text,
        };
        match self.session.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.start_editor(Compose {
                    path,
                    recall_source: None,
                    security: self.session.default_security(),
                    attach: None,
                    hidden_head,
                    fcc: None,
                });
            }
            Err(err) => self.error(format!("cannot write draft: {err:#}")),
        }
    }

    /// Open a copy of the message as a new draft (mutt's resend).
    fn resend_current(&mut self) {
        if self.session.full_message_bytes().is_none() {
            return;
        }
        let Some(base) = self.session.compose_base() else {
            self.error("no message selected");
            return;
        };
        let body = rmut_core::message::body_text(&base.path).unwrap_or_default();
        let text = rmut_core::compose::draft_text(
            &rmut_core::compose::DraftHeaders {
                from: self.session.compose_from(None, &base.orig_to),
                to: base.orig_to.clone(),
                cc: (!base.orig_cc.trim().is_empty()).then(|| base.orig_cc.clone()),
                subject: base.subject.clone(),
                in_reply_to: None,
                references: None,
            },
            &body,
        );
        match self.session.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.start_editor(Compose {
                    path,
                    recall_source: None,
                    security: self.session.default_security(),
                    attach: None,
                    hidden_head,
                    fcc: None,
                });
            }
            Err(err) => self.error(format!("cannot write draft: {err:#}")),
        }
    }

    fn pipe_part(&mut self, command: &str) {
        if command.is_empty() {
            self.error("no command given");
            return;
        }
        let Some(bytes) = self.selected_part_bytes() else {
            return;
        };
        match rmut_session::pipe_to(command, &bytes) {
            Ok(()) => self.note(format!("piped to {command}")),
            Err(err) => self.error(format!("pipe failed: {err:#}")),
        }
    }

    /// p on the attachment menu: the decoded part to print_command.
    fn print_part(&mut self) {
        let Some(bytes) = self.selected_part_bytes() else {
            return;
        };
        let command = self
            .session
            .config
            .mail
            .print
            .clone()
            .unwrap_or_else(|| "lpr".into());
        match rmut_session::pipe_to(&command, &bytes) {
            Ok(()) => self.note(format!("printed via {command}")),
            Err(err) => self.error(format!("print failed: {err:#}")),
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match &mut self.prompt {
            Some(Prompt::Key { .. }) => {
                let Some(Prompt::Key { kind, .. }) = self.prompt.take() else {
                    return;
                };
                match kind {
                    KeyKind::Ask(what) => {
                        let answer = match key.code {
                            KeyCode::Char(c) => Key::Char(c),
                            KeyCode::Enter => Key::Enter,
                            _ => Key::Other,
                        };
                        self.answer_ask(what, Answer::Key(answer));
                    }
                    KeyKind::PrintPart => {
                        if key.code == KeyCode::Char('y') {
                            self.print_part();
                        }
                    }
                    KeyKind::Recall => match key.code {
                        KeyCode::Char('r') => self.recall_postponed(),
                        KeyCode::Char('n') => {
                            let ask = self.session.start_compose(ComposeKind::New);
                            self.open_ask(ask);
                        }
                        _ => {}
                    },
                }
            }
            Some(Prompt::Line { edit, .. }) => match edit.key(key) {
                Edit::Cancel => {
                    self.prompt = None;
                    self.session.cancel_setup();
                }
                Edit::History(older) => {
                    if let Some(Prompt::Line { edit, kind, .. }) = &mut self.prompt {
                        edit.history_step(self.history.get(kind.history_bucket()), older);
                    }
                }
                Edit::Complete => self.tab_complete(),
                Edit::Submit => {
                    if let Some(Prompt::Line { edit, kind, .. }) = self.prompt.take() {
                        self.history.push(kind.history_bucket(), &edit.buf);
                        if let Some(path) = self.history_path() {
                            let cap = self.session.config.ui.save_history.unwrap_or(100);
                            self.history.save(&path, cap);
                        }
                        self.run_line_prompt(kind, edit.buf.trim());
                    }
                }
                Edit::Edited | Edit::Ignored => {}
            },
            None => {}
        }
    }

    fn run_line_prompt(&mut self, kind: LineKind, input: &str) {
        let expanded;
        let input = match &kind {
            LineKind::ChangeDir { .. } => {
                expanded = rmut_core::config::expand_folder(
                    input,
                    self.session.config.mail.folder.as_deref(),
                );
                expanded.as_str()
            }
            _ => input,
        };
        match kind {
            LineKind::Ask { what, .. } => self.answer_ask(what, Answer::Line(input)),
            LineKind::ChangeDir { read_only } => {
                if input.is_empty() {
                    return;
                }
                self.open_mailbox_spec(input);
                // mutt's Alt+c: this one mailbox opens read-only.
                if read_only {
                    self.session.read_only = true;
                    self.note(format!("{} (read-only)", self.session.title));
                }
            }
            LineKind::Query => self.run_query(input),
            LineKind::Notmuch => self.notmuch_search(input),
            LineKind::PagerSearch => {
                if !input.is_empty() {
                    self.pager_search = Some(rmut_core::pattern::Matcher::new(input));
                    self.pager_search_off = false;
                    self.pager_search_text = input.to_string();
                }
                if self.pager_search.is_some() {
                    self.pager_search_step(true);
                } else {
                    self.error("No search pattern.");
                }
            }
            LineKind::SavePart => self.save_part(input),
            LineKind::PipePart => self.pipe_part(input),
            LineKind::EnterCommand => {
                if input.is_empty() {
                    return;
                }
                // The sidebar's runtime toggle is the front end's, so
                // only a config change moves it, not a recompile.
                let sidebar_before = self.session.config.sidebar.visible;
                let run = self.session.run_command_line(input);
                let mut reports = run.reports;
                reports.extend(run.warnings);
                self.run_requests();
                if self.session.config.sidebar.visible != sidebar_before {
                    self.sidebar_visible = self.session.config.sidebar.visible;
                    self.refresh_sidebar();
                }
                if !reports.is_empty() {
                    self.note(reports.join("; "));
                }
            }
        }
    }

    fn outcome(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Done => self.run_requests(),
            Outcome::Ask(ask) => self.open_ask(Some(ask)),
            Outcome::Front(op) => self.run_front_op(op),
        }
    }

    fn handle_index_key(&mut self, key: KeyEvent) {
        if let Some(seq) = self.keymap.lookup_index_macro(&key) {
            let seq: Vec<KeyEvent> = seq.to_vec();
            for k in seq.into_iter().rev() {
                self.pending_keys.push_front(k);
            }
            return;
        }
        let Some(action) = self.keymap.lookup_index(&key) else {
            self.tag_next = false;
            return;
        };
        let tagged = std::mem::take(&mut self.tag_next);
        self.run_index_action(action, tagged);
    }

    fn run_index_action(&mut self, action: Function, tagged: bool) {
        if tagged && !action.takes_tagged() {
            self.error(format!("{} does not take the tagged set", action.name()));
            return;
        }
        let page = self.view_size.0;
        let outcome = self.session.run_function(action, tagged, page);
        self.outcome(outcome);
    }

    /// Every `FrontOp` has an arm here, and the lint keeps it so: a
    /// new one fails this build until the window says what it does
    /// with it. The writing and sending arms say their round.
    #[deny(clippy::wildcard_enum_match_arm)]
    fn run_front_op(&mut self, op: FrontOp) {
        match op {
            FrontOp::Exit => self.quit = true,
            FrontOp::OpenSelected => self.open_selected(),
            FrontOp::Help => {
                self.mode = Mode::Help {
                    lines: self.keymap.help_lines(),
                    scroll: 0,
                }
            }
            FrontOp::Redraw => {}
            FrontOp::OpenMailbox(spec) => self.open_mailbox_spec(&spec),
            FrontOp::ErrorHistory(lines) => {
                self.mode = Mode::Help { lines, scroll: 0 };
            }
            FrontOp::WhatKey => {
                self.what_key = true;
                self.note("describing keys (Ctrl+G ends it)");
            }
            FrontOp::ChangeMailbox { read_only } => {
                self.prompt = Some(Prompt::Line {
                    label: "Open mailbox: ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::ChangeDir { read_only },
                });
            }
            FrontOp::CommandPrompt => {
                self.prompt = Some(Prompt::Line {
                    label: ": ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::EnterCommand,
                });
            }
            FrontOp::Folders => self.open_folder_browser(),
            FrontOp::TagPrefix => self.tag_next = true,
            FrontOp::PageMove(spot) => {
                let rows = self.view_size.0;
                let last = self
                    .session
                    .visible
                    .len()
                    .min(self.index_offset + rows)
                    .saturating_sub(1);
                let target = match spot {
                    PageSpot::Top => self.index_offset,
                    PageSpot::Middle => (self.index_offset + last) / 2,
                    PageSpot::Bottom => last,
                };
                self.session.select(target);
            }
            FrontOp::Sidebar(SidebarOp::Toggle) => {
                self.sidebar_visible = !self.sidebar_visible;
                self.refresh_sidebar();
            }
            FrontOp::Sidebar(_) if !self.sidebar_visible => {
                self.error("the sidebar is hidden (B shows it)");
            }
            FrontOp::Sidebar(SidebarOp::Next) => {
                if !self.sidebar.is_empty() {
                    self.sidebar_sel = (self.sidebar_sel + 1).min(self.sidebar.len() - 1);
                }
            }
            FrontOp::Sidebar(SidebarOp::Prev) => {
                self.sidebar_sel = self.sidebar_sel.saturating_sub(1);
            }
            FrontOp::Sidebar(SidebarOp::Open) => {
                if let Some((spec, _)) = self.sidebar.get(self.sidebar_sel).cloned() {
                    self.open_mailbox_spec(&spec);
                }
            }
            FrontOp::Compose(kind) => self.start_compose(kind),
            FrontOp::Resend => self.resend_current(),
            FrontOp::RawEdit => self.start_raw_edit(),
            FrontOp::Attachments => self.open_attachments(),
            FrontOp::Query => {
                self.prompt = Some(Prompt::Line {
                    label: "Query for: ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::Query,
                });
            }
            FrontOp::Notmuch => {
                self.prompt = Some(Prompt::Line {
                    label: "Notmuch query: ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::Notmuch,
                });
            }
        }
    }

    pub(crate) fn open_selected(&mut self) {
        // A fresh pager session, search-wise, like mutt; the text
        // stays as the next prompt's prefill.
        self.pager_search = None;
        self.session.open_message();
        self.run_requests();
    }

    pub fn open_mailbox_spec(&mut self, spec: &str) {
        if !self.session.ready_to_leave() {
            self.run_requests();
            return;
        }
        let progress: rmut_core::remote::Progress = Box::new(|_| {});
        match self.session.switch_to(spec, progress) {
            Ok(warnings) => {
                self.mode = Mode::Index;
                self.index_offset = 0;
                self.tag_next = false;
                if !warnings.is_empty() {
                    self.note(warnings.join("; "));
                }
                self.refresh_sidebar();
                self.session.maybe_backfill();
                self.session.run_folder_hooks();
                self.run_requests();
            }
            Err(err) => self.error(format!("cannot open {spec}: {err:#}")),
        }
        self.complete = None;
    }

    /// -e on the command line: one config command before the window.
    pub fn run_startup_command(&mut self, line: &str) {
        let run = self.session.run_command_line(line);
        let mut reports = run.reports;
        reports.extend(run.warnings);
        self.run_requests();
        if !reports.is_empty() {
            self.note(reports.join("; "));
        }
    }

    /// Tab at a prompt: at an address prompt, complete the token
    /// against aliases and query_command; at a mailbox prompt,
    /// against the folder candidates; with nothing typed at the c
    /// prompt, the folder browser opens instead. Repeated Tab
    /// cycles. The TUI's, ported.
    fn tab_complete(&mut self) {
        let (buf_now, kind) = match &self.prompt {
            Some(Prompt::Line { edit, kind, .. }) => (edit.buf.clone(), kind.clone()),
            _ => return,
        };
        let is_addr = matches!(
            kind,
            LineKind::Ask {
                wants: Wants::Address,
                ..
            }
        );
        let is_mbox = matches!(
            kind,
            LineKind::Ask {
                wants: Wants::Mailbox,
                ..
            } | LineKind::ChangeDir { .. }
        );
        if !is_addr && !is_mbox {
            return;
        }
        if matches!(kind, LineKind::ChangeDir { .. }) && buf_now.trim().is_empty() {
            self.prompt = None;
            self.complete = None;
            self.open_folder_browser();
            return;
        }
        let set_buf = |gui: &mut Gui, text: &str| {
            if let Some(Prompt::Line { edit, .. }) = &mut gui.prompt {
                edit.set(text);
            }
        };
        if let Some(c) = &mut self.complete
            && let Some((next, note)) = c.cycle(&buf_now)
        {
            set_buf(self, &next);
            self.note(note);
            return;
        }
        self.complete = None;
        let (start, word) = Complete::token(&buf_now, is_addr);
        if word.is_empty() {
            self.error("nothing to complete");
            return;
        }
        let candidates = if is_addr {
            rmut_core::alias::complete(
                &word,
                &rmut_core::alias::load(self.session.config.mail.alias_file.as_deref()),
                self.session.config.mail.query_command.as_deref(),
                self.session.config.mail.sort_alias.as_deref(),
            )
        } else {
            let specs = match self.session.folder_candidates() {
                Ok(specs) => specs,
                Err(err) => {
                    self.error(format!("cannot list folders: {err:#}"));
                    return;
                }
            };
            // The typed prefix against the spec as written and
            // tilde-expanded, so both spellings hit.
            let wexp = rmut_session::expand_tilde(&word).display().to_string();
            specs
                .into_iter()
                .map(|(spec, _)| spec)
                .filter(|s| {
                    s.starts_with(&word)
                        || rmut_session::expand_tilde(s)
                            .display()
                            .to_string()
                            .starts_with(&wexp)
                })
                .collect()
        };
        if candidates.is_empty() {
            self.error(format!("no matches for {word}"));
            return;
        }
        let (state, next, note) = Complete::first(&buf_now, start, candidates);
        set_buf(self, &next);
        if let Some(note) = note {
            self.note(note);
        }
        self.complete = Some(state);
    }

    pub fn open_folder_browser(&mut self) {
        match self.session.folder_candidates() {
            Ok(dirs) => {
                let mut dirs = dirs;
                rmut_session::sort_browser(
                    &mut dirs,
                    self.session.config.ui.sort_browser.as_deref(),
                );
                self.mode = Mode::Folders { dirs, sel: 0 };
            }
            Err(err) => self.error(format!("cannot list folders: {err:#}")),
        }
    }

    fn handle_pager_key(&mut self, key: KeyEvent) {
        if let Some(seq) = self.keymap.lookup_pager_macro(&key) {
            let seq: Vec<KeyEvent> = seq.to_vec();
            for k in seq.into_iter().rev() {
                self.pending_keys.push_front(k);
            }
            return;
        }
        let Some(action) = self.keymap.lookup_pager(&key) else {
            return;
        };
        self.run_pager_action(action);
    }

    /// The k-th image part of the open message, decoded once per
    /// message: the body's image Type markers and the leaf walk are
    /// both depth-first, so the k-th marker is the k-th image leaf.
    /// (An alternative branch left unrendered could shift the pair;
    /// then the guard below simply draws nothing.)
    pub fn pager_image(&mut self, k: usize) -> Option<PagerImage> {
        let path = self.session.selected_path()?;
        if self.pager_images.as_ref().map(|(p, _)| p.as_path()) != Some(path.as_path()) {
            let mut list = Vec::new();
            if let Ok(parts) = rmut_core::message::parts(&path) {
                for (i, part) in parts.iter().enumerate() {
                    if part.mimetype.starts_with("image/") {
                        list.push(rmut_core::message::part_bytes(&path, i).ok().map(|bytes| {
                            let uri = format!("bytes://{}#{i}", path.display());
                            (uri, std::sync::Arc::from(bytes))
                        }));
                    }
                }
            }
            self.pager_images = Some((path, list));
        }
        self.pager_images.as_ref()?.1.get(k)?.clone()
    }

    /// Total pager display lines at this width, for the wheel.
    pub fn pager_line_total(&self, width: usize) -> usize {
        let Mode::Pager(pager) = &self.mode else {
            return 0;
        };
        let wrap = rmut_front::status::pager_wrap(&self.session.config, width);
        pager_line_count(
            &pager.view,
            wrap,
            pager.full_headers,
            &PagerStyle::of(&self.session.config, &self.session.quote_re),
            pager.hide_quoted,
        )
    }

    fn pager_lines(&self) -> usize {
        let Mode::Pager(pager) = &self.mode else {
            return 0;
        };
        let width = rmut_front::status::pager_wrap(&self.session.config, self.view_size.1);
        pager_line_count(
            &pager.view,
            width,
            pager.full_headers,
            &PagerStyle::of(&self.session.config, &self.session.quote_re),
            pager.hide_quoted,
        )
    }

    /// The pager's own page: the mini-index (pager.index_lines)
    /// shrinks the viewport, and $pager_context keeps overlap.
    fn pager_page(&self) -> usize {
        self.view_size
            .0
            .saturating_sub(self.session.config.pager.index_lines as usize)
            .max(1)
    }

    fn pager_search_step(&mut self, forward: bool) {
        let Some(matcher) = self.pager_search.clone() else {
            // n/N with nothing searched yet: ask for the pattern
            // first (mutt falls through to search the same way).
            self.prompt = Some(Prompt::Line {
                label: "Search for: ".into(),
                edit: LineEdit::new(self.pager_search_text.clone()),
                kind: LineKind::PagerSearch,
            });
            return;
        };
        let width = status::pager_wrap(&self.session.config, self.view_size.1);
        let context = self.session.config.pager.search_context;
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let lines = rmut_front::pager::pager_text_lines(
            &pager.view,
            width,
            pager.full_headers,
            &PagerStyle::of(&self.session.config, &self.session.quote_re),
            pager.hide_quoted,
        );
        match rmut_front::pager::search_lines(&lines, &matcher, pager.scroll, forward) {
            Some((hit, wrapped)) => {
                // The hit becomes the top line even near the end,
                // like mutt; $search_context keeps lines above it.
                pager.scroll = hit.saturating_sub(context);
                if wrapped {
                    self.note(match forward {
                        true => "Search wrapped to top.",
                        false => "Search wrapped to bottom.",
                    });
                }
            }
            None => self.error("Not found."),
        }
    }

    fn run_pager_action(&mut self, action: PagerAction) {
        let page = self.pager_page();
        let step = page
            .saturating_sub(self.session.config.pager.context)
            .max(1);
        let total = self.pager_lines();
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let max_scroll = total.saturating_sub(page);
        match action {
            PagerAction::Back => {
                let back = pager.back.take();
                self.mode = match back {
                    Some(menu) => *menu,
                    None => Mode::Index,
                };
            }
            PagerAction::Down => {
                pager.scroll = (pager.scroll + 1).min(max_scroll);
            }
            PagerAction::Up => pager.scroll = pager.scroll.saturating_sub(1),
            PagerAction::PageDown => pager.scroll = (pager.scroll + step).min(max_scroll),
            PagerAction::PageUp => pager.scroll = pager.scroll.saturating_sub(step),
            PagerAction::HalfDown => pager.scroll = (pager.scroll + page / 2).min(max_scroll),
            PagerAction::HalfUp => pager.scroll = pager.scroll.saturating_sub(page / 2),
            PagerAction::Top => pager.scroll = 0,
            PagerAction::Bottom => pager.scroll = max_scroll,
            PagerAction::ToggleQuoted => {
                pager.hide_quoted = !pager.hide_quoted;
                pager.scroll = 0;
            }
            PagerAction::Headers => {
                pager.full_headers = !pager.full_headers;
                pager.scroll = 0;
            }
            PagerAction::NextMsg | PagerAction::NextUndeleted => {
                match self
                    .session
                    .step_message(true, action == PagerAction::NextUndeleted)
                {
                    Some(pos) => {
                        self.session.sel = pos;
                        self.open_selected();
                    }
                    None => self.error("last message"),
                }
            }
            PagerAction::PrevMsg | PagerAction::PrevUndeleted => {
                match self
                    .session
                    .step_message(false, action == PagerAction::PrevUndeleted)
                {
                    Some(pos) => {
                        self.session.sel = pos;
                        self.open_selected();
                    }
                    None => self.error("first message"),
                }
            }
            PagerAction::Redraw => {}
            PagerAction::Help => {
                self.mode = Mode::Help {
                    lines: self.keymap.help_lines(),
                    scroll: 0,
                }
            }
            PagerAction::EnterCommand => self.run_front_op(FrontOp::CommandPrompt),
            PagerAction::ErrorHistory => {
                let lines = self.session.error_history();
                self.mode = Mode::Help { lines, scroll: 0 };
            }
            PagerAction::WhatKey => self.run_front_op(FrontOp::WhatKey),
            PagerAction::Search => {
                self.prompt = Some(Prompt::Line {
                    label: "Search for: ".into(),
                    edit: LineEdit::new(self.pager_search_text.clone()),
                    kind: LineKind::PagerSearch,
                });
            }
            PagerAction::SearchNext => self.pager_search_step(true),
            PagerAction::SearchPrev => self.pager_search_step(false),
            PagerAction::SearchToggle => {
                if self.pager_search.is_none() {
                    self.note("no search to toggle");
                } else {
                    self.pager_search_off = !self.pager_search_off;
                    self.note(if self.pager_search_off {
                        "search highlighting off"
                    } else {
                        "search highlighting on"
                    });
                }
            }
            PagerAction::SkipQuoted => self.not_yet("skip-quoted"),
            PagerAction::Delete => {
                if self.session.deny_readonly() {
                    return;
                }
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("delete", &[i]);
                    self.session.msgs[i].env.file.flags.deleted = true;
                    self.session.msgs[i].dirty = true;
                }
                // mutt's $resolve: advance to the next undeleted, or
                // fall out to the index from the last message.
                match self.session.step_message(true, true) {
                    Some(pos) => {
                        self.session.sel = pos;
                        self.open_selected();
                    }
                    None => self.mode = Mode::Index,
                }
            }
            PagerAction::Undelete | PagerAction::Flag | PagerAction::ToggleNew => {
                if self.session.deny_readonly() {
                    return;
                }
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    let what = match action {
                        PagerAction::Undelete => "undelete",
                        PagerAction::Flag => "flag",
                        _ => "toggle read",
                    };
                    self.session.push_undo(what, &[i]);
                    let file = &mut self.session.msgs[i].env.file;
                    match action {
                        PagerAction::Undelete => file.flags.deleted = false,
                        PagerAction::Flag => file.flags.flagged = !file.flags.flagged,
                        _ => {
                            file.flags.seen = !file.flags.seen;
                            file.is_new = false;
                        }
                    }
                    self.session.msgs[i].dirty = true;
                }
            }
            PagerAction::Tag => {
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("tag", &[i]);
                    self.session.msgs[i].env.tagged = !self.session.msgs[i].env.tagged;
                }
            }
            PagerAction::Undo => {
                // A message still inside its $undo_send window is the
                // most recent thing done, so it is what undo takes
                // back first.
                if self.session.cancel_send() {
                    self.run_requests();
                } else {
                    self.session.undo_last();
                }
            }
            PagerAction::Suspend => self.not_yet("suspend"),
            PagerAction::Attachments => self.open_attachments(),
            PagerAction::Compose
            | PagerAction::Reply
            | PagerAction::GroupReply
            | PagerAction::ListReply
            | PagerAction::Forward
            | PagerAction::Resend
            | PagerAction::Edit
            | PagerAction::CreateAlias
            | PagerAction::Bounce => self.not_yet("composing"),
            PagerAction::Print | PagerAction::Pipe => self.not_yet("piping"),
            PagerAction::Save | PagerAction::Copy => self.not_yet("saving"),
            PagerAction::ListAction => self.not_yet("list actions"),
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        let page = self.view_size.0;
        let Mode::Help { lines, scroll } = &mut self.mode else {
            return;
        };
        let max = lines.len().saturating_sub(page);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') => {
                self.mode = Mode::Index;
                // Leaving the entry preview resumes the send flow,
                // the TUI's way: a staged draft means the compose
                // menu is what q returns to, never the bare index.
                if self.session.draft().is_some() {
                    self.open_compose_menu();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => *scroll = (*scroll + page).min(max),
            KeyCode::Char('-') | KeyCode::PageUp => *scroll = scroll.saturating_sub(page),
            _ => {}
        }
    }

    fn handle_folders_key(&mut self, key: KeyEvent) {
        let Mode::Folders { dirs, sel } = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
            KeyCode::Char('j') | KeyCode::Down => {
                *sel = (*sel + 1).min(dirs.len().saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Enter => {
                if let Some((spec, _)) = dirs.get(*sel).cloned() {
                    self.open_mailbox_spec(&spec);
                }
            }
            _ => {}
        }
    }
}

impl Gui {
    /// One frame: the session's background work, this frame's keys,
    /// the painting, and the next wake-up. Everything but the window
    /// itself, so a test harness can drive it without one.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Embedded nvim: apply whatever it drew, and when it quits
        // (:wq, :q!) the flow continues as an editor exit does.
        self.ctx = Some(ctx.clone());
        if let Some((nvim, _)) = &mut self.nvim {
            nvim.pump();
            if nvim.finished {
                let (nvim, what) = self.nvim.take().unwrap();
                drop(nvim);
                self.mode = Mode::Index;
                self.editor_done(what, true);
            }
        }
        // The terminal child ($EDITOR, `!`): when it closes, its
        // flow continues; while it runs, this window only waits.
        if let Some((child, _)) = &mut self.editing {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (_, what) = self.editing.take().unwrap();
                    self.editor_done(what, status.success());
                }
                Ok(None) => {}
                Err(err) => {
                    self.editing = None;
                    self.error(format!("lost the editor: {err}"));
                }
            }
        }
        // Keys a menu or context item queued during the last paint.
        if !self.pending_keys.is_empty() {
            self.handle_keys(Vec::new());
            self.keys_this_frame = true;
        }
        // The connection's finished work lands between frames, the
        // way the TUI does it between keys.
        self.session.sync_message_hooks();
        self.session.poll_network();
        self.session.tick_outbox();
        self.run_requests();
        let poll_every =
            Duration::from_secs(self.session.config.mail.poll_seconds.unwrap_or(5).max(1));
        if self.session.idle_kick() || self.last_poll.elapsed() >= poll_every {
            self.last_poll = Instant::now();
            self.session.check_new_mail();
            self.refresh_sidebar();
        }
        // Ctrl+wheel (and a trackpad pinch) zoom the window, on top
        // of Ctrl+= / Ctrl+- / Ctrl+0. egui splits the wheel itself:
        // with the zoom modifier held the scroll delta stays zero, so
        // this never fights the row scrolling.
        let zoom = ctx.input(|i| i.zoom_delta());
        if zoom != 1.0 {
            ctx.set_zoom_factor((ctx.zoom_factor() * zoom).clamp(0.5, 4.0));
        }
        // Whatever changed the zoom (wheel here, egui's own keys),
        // remember it for the next run, once per change.
        let now = ctx.zoom_factor();
        if (now - self.last_zoom).abs() > f32::EPSILON {
            self.last_zoom = now;
            if let Some(path) = zoom_path() {
                let _ = std::fs::create_dir_all(path.parent().unwrap());
                let _ = std::fs::write(&path, format!("{now}\n"));
            }
        }
        // The plan's G5 notification: new mail while the window is
        // unfocused becomes a desktop notice - only as the fallback,
        // when no $new_mail_command is configured (the session runs
        // that one itself, whatever the front end).
        if let Some(n) = self.notice()
            && n.is_new_mail()
            && self.session.config.mail.new_mail_command.is_none()
            && !ctx.input(|i| i.focused)
        {
            let text = n.text();
            if self.last_notified != text {
                self.last_notified = text.clone();
                let _ = std::process::Command::new("notify-send")
                    .arg("rmut")
                    .arg(&text)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
        let keys = ctx.input(|i| crate::input::keys(&i.events));
        // While the keymap owns the keyboard, egui must not run its
        // own focus traversal: Tab is completion here, not
        // focus-next, or the first menu button quietly takes focus
        // and the next Enter "clicks" it. The events were read
        // above; consuming them now hides them from the widgets
        // only. Any focus already granted is surrendered the same
        // way - except when a widget screen (the built-in editor,
        // a dialog) really owns the keys.
        let widgets_own_keys =
            matches!(self.mode, Mode::Edit { .. }) || self.prefs.is_some() || self.about;
        if !widgets_own_keys {
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
            });
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        if !keys.is_empty() {
            if self.editing.is_some() {
                self.note("editing in the terminal; close it to continue");
            } else {
                self.keys_this_frame = true;
                self.handle_keys(keys);
            }
        }
        if self.editing.is_some() {
            // Wake soon to notice the editor closing.
            ctx.request_repaint_after(Duration::from_millis(200));
        }
        crate::paint::draw(self, ui);
        self.keys_this_frame = false;
        // Idle by default: wake for the poll interval, or quickly
        // while the connection owes an answer.
        ctx.request_repaint_after(if self.session.busy().is_some() {
            Duration::from_millis(30)
        } else {
            poll_every
        });
    }
}

/// Where the window remembers its zoom: the cache, next to the
/// header caches, never the user's config.
pub fn zoom_path() -> Option<std::path::PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(dir) if !dir.is_empty() => std::path::PathBuf::from(dir),
        _ => rmut_session::expand_tilde("~/.cache"),
    };
    Some(base.join("rmut").join("gui-zoom"))
}

/// The saved zoom factor, if any run saved one.
pub fn saved_zoom() -> Option<f32> {
    let text = std::fs::read_to_string(zoom_path()?).ok()?;
    let zoom: f32 = text.trim().parse().ok()?;
    (0.5..=4.0).contains(&zoom).then_some(zoom)
}

/// A shell command over a file's bytes, its stdout as text.
/// How a compose-menu file is shown: through its [filters] command
/// (Enter), its mailcap viewer (V), or as text (Esc v).
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Filter,
    Mailcap,
    Text,
}

fn run_file_filter(command: &str, path: &std::path::Path) -> anyhow::Result<String> {
    use anyhow::Context as _;
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::from(file))
        .output()
        .with_context(|| format!("running {command}"))?;
    anyhow::ensure!(out.status.success(), "{command} exited with {}", out.status);
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The config as the window colors it: `[gui.colors]` laid over
/// `[colors]`, so the window can wear its own palette while the
/// terminal keeps the shared one.
fn gui_colored(config: &rmut_core::config::Config) -> rmut_core::config::Config {
    let mut config = config.clone();
    for (key, value) in config.gui.colors.clone() {
        config.colors.insert(key, value);
    }
    config
}

/// The Preferences dialog's working copy: edited in the modal,
/// applied to the live config, saved to the overlay file.
pub struct Prefs {
    pub size: f32,
    pub font: String,
    pub terminal: String,
    pub background: eframe::egui::Color32,
    pub foreground: eframe::egui::Color32,
    pub proportional: bool,
    pub inline_images: bool,
    /// text/html rendered as text (off: mutt's raw source view).
    pub html_text: bool,
    pub editor: EditorMode,
}

impl Prefs {
    /// The dialog opens on what the window currently runs with.
    pub fn from_config(config: &rmut_core::config::Config) -> Prefs {
        let color = |name: &Option<String>, fallback: (u8, u8, u8)| {
            let (r, g, b) = name
                .as_deref()
                .and_then(rmut_front::style::parse_color)
                .and_then(|c| match c {
                    rmut_front::style::Color::Rgb(r, g, b) => Some((r, g, b)),
                    _ => None,
                })
                .unwrap_or(fallback);
            eframe::egui::Color32::from_rgb(r, g, b)
        };
        Prefs {
            size: config.gui.size.unwrap_or(14.0),
            font: config.gui.font.clone().unwrap_or_default(),
            terminal: config.gui.terminal.clone().unwrap_or_default(),
            background: color(&config.gui.background, (0x10, 0x10, 0x10)),
            foreground: color(&config.gui.foreground, (0xd8, 0xd8, 0xd8)),
            proportional: config.gui.proportional.unwrap_or(false),
            inline_images: config.gui.inline_images.unwrap_or(true),
            html_text: config.pager.html.as_deref() != Some("raw"),
            editor: match config.gui.editor.as_deref() {
                Some("builtin") => EditorMode::Builtin,
                Some("nvim") => EditorMode::Nvim,
                _ => EditorMode::External,
            },
        }
    }
}

fn hex(c: eframe::egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

impl Gui {
    /// Apply the dialog to the running window: the config's [gui]
    /// half moves, and everything that reads it follows next frame.
    pub fn apply_prefs(&mut self, ctx: &eframe::egui::Context) {
        let Some(prefs) = &self.prefs else { return };
        let gui = &mut self.session.config.gui;
        gui.size = Some(prefs.size.clamp(6.0, 40.0));
        gui.background = Some(hex(prefs.background));
        gui.foreground = Some(hex(prefs.foreground));
        gui.terminal = (!prefs.terminal.trim().is_empty()).then(|| prefs.terminal.clone());
        gui.proportional = Some(prefs.proportional);
        gui.inline_images = Some(prefs.inline_images);
        let html = if prefs.html_text { "text" } else { "raw" };
        gui.html = Some(html.into());
        gui.editor = Some(
            match prefs.editor {
                EditorMode::External => "external",
                EditorMode::Builtin => "builtin",
                EditorMode::Nvim => "nvim",
            }
            .into(),
        );
        let font = prefs.font.trim().to_string();
        let font_changed = gui.font.as_deref().unwrap_or("") != font;
        gui.font = (!font.is_empty()).then(|| font.clone());
        if font_changed {
            if font.is_empty() {
                ctx.set_fonts(eframe::egui::FontDefinitions::default());
            } else {
                install_font(ctx, &font);
            }
        }
        // The html choice lives in the shared pager config; the
        // display rebuilds so the next message opens the new way.
        self.session.config.pager.html = Some(html.into());
        let warnings = self.session.recompile();
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
    }

    /// Save the [gui] section to the overlay file the window owns
    /// (never the hand-written config), and say where it went.
    pub fn save_prefs(&mut self) {
        let Some(path) = gui_overlay_path() else {
            self.error("no config directory to save into");
            return;
        };
        let gui = &self.session.config.gui;
        let mut out = String::from(
            "# written by rmut-egui's Preferences dialog; loaded over\n# the [gui] section of config.toml\n",
        );
        if let Some(size) = gui.size {
            out += &format!("size = {size}\n");
        }
        if let Some(proportional) = gui.proportional {
            out += &format!("proportional = {proportional}\n");
        }
        if let Some(inline_images) = gui.inline_images {
            out += &format!("inline_images = {inline_images}\n");
        }
        for (key, value) in [
            ("html", &gui.html),
            ("editor", &gui.editor),
            ("font", &gui.font),
            ("terminal", &gui.terminal),
            ("background", &gui.background),
            ("foreground", &gui.foreground),
        ] {
            if let Some(value) = value {
                out += &format!("{key} = {value:?}\n");
            }
        }
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&path, out));
        match result {
            Ok(()) => self.note(format!("saved to {}", path.display())),
            Err(err) => self.error(format!("cannot save: {err}")),
        }
    }
}

/// The dialog's file: gui.toml next to the config.
pub fn gui_overlay_path() -> Option<std::path::PathBuf> {
    Some(rmut_core::config::path()?.parent()?.join("gui.toml"))
}

/// Lay the saved dialog values over the loaded config's [gui]: the
/// hand-written section is the default, the dialog's file wins.
pub fn load_gui_overlay(config: &mut rmut_core::config::Config) {
    let Some(path) = gui_overlay_path() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(saved) = toml::from_str::<rmut_core::config::Gui>(&text) else {
        eprintln!("rmut-egui: {} is not readable, ignored", path.display());
        return;
    };
    let gui = &mut config.gui;
    for (mine, theirs) in [
        (&mut gui.html, saved.html),
        (&mut gui.editor, saved.editor),
        (&mut gui.font, saved.font),
        (&mut gui.terminal, saved.terminal),
        (&mut gui.background, saved.background),
        (&mut gui.foreground, saved.foreground),
    ] {
        if theirs.is_some() {
            *mine = theirs;
        }
    }
    if saved.size.is_some() {
        gui.size = saved.size;
    }
    if saved.proportional.is_some() {
        gui.proportional = saved.proportional;
    }
    if saved.inline_images.is_some() {
        gui.inline_images = saved.inline_images;
    }
}

/// `[gui] font`: the file's face becomes the monospace family (and
/// the fallback for everything), egui's built-ins behind it. A file
/// that cannot be read is said and skipped, never fatal.
pub fn install_font(ctx: &eframe::egui::Context, path: &str) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};
    let expanded = rmut_session::expand_tilde(path);
    let bytes = match std::fs::read(&expanded) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!(
                "rmut-egui: cannot read [gui] font {}: {err}",
                expanded.display()
            );
            return;
        }
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("gui.font".to_string(), FontData::from_owned(bytes).into());
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "gui.font".to_string());
    }
    ctx.set_fonts(fonts);
}

/// How this terminal takes a command line. gnome-terminal without
/// --wait forks to its server and "finishes" at once, which would
/// resume the compose flow mid-edit; terminator and xfce4-terminal
/// run the rest of the line only behind -x; kitty takes it bare;
/// everything else honours -e.
pub fn terminal_invocation(term: &str) -> &'static [&'static str] {
    let base = term.rsplit('/').next().unwrap_or(term);
    match base {
        "kitty" => &[],
        "gnome-terminal" => &["--wait", "--"],
        "xfce4-terminal" => &["--disable-server", "-x"],
        "terminator" => &["-x"],
        _ => &["-e"],
    }
}
