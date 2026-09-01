//! The window's half of rmut: a [`Session`] driven by the shared
//! keymap, with the modes a reader needs. Writing and sending are
//! later rounds; what they would do says so instead of doing it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use eframe::egui;
use rmut_core::notice::{Notice, NoticeSink};
use rmut_core::pattern::Pattern;
use rmut_front::editor::{Edit, History, LineEdit};
use rmut_front::pager::{PagerStyle, pager_line_count};
use rmut_front::style::{Style, rule_style};
use rmut_front::theme::Theme;
use rmut_front::{KeyCode, KeyEvent, Keymap, PagerAction, parse_sequence};
use rmut_session::{
    Answer, Ask, AskKind, FrontOp, Function, Key, Outcome, PageSpot, Request, Session, SidebarOp,
    Wants,
};

pub struct Pager {
    pub view: rmut_core::message::MessageView,
    pub scroll: usize,
    pub full_headers: bool,
    pub hide_quoted: bool,
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
}

#[derive(Clone)]
pub enum LineKind {
    Ask {
        what: AskKind,
        wants: Wants,
    },
    /// The round is read-only throughout, so the mailbox opens the
    /// only way it can.
    ChangeDir,
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
            LineKind::ChangeDir => "mailbox",
            LineKind::EnterCommand => "command",
        }
    }
}

pub enum Prompt {
    Line {
        label: String,
        edit: LineEdit,
        kind: LineKind,
    },
    Key {
        label: String,
        what: AskKind,
    },
}

/// The message line, exactly the TUI's: one notice at a time, shared
/// between the app and its session.
#[derive(Default)]
struct LineState {
    latest: Option<Notice>,
}

#[derive(Clone, Default)]
struct MessageLine(Rc<RefCell<LineState>>);

impl NoticeSink for MessageLine {
    fn notice(&mut self, notice: Notice) {
        self.0.borrow_mut().latest = Some(notice);
    }

    fn latest(&self) -> Option<&Notice> {
        None
    }

    fn clear(&mut self) {
        self.0.borrow_mut().latest = None;
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
    last_poll: Instant,
    pub quit: bool,
}

impl Gui {
    pub fn new(session: Session, mut warnings: Vec<String>, _read_only_flag: bool) -> Gui {
        let config = &session.config;
        let (theme, mut all) = Theme::from_config(config);
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
            last_poll: Instant::now(),
            quit: false,
        };
        // The round is read-only: nothing this window does may write.
        gui.session.read_only = true;
        gui.session.read_only_session = true;
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
        self.notices.0.borrow().latest.clone()
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
                    });
                }
                Request::Command(cmd) => match cmd {
                    rmut_core::command::Command::Exec(name) => self.exec_function(&name),
                    _ => self.not_yet("key bindings from ':'"),
                },
                Request::Editor(_) | Request::ShowDraft | Request::Mailto(_) => {
                    self.not_yet("composing")
                }
                Request::EditFile(_) => self.not_yet("editing"),
                Request::Shell(_) => self.not_yet("shell commands"),
                Request::Suspend => self.not_yet("suspend"),
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
        let (theme, mut more) = Theme::from_config(config);
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
        if self.prompt.is_none() && matches!(self.mode, Mode::Pager(_)) {
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
                self.prompt = Some(Prompt::Key { label, what });
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
            Mode::Index => self.handle_index_key(key),
            Mode::Pager(_) => self.handle_pager_key(key),
            Mode::Help { .. } => self.handle_help_key(key),
            Mode::Folders { .. } => self.handle_folders_key(key),
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match &mut self.prompt {
            Some(Prompt::Key { .. }) => {
                let Some(Prompt::Key { what, .. }) = self.prompt.take() else {
                    return;
                };
                let answer = match key.code {
                    KeyCode::Char(c) => Key::Char(c),
                    KeyCode::Enter => Key::Enter,
                    _ => Key::Other,
                };
                self.answer_ask(what, Answer::Key(answer));
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
                Edit::Complete => {}
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
            LineKind::ChangeDir => {
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
            LineKind::ChangeDir => {
                if input.is_empty() {
                    return;
                }
                self.open_mailbox_spec(input);
            }
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
            FrontOp::ChangeMailbox { read_only: _ } => {
                self.prompt = Some(Prompt::Line {
                    label: "Open mailbox: ".into(),
                    edit: LineEdit::new(String::new()),
                    kind: LineKind::ChangeDir,
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
            FrontOp::Compose(_) | FrontOp::Resend => self.not_yet("composing"),
            FrontOp::RawEdit => self.not_yet("editing"),
            FrontOp::Attachments => self.not_yet("the attachment menu"),
            FrontOp::Query => self.not_yet("query"),
            FrontOp::Notmuch => self.not_yet("notmuch"),
        }
    }

    fn open_selected(&mut self) {
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

    fn run_pager_action(&mut self, action: PagerAction) {
        let page = self.view_size.0;
        let total = self.pager_lines();
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let max_scroll = total.saturating_sub(page);
        match action {
            PagerAction::Back => self.mode = Mode::Index,
            PagerAction::Down => {
                pager.scroll = (pager.scroll + 1).min(max_scroll);
            }
            PagerAction::Up => pager.scroll = pager.scroll.saturating_sub(1),
            PagerAction::PageDown => pager.scroll = (pager.scroll + page).min(max_scroll),
            PagerAction::PageUp => pager.scroll = pager.scroll.saturating_sub(page),
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
            PagerAction::SkipQuoted
            | PagerAction::Search
            | PagerAction::SearchNext
            | PagerAction::SearchPrev
            | PagerAction::SearchToggle => self.not_yet("the pager search"),
            PagerAction::Delete
            | PagerAction::Undelete
            | PagerAction::Flag
            | PagerAction::ToggleNew
            | PagerAction::Tag
            | PagerAction::Undo => self.not_yet("changing mail"),
            PagerAction::Suspend => self.not_yet("suspend"),
            PagerAction::Attachments => self.not_yet("the attachment menu"),
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
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
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
        let keys = ctx.input(|i| crate::input::keys(&i.events));
        if !keys.is_empty() {
            self.keys_this_frame = true;
            self.handle_keys(keys);
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

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
        if self.quit {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
