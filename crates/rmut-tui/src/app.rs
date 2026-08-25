use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write as _;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rmut_core::config::Config;
use rmut_core::notice::{Notice, NoticeSink};
use rmut_core::pattern::{self, Pattern};
use rmut_core::{alias, command, compose, maildir, message};
use rmut_session::{
    Answer, Ask, AskKind, Compose, ComposeKind, Key, PatternOp, Request, Session, ThreadOp, Wants,
    default_from, draft_full, expand_tilde, pipe_to, wrap_order, write_draft,
};

use crate::keymap::{IndexAction, Keymap, PagerAction, parse_key, parse_sequence};
use crate::theme::Theme;

/// neomutt's $abort_noattach_regex default: the words that make a
/// draft look like it should have carried a file.
pub struct Pager {
    pub view: message::MessageView,
    pub scroll: usize,
    pub full_headers: bool,
    /// mutt's toggle-quoted (`T`): quoted lines are dropped from the
    /// display while set.
    pub hide_quoted: bool,
    /// Set when this pager shows a single attachment part: the
    /// attachment menu to restore on q or paging past the end (mutt
    /// returns to the menu there, never to the next message).
    pub back: Option<Box<Mode>>,
}

pub enum Mode {
    Index,
    Pager(Pager),
    /// Mutt's compose menu: the draft's headers and attachment list,
    /// reviewed between the editor and y (send).
    Compose {
        sel: usize,
    },
    Attach {
        msg_path: PathBuf,
        parts: Vec<message::Part>,
        sel: usize,
        /// Pager to return to when the menu was opened from there.
        back: Option<Pager>,
    },
    Folders {
        /// Paths or `imap:` specs (ready for `open_mailbox_spec`) with
        /// their new/unseen counts. A trailing `/` marks a plain
        /// directory (Enter descends); `..` goes up.
        dirs: Vec<(String, usize)>,
        sel: usize,
        /// Directory being browsed; None for the mailbox candidate
        /// list the browser opens with.
        root: Option<PathBuf>,
    },
    /// Picking one of several postponed drafts to recall.
    Postponed {
        drafts: Vec<(PathBuf, String)>,
        sel: usize,
    },
    /// query_command results (`Q`); Enter composes to the pick.
    Query {
        results: Vec<String>,
        sel: usize,
    },
    Help {
        lines: Vec<String>,
        scroll: usize,
    },
}

#[derive(Clone)]
pub enum LineKind {
    /// A question the session asked, carried back untouched when it
    /// is answered. `wants` is its hint about what the answer is, for
    /// the history bucket and Tab completion.
    Ask {
        what: AskKind,
        wants: Wants,
    },
    /// The pager's text search (`/` inside a message).
    PagerSearch,
    /// Folder browser: directory to list, and maildir to create.
    BrowseDir,
    CreateDir,
    /// Pipe the selected attachment part to a command.
    PipePart,
    /// The `Q` query menu's search term.
    Query,
    /// The notmuch query (`X`).
    Notmuch,
    /// The `:` prompt: one config command (mutt's enter-command).
    EnterCommand,
    ChangeDir,
    /// The same, opened read-only (mutt's Esc c).
    ChangeDirReadOnly,
    SavePart,
}

impl LineKind {
    /// History bucket, mutt-style: one shared list per input class.
    /// The kinds whose answer names a mailbox, so `=x` / `+x` expand
    /// before anything reads it. Same list as the "mailbox" history
    /// bucket below.
    fn takes_mailbox(&self) -> bool {
        matches!(
            self,
            LineKind::ChangeDir
                | LineKind::ChangeDirReadOnly
                | LineKind::BrowseDir
                | LineKind::CreateDir
        )
    }

    fn history_bucket(&self) -> &'static str {
        match self {
            // A session question says what its answer is; the buckets
            // are the same ones the front end's own prompts use.
            LineKind::Ask { wants, .. } => match wants {
                Wants::Mailbox => "mailbox",
                Wants::Pattern => "pattern",
                Wants::Address => "address",
                Wants::Command => "command",
                Wants::Other => "other",
            },
            LineKind::PagerSearch => "pattern",
            LineKind::ChangeDir
            | LineKind::ChangeDirReadOnly
            | LineKind::BrowseDir
            | LineKind::CreateDir => "mailbox",
            LineKind::SavePart => "file",
            LineKind::PipePart => "command",
            LineKind::Notmuch => "notmuch",
            LineKind::EnterCommand => "command",
            LineKind::Query => "other",
        }
    }
}

#[derive(Clone)]
pub enum KeyKind {
    /// A one-key question the session asked.
    Ask(AskKind),
    Recall,
    /// Confirm printing the selected attachment part.
    PrintPart,
}

pub enum Prompt {
    Line {
        label: String,
        buf: String,
        kind: LineKind,
        /// Cursor as a char position into `buf`.
        cursor: usize,
        /// Index into the kind's history while browsing with Up/Down.
        hist_pos: Option<usize>,
        /// The line being typed, restored when browsing steps back
        /// past the newest history entry.
        stash: String,
    },
    Key {
        label: String,
        kind: KeyKind,
    },
}

impl Prompt {
    /// A line prompt with the cursor at the end of the prefill.
    fn line(label: impl Into<String>, buf: String, kind: LineKind) -> Prompt {
        let cursor = buf.chars().count();
        Prompt::Line {
            label: label.into(),
            buf,
            kind,
            cursor,
            hist_pos: None,
            stash: String::new(),
        }
    }
}

/// Byte offset of the `cursor`-th char (the length when past the end).
pub(crate) fn byte_at(buf: &str, cursor: usize) -> usize {
    buf.char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(buf.len())
}

/// Tab-completion state at an address prompt: candidates for the token
/// at `start`, `expect` being the whole buffer after the last
/// insertion (an edit in between restarts the match).
struct Complete {
    start: usize,
    candidates: Vec<String>,
    index: usize,
    expect: String,
}

/// The sink the TUI installs in its session: mutt's message line says
/// one thing at a time, so the last notice is the only one worth
/// keeping. It is dropped on the next key, once the user has had
/// their chance to read it.
///
/// The app keeps it behind an `Rc` and hands the session a second
/// handle, so a `set beep` mid-session still reaches the bell.
#[derive(Default)]
struct Line {
    latest: Option<Notice>,
    /// mutt's $beep: ring the terminal bell on an error.
    beep: bool,
}

/// A handle on it, so the app and the session it drives write to the
/// same line.
#[derive(Clone, Default)]
struct MessageLine(Rc<RefCell<Line>>);

impl NoticeSink for MessageLine {
    fn notice(&mut self, notice: Notice) {
        let mut line = self.0.borrow_mut();
        // The bell belongs to the front end, not to the notice.
        if notice.is_error() && line.beep {
            use std::io::Write as _;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
        line.latest = Some(notice);
    }

    fn latest(&self) -> Option<&Notice> {
        // A `RefCell` cannot hand out a plain reference; the app
        // reads its own handle through `App::notice` instead, and the
        // session only ever writes through this one.
        None
    }

    fn clear(&mut self) {
        self.0.borrow_mut().latest = None;
    }
}

pub struct App {
    /// The open mailbox and everything that can be done to it. The
    /// front end owns one and drives it; it owns nothing here.
    pub session: Session,
    pub index_offset: usize,
    pub mode: Mode,
    pub prompt: Option<Prompt>,
    /// The message line, written to from both sides.
    notices: MessageLine,
    /// The pager's text search, kept across messages so n/N carry
    /// over; the pager highlights its hits.
    pub(crate) pager_search: Option<pattern::Matcher>,
    /// Its raw text, prefilling the next Search for: prompt (mutt
    /// prefills from its searchbuf the same way).
    pub(crate) pager_search_text: String,
    /// Compiled [[color_body]] rules: regex + style, in config order.
    pub(crate) body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
    /// Trouble from the send at exit, printed once the terminal is
    /// back (nobody would see a status line by then).
    pub exit_notes: Vec<String>,
    /// Width and content rows from the last key dispatch, for actions
    /// (prompt submissions) that arrive without a size at hand.
    view_size: (usize, usize),
    pub theme: Theme,
    pub keymap: Keymap,
    pending_editor: Option<Compose>,
    /// Message whose raw bytes go through $EDITOR next loop tick
    /// (mutt's edit function).
    pending_raw_edit: Option<PathBuf>,
    /// Address completion state at the To prompt (Tab cycles).
    complete: Option<Complete>,
    /// Keys queued by a macro, consumed before real terminal input.
    pending_keys: std::collections::VecDeque<KeyEvent>,
    /// Prompt history per input class (newest first), for the session.
    history: HashMap<&'static str, Vec<String>>,
    /// Compiled [[color_index]] rules: (patterns, style patch); the
    /// first matching rule colors the line.
    pub index_rules: Vec<(Vec<Pattern>, ratatui::style::Style)>,
    /// The left mailbox pane: (spec, new/unseen count) entries.
    pub sidebar: Vec<(String, usize)>,
    pub sidebar_sel: usize,
    /// Which entry is the open mailbox (for its > marker).
    pub sidebar_open: Option<usize>,
    pub sidebar_visible: bool,
    /// Ctrl+L: clear and repaint before the next draw.
    redraw: bool,
    /// `!`: a shell command to run with the TUI stood down.
    pending_shell: Option<String>,
    /// Ctrl+Z: stop, and pick the terminal back up on SIGCONT.
    pending_suspend: bool,
    /// `;` was pressed: the next flag operation applies to tagged messages.
    tag_next: bool,
    quit: bool,
}

impl App {
    /// mutt's $wrap: the effective text width inside `width` columns
    /// (positive = wrap there, negative = a right margin).
    pub(crate) fn pager_wrap(&self, width: usize) -> usize {
        match self.session.config.pager.wrap {
            Some(n) if n > 0 => (n as usize).min(width),
            Some(n) if n < 0 => width.saturating_sub(n.unsigned_abs() as usize).max(20),
            _ => width,
        }
    }
}

/// An rmut action name or a mutt function name, resolved to the rmut
/// name the key tables use. None when the menu has no such function.
fn resolve_function(menu: command::Menu, name: &str) -> Option<String> {
    if menu == command::Menu::Index {
        if IndexAction::from_name(name).is_some() {
            return Some(name.to_string());
        }
        let mapped = rmut_core::muttrc::index_function(name)?;
        IndexAction::from_name(mapped).map(|_| mapped.to_string())
    } else {
        if PagerAction::from_name(name).is_some() {
            return Some(name.to_string());
        }
        let mapped = rmut_core::muttrc::pager_function(name)?;
        PagerAction::from_name(mapped).map(|_| mapped.to_string())
    }
}

/// Everything the running app derives from [`Config`]: compiled rules,
/// the theme, and the key tables. Kept in one place so `:` commands can
/// rebuild it after changing the config, exactly as startup built it.
struct Derived {
    theme: Theme,
    keymap: Keymap,
    index_rules: Vec<(Vec<Pattern>, ratatui::style::Style)>,
    body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
}

impl Derived {
    /// Compile the config, collecting warnings for anything unusable
    /// (a bad pattern, an unknown color) rather than failing.
    fn from_config(config: &Config) -> (Derived, Vec<String>) {
        let (theme, mut warnings) = Theme::from_config(config);
        let (keymap, key_warnings) = Keymap::with_config(
            &config.keys.index,
            &config.keys.pager,
            &config.macros.index,
            &config.macros.pager,
        );
        warnings.extend(key_warnings);
        // Compile [[color_index]] rules; broken ones warn and drop.
        let mut index_rules = Vec::new();
        for rule in &config.color_index {
            let patterns = match pattern::parse(&rule.pattern) {
                Ok(p) => p,
                Err(err) => {
                    warnings.push(format!("bad color_index pattern {:?}: {err}", rule.pattern));
                    continue;
                }
            };
            let mut style = ratatui::style::Style::new();
            for (name, is_fg) in [(&rule.fg, true), (&rule.bg, false)] {
                if let Some(name) = name {
                    match crate::theme::parse_color(name) {
                        Some(color) if is_fg => style = style.fg(color),
                        Some(color) => style = style.bg(color),
                        None => warnings.push(format!("unknown color_index color {name:?}")),
                    }
                }
            }
            index_rules.push((patterns, style));
        }
        // Compile [[color_body]] rules (plain regexes) the same way.
        let mut body_rules = Vec::new();
        for rule in &config.color_body {
            let re = match regex_lite::Regex::new(&rule.pattern) {
                Ok(re) => re,
                Err(err) => {
                    warnings.push(format!("bad color_body regex {:?}: {err}", rule.pattern));
                    continue;
                }
            };
            let mut style = ratatui::style::Style::new();
            for (name, is_fg) in [(&rule.fg, true), (&rule.bg, false)] {
                if let Some(name) = name {
                    match crate::theme::parse_color(name) {
                        Some(color) if is_fg => style = style.fg(color),
                        Some(color) => style = style.bg(color),
                        None => warnings.push(format!("unknown color_body color {name:?}")),
                    }
                }
            }
            body_rules.push((re, style));
        }
        (
            Derived {
                theme,
                keymap,
                index_rules,
                body_rules,
            },
            warnings,
        )
    }
}

impl App {
    /// Wrap an open session in a front end: the menus, the key
    /// tables, the theme and everything else that only matters
    /// because there is a screen.
    pub fn new(session: Session, mut warnings: Vec<String>) -> Self {
        let (derived, config_warnings) = Derived::from_config(&session.config);
        let Derived {
            theme,
            keymap,
            index_rules,
            body_rules,
        } = derived;
        // The config's own complaints come first: they are about the
        // setup, and the session's are about this mailbox.
        let mut all = config_warnings;
        all.append(&mut warnings);
        let sidebar_visible = session.config.sidebar.visible;
        let notices = MessageLine::default();
        notices.0.borrow_mut().beep = session.config.ui.beep;
        let mut app = App {
            session,
            index_offset: 0,
            mode: Mode::Index,
            prompt: None,
            notices: notices.clone(),
            pager_search: None,
            pager_search_text: String::new(),
            body_rules,
            exit_notes: Vec::new(),
            view_size: (80, 24),
            theme,
            keymap,
            pending_editor: None,
            pending_raw_edit: None,
            complete: None,
            pending_keys: std::collections::VecDeque::new(),
            history: HashMap::new(),
            index_rules,
            sidebar: Vec::new(),
            sidebar_sel: 0,
            sidebar_open: None,
            sidebar_visible,
            redraw: false,
            pending_shell: None,
            pending_suspend: false,
            tag_next: false,
            quit: false,
        };
        app.session.install_notices(Box::new(notices));
        if !all.is_empty() {
            app.note(all.join("; "));
        }
        app
    }

    /// Put a session's question on the message line, and carry out
    /// anything it asked the front end to do.
    fn open_ask(&mut self, ask: Option<Ask>) {
        match ask {
            Some(Ask::Line {
                label,
                prefill,
                wants,
                what,
            }) => {
                self.prompt = Some(Prompt::line(label, prefill, LineKind::Ask { what, wants }));
            }
            Some(Ask::Key { label, what }) => {
                self.prompt = Some(Prompt::Key {
                    label,
                    kind: KeyKind::Ask(what),
                });
            }
            None => {}
        }
        self.run_requests_quietly();
    }

    /// What the session cannot do for itself. Hands back anything
    /// worth saying, for the caller to fold into its own report.
    fn run_requests(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        while let Some(request) = self.session.take_request() {
            match request {
                Request::Quit => self.quit = true,
                Request::ConfigChanged => warnings.extend(self.recompile_ui()),
                Request::Editor(compose) => self.pending_editor = Some(compose),
                Request::Shell(command) => self.pending_shell = Some(command),
                Request::Suspend => self.pending_suspend = true,
                Request::ShowDraft => self.open_compose_menu(),
                Request::Command(cmd) => {
                    let outcome = match &cmd {
                        command::Command::Push(seq) => self.push_command(seq),
                        command::Command::Exec(function) => self.exec_command(function),
                        _ => self.bind_command(&cmd),
                    };
                    match outcome {
                        Ok(Some(text)) => warnings.push(text),
                        Ok(None) => {}
                        Err(err) => self.error(err),
                    }
                }
            }
        }
        warnings
    }

    /// Honour whatever the session asked for, saying nothing: for
    /// the callers that have no report to fold warnings into.
    fn run_requests_quietly(&mut self) {
        let warnings = self.run_requests();
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
    }

    /// One of the draft's headers, edited from the compose menu.
    fn ask_header(&mut self, name: &str) {
        let ask = self.session.ask_header(name);
        self.open_ask(ask);
    }

    /// mutt asks before a new message when there are postponed
    /// drafts, since recalling one is a menu rather than an answer.
    fn start_compose(&mut self, kind: ComposeKind) {
        if kind == ComposeKind::New && self.session.has_postponed() {
            self.prompt = Some(Prompt::Key {
                label: "(n)ew message or (r)ecall postponed? ".into(),
                kind: KeyKind::Recall,
            });
            return;
        }
        let ask = self.session.start_compose(kind);
        self.open_ask(ask);
    }

    /// Something worth saying that is not a complaint.
    pub(crate) fn note(&mut self, msg: impl Into<String>) {
        self.session.note(msg);
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.session.error(msg);
    }

    /// The last thing said, for the message line and for the callers
    /// that only speak up when nothing else has.
    pub(crate) fn notice(&self) -> Option<Notice> {
        self.notices.0.borrow().latest.clone()
    }

    fn clear_notice(&mut self) {
        self.session.clear_notice();
    }

    /// Open a mailbox by spec: an `imap:account[/folder]` string (the
    /// folder is mirrored into a cache maildir) or a local path.
    pub fn open_spec(spec: &str, config: Config) -> Result<Self> {
        let (session, warnings) = Session::open_spec(spec, config, Box::new(progress))?;
        let mut app = App::new(session, warnings);
        app.refresh_sidebar();
        // A huge IMAP folder mirrors its tail in the background.
        app.session.maybe_backfill();
        Ok(app)
    }

    pub fn run(&mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let poll_every =
            Duration::from_secs(self.session.config.mail.poll_seconds.unwrap_or(5).max(1));
        let mut last_poll = Instant::now();
        while !self.quit {
            if PROGRESS_DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed)
                || mem::take(&mut self.redraw)
            {
                terminal.clear()?;
            }
            self.session.sync_message_hooks();
            self.session.tick_outbox();
            // A send that failed hands its draft back, and the hooks
            // may have asked for something too.
            self.run_requests_quietly();
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            // Macro-queued keys run first, without waiting for input.
            let key = match self.pending_keys.pop_front() {
                Some(key) => Some(key),
                None => {
                    if event::poll(Duration::from_millis(1000))?
                        && let Event::Key(key) = event::read()?
                        && key.kind == KeyEventKind::Press
                    {
                        Some(key)
                    } else {
                        None
                    }
                }
            };
            if let Some(key) = key {
                let size = terminal.size()?;
                self.clear_notice();
                self.handle_key(key, size.width as usize, size.height as usize);
            }
            // The IDLE watcher makes server changes show up within a
            // loop tick instead of waiting out the poll interval.
            let idle_kick = self.session.idle_kick();
            if idle_kick || last_poll.elapsed() >= poll_every {
                last_poll = Instant::now();
                self.check_new_mail();
            }
            if let Some(compose) = self.pending_editor.take() {
                self.edit_draft(&mut terminal, compose);
            }
            if let Some(path) = self.pending_raw_edit.take() {
                self.edit_raw(&mut terminal, path);
            }
            if let Some(command) = self.pending_shell.take() {
                self.run_shell(&mut terminal, &command);
            }
            if mem::take(&mut self.pending_suspend) {
                self.suspend(&mut terminal);
            }
        }
        // Whatever is still inside its $undo_send window goes out now:
        // quitting is not cancelling.
        self.exit_notes.extend(self.session.flush_outbox());
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, width: usize, height: usize) {
        // Rows available for content: total minus the help bar, the
        // status bar and the message line.
        let page = height.saturating_sub(3).max(1);
        self.view_size = (width, page);
        // Ctrl+L repaints from the menus that have no keymap of their
        // own; the index and pager route it through theirs, so it can
        // be rebound there.
        if is_ctrl(&key)
            && key.code == KeyCode::Char('l')
            && self.prompt.is_none()
            && !matches!(self.mode, Mode::Index | Mode::Pager(_))
        {
            self.redraw = true;
            return;
        }
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
        } else if matches!(self.mode, Mode::Index) {
            self.handle_index_key(key, page);
        } else if matches!(self.mode, Mode::Pager(_)) {
            self.handle_pager_key(key, width, page);
        } else if matches!(self.mode, Mode::Compose { .. }) {
            self.handle_compose_key(key);
        } else if matches!(self.mode, Mode::Attach { .. }) {
            self.handle_attach_key(key);
        } else if matches!(self.mode, Mode::Help { .. }) {
            self.handle_help_key(key, page);
        } else if matches!(self.mode, Mode::Postponed { .. }) {
            self.handle_postponed_key(key);
        } else if matches!(self.mode, Mode::Query { .. }) {
            self.handle_query_key(key);
        } else {
            self.handle_folders_key(key);
        }
    }

    // ---- new-mail detection ----

    /// The session's check, with the sidebar counts redrawn around it.
    fn check_new_mail(&mut self) {
        self.session.check_new_mail(&mut |session| {
            let _ = session;
        });
        self.refresh_sidebar();
    }

    /// Rebuild the sidebar entries: the configured mailboxes with
    /// their new/unseen counts (local: new/ scan; the open account's
    /// IMAP folders: STATUS), the open mailbox always included.
    fn refresh_sidebar(&mut self) {
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
        let open = |e: &(String, usize)| e.0 == title || expand_tilde(&e.0) == dir;
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

    // ---- help ----

    fn open_help(&mut self) {
        self.mode = Mode::Help {
            lines: self.keymap.help_lines(),
            scroll: 0,
        };
    }

    fn handle_help_key(&mut self, key: KeyEvent, page: usize) {
        let Mode::Help { lines, scroll } = &mut self.mode else {
            return;
        };
        let max_scroll = lines.len().saturating_sub(page);
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('i') | KeyCode::Esc | KeyCode::Char('?') => {
                self.mode = Mode::Index;
                // Leaving the attachment review resumes the send flow.
                if self.session.draft().is_some() {
                    self.open_compose_menu();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => *scroll = (*scroll + 1).min(max_scroll),
            KeyCode::Char('k') | KeyCode::Up => *scroll = scroll.saturating_sub(1),
            KeyCode::Char(' ') | KeyCode::PageDown => *scroll = (*scroll + page).min(max_scroll),
            KeyCode::Char('-') | KeyCode::PageUp => *scroll = scroll.saturating_sub(page),
            KeyCode::Home => *scroll = 0,
            KeyCode::End => *scroll = max_scroll,
            _ => {}
        }
    }

    // ---- prompts ----

    fn handle_prompt_key(&mut self, key: KeyEvent) {
        match &mut self.prompt {
            Some(Prompt::Key { kind, .. }) => {
                let kind = kind.clone();
                self.prompt = None;
                self.run_key_prompt(kind, key.code);
            }
            Some(Prompt::Line { buf, cursor, .. }) => match key.code {
                KeyCode::Esc => {
                    self.prompt = None;
                    self.session.cancel_setup();
                    // Escaping a sub-prompt of the send flow (attach
                    // file) returns to the compose menu.
                    if self.session.draft().is_some() {
                        self.open_compose_menu();
                    }
                }
                // The line editor, mutt/readline style.
                KeyCode::Left => *cursor = cursor.saturating_sub(1),
                KeyCode::Right => *cursor = (*cursor + 1).min(buf.chars().count()),
                KeyCode::Home => *cursor = 0,
                KeyCode::End => *cursor = buf.chars().count(),
                KeyCode::Char('a') if is_ctrl(&key) => *cursor = 0,
                KeyCode::Char('e') if is_ctrl(&key) => *cursor = buf.chars().count(),
                KeyCode::Backspace => {
                    if *cursor > 0 {
                        buf.remove(byte_at(buf, *cursor - 1));
                        *cursor -= 1;
                    }
                }
                KeyCode::Delete => {
                    if *cursor < buf.chars().count() {
                        buf.remove(byte_at(buf, *cursor));
                    }
                }
                KeyCode::Char('d') if is_ctrl(&key) => {
                    if *cursor < buf.chars().count() {
                        buf.remove(byte_at(buf, *cursor));
                    }
                }
                KeyCode::Char('u') if is_ctrl(&key) => {
                    // Kill to the start of the line.
                    let i = byte_at(buf, *cursor);
                    buf.replace_range(..i, "");
                    *cursor = 0;
                }
                KeyCode::Char('k') if is_ctrl(&key) => {
                    let i = byte_at(buf, *cursor);
                    buf.truncate(i);
                }
                KeyCode::Char('w') if is_ctrl(&key) => {
                    // Kill the word before the cursor.
                    let chars: Vec<char> = buf.chars().collect();
                    let mut c = *cursor;
                    while c > 0 && chars[c - 1].is_whitespace() {
                        c -= 1;
                    }
                    while c > 0 && !chars[c - 1].is_whitespace() {
                        c -= 1;
                    }
                    let (start, end) = (byte_at(buf, c), byte_at(buf, *cursor));
                    buf.replace_range(start..end, "");
                    *cursor = c;
                }
                KeyCode::Char(c) if !is_ctrl(&key) => {
                    buf.insert(byte_at(buf, *cursor), c);
                    *cursor += 1;
                }
                KeyCode::Up => self.history_step(true),
                KeyCode::Down => self.history_step(false),
                KeyCode::Tab => self.tab_complete(),
                KeyCode::Enter => {
                    if let Some(Prompt::Line { buf, kind, .. }) = self.prompt.take() {
                        let entry = buf.trim().to_string();
                        if !entry.is_empty() {
                            let bucket = self.history.entry(kind.history_bucket()).or_default();
                            bucket.retain(|e| e != &entry);
                            bucket.insert(0, entry);
                            bucket.truncate(100);
                        }
                        self.run_line_prompt(kind, buf.trim());
                    }
                }
                _ => {}
            },
            None => {}
        }
    }

    /// Up/Down at a line prompt: recall the kind's history (newest
    /// first); stepping back past the newest restores the line that
    /// was being typed.
    fn history_step(&mut self, older: bool) {
        let Some(Prompt::Line {
            buf,
            cursor,
            kind,
            hist_pos,
            stash,
            ..
        }) = &mut self.prompt
        else {
            return;
        };
        let bucket = self
            .history
            .get(kind.history_bucket())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if bucket.is_empty() {
            return;
        }
        let next = match (*hist_pos, older) {
            (None, true) => Some(0),
            (None, false) => return,
            (Some(p), true) => Some((p + 1).min(bucket.len() - 1)),
            (Some(0), false) => None,
            (Some(p), false) => Some(p - 1),
        };
        match next {
            Some(p) => {
                if hist_pos.is_none() {
                    *stash = buf.clone();
                }
                *buf = bucket[p].clone();
            }
            None => *buf = stash.clone(),
        }
        *hist_pos = next;
        *cursor = buf.chars().count();
    }

    fn run_key_prompt(&mut self, kind: KeyKind, code: KeyCode) {
        match kind {
            KeyKind::Ask(what) => {
                let key = match code {
                    KeyCode::Char(c) => Key::Char(c),
                    KeyCode::Enter => Key::Enter,
                    _ => Key::Other,
                };
                let next = self.session.answer(what, Answer::Key(key));
                self.open_ask(next);
            }
            KeyKind::Recall => match code {
                KeyCode::Char('r') => self.recall_postponed(),
                KeyCode::Char('n') => {
                    let ask = self.session.start_compose(ComposeKind::New);
                    self.open_ask(ask);
                }
                _ => {}
            },
            KeyKind::PrintPart => {
                if code == KeyCode::Char('y') {
                    self.print_part();
                }
            }
        }
    }

    /// Tab at a prompt: at an address prompt, complete the token under
    /// the cursor against aliases and query_command; at a mailbox
    /// prompt (c, save, copy), complete the buffer against the folder
    /// candidates, and with nothing typed at the c prompt, open the
    /// folder browser instead. Repeated Tab cycles the candidates.
    fn tab_complete(&mut self) {
        let (buf_now, kind) = match &self.prompt {
            Some(Prompt::Line { buf, kind, .. }) => (buf.clone(), kind.clone()),
            _ => return,
        };
        // A session question says what its answer is; the front end's
        // own prompts are classified here.
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
            } | LineKind::ChangeDir
                | LineKind::BrowseDir
        );
        if !is_addr && !is_mbox {
            return;
        }
        if matches!(kind, LineKind::ChangeDir) && buf_now.trim().is_empty() {
            self.prompt = None;
            self.complete = None;
            self.open_folder_browser();
            return;
        }
        let set_buf = |app: &mut App, text: &str| {
            if let Some(Prompt::Line { buf, cursor, .. }) = &mut app.prompt {
                *buf = text.to_string();
                *cursor = buf.chars().count();
            }
        };
        if let Some(c) = &mut self.complete
            && c.expect == buf_now
            && c.candidates.len() > 1
        {
            c.index = (c.index + 1) % c.candidates.len();
            let next = format!("{}{}", &buf_now[..c.start], c.candidates[c.index]);
            c.expect = next.clone();
            let note = format!("match {}/{}", c.index + 1, c.candidates.len());
            set_buf(self, &next);
            self.note(note);
            return;
        }
        self.complete = None;
        let after_comma = if is_addr {
            buf_now.rfind(',').map(|i| i + 1).unwrap_or(0)
        } else {
            0
        };
        let start =
            after_comma + buf_now[after_comma..].len() - buf_now[after_comma..].trim_start().len();
        let word = buf_now[start..].trim().to_string();
        if word.is_empty() {
            self.error("nothing to complete");
            return;
        }
        let candidates = if is_addr {
            alias::complete(
                &word,
                &alias::load_default(),
                self.session.config.mail.query_command.as_deref(),
            )
        } else {
            let specs = match self.session.folder_candidates() {
                Ok(specs) => specs,
                Err(err) => {
                    self.error(format!("cannot list folders: {err:#}"));
                    return;
                }
            };
            // Match the typed prefix against the spec as written and
            // tilde-expanded, so ~/Mail and /home/jane/Mail both hit.
            let wexp = expand_tilde(&word).display().to_string();
            specs
                .into_iter()
                .map(|(spec, _)| spec)
                .filter(|s| {
                    s.starts_with(&word) || expand_tilde(s).display().to_string().starts_with(&wexp)
                })
                .collect()
        };
        match candidates.len() {
            0 => self.error(format!("no matches for {word}")),
            n => {
                let next = format!("{}{}", &buf_now[..start], candidates[0]);
                set_buf(self, &next);
                if n > 1 {
                    self.note(format!("match 1/{n} (Tab cycles)"));
                }
                self.complete = Some(Complete {
                    start,
                    candidates,
                    index: 0,
                    expect: next,
                });
            }
        }
    }

    fn run_line_prompt(&mut self, kind: LineKind, input: &str) {
        // mutt's +x / =x: a mailbox under $folder, wherever one is
        // typed. Macros come through here too, which is what makes an
        // imported "<save-message>=archive<enter>" work.
        let expanded;
        let input = match kind.takes_mailbox() {
            true => {
                expanded = rmut_core::config::expand_folder(
                    input,
                    self.session.config.mail.folder.as_deref(),
                );
                expanded.as_str()
            }
            false => input,
        };
        match kind {
            LineKind::Ask { what, .. } => {
                let next = self.session.answer(what, Answer::Line(input));
                self.open_ask(next);
            }
            LineKind::PagerSearch => {
                if !input.is_empty() {
                    self.pager_search = Some(pattern::Matcher::new(input));
                    self.pager_search_text = input.to_string();
                }
                if self.pager_search.is_some() {
                    self.pager_search_step(true);
                } else {
                    self.error("No search pattern.");
                }
            }
            LineKind::ChangeDir => self.open_mailbox_spec(input),
            LineKind::ChangeDirReadOnly => self.open_mailbox_read_only(input),
            LineKind::BrowseDir => self.browse_dir(input),
            LineKind::CreateDir => self.create_maildir(input),
            LineKind::SavePart => self.save_part(input),
            LineKind::PipePart => self.pipe_part(input),
            LineKind::Query => self.run_query(input),
            LineKind::Notmuch => self.notmuch_search(input),
            LineKind::EnterCommand => self.run_command_line(input),
        }
    }

    // ---- index ----

    fn handle_index_key(&mut self, key: KeyEvent, page: usize) {
        if let Some(seq) = self.keymap.lookup_index_macro(&key) {
            self.replay(seq.to_vec());
            return;
        }
        let Some(action) = self.keymap.lookup_index(&key) else {
            self.tag_next = false;
            return;
        };
        let apply_tagged = mem::take(&mut self.tag_next);
        self.run_index_action(action, apply_tagged, page);
    }

    /// One index action, however it arrived: a key, a macro replay, or
    /// `:exec`.
    fn run_index_action(&mut self, action: IndexAction, apply_tagged: bool, page: usize) {
        // Any new action ends a previous `;` operation, including one
        // whose prompt was abandoned with Esc.
        if apply_tagged && !takes_tagged(action) {
            self.error(format!("{} does not take the tagged set", action.name()));
            return;
        }
        match action {
            IndexAction::Tag => {
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("tag", &[i]);
                    self.session.msgs[i].env.tagged = !self.session.msgs[i].env.tagged;
                    self.session.select(self.session.sel.saturating_add(1));
                }
            }
            IndexAction::Undo => self.undo_last(),
            IndexAction::DeleteThread => self.session.thread_mark(false, ThreadOp::Delete),
            IndexAction::UndeleteThread => self.session.thread_mark(false, ThreadOp::Undelete),
            IndexAction::TagThread => self.session.thread_mark(false, ThreadOp::Tag),
            IndexAction::DeleteSubthread => self.session.thread_mark(true, ThreadOp::Delete),
            IndexAction::UndeleteSubthread => self.session.thread_mark(true, ThreadOp::Undelete),
            IndexAction::NextThread => self.session.jump_thread(true),
            IndexAction::PrevThread => self.session.jump_thread(false),
            IndexAction::TagPrefix => {
                if self.session.msgs.iter().any(|m| m.env.tagged) {
                    self.tag_next = true;
                    // mutt writes "Tag-" on its message line and
                    // waits; rmut's one bottom line appends it to the
                    // status bar, which then stays readable.
                    self.note("Tag-");
                } else {
                    self.note("no tagged messages");
                }
            }
            IndexAction::FetchMail => {
                self.check_new_mail();
                if self.notice().is_none() {
                    self.note("checked for new mail");
                }
            }
            IndexAction::Save | IndexAction::Copy => {
                let ask = self
                    .session
                    .ask_copy(action == IndexAction::Save, apply_tagged);
                self.open_ask(ask);
            }
            IndexAction::Pipe => {
                let ask = self.session.ask_pipe(apply_tagged);
                self.open_ask(ask);
            }
            IndexAction::Bounce => {
                let ask = self.session.ask_bounce(apply_tagged);
                self.open_ask(ask);
            }
            IndexAction::Resend => self.resend_current(),
            IndexAction::Edit => self.start_raw_edit(),
            IndexAction::CreateAlias => {
                let ask = self.session.ask_alias();
                self.open_ask(ask);
            }
            IndexAction::Query => {
                if self.session.config.mail.query_command.is_none() {
                    self.error("no query_command configured");
                } else {
                    self.prompt = Some(Prompt::line("Query: ", String::new(), LineKind::Query));
                }
            }
            IndexAction::Notmuch => {
                if self.session.config.mail.notmuch == Some(false) {
                    self.error("notmuch is disabled in the config");
                } else {
                    self.prompt = Some(Prompt::line(
                        "Notmuch query: ",
                        String::new(),
                        LineKind::Notmuch,
                    ));
                }
            }
            IndexAction::Quit => {
                // Like mutt: flag changes are written silently; only
                // pending deletions raise a question (the purge one).
                self.session.mark_old_unread();
                if self.session.deleted_count() > 0 {
                    let ask = self.session.ask_purge(true);
                    self.open_ask(ask);
                } else {
                    if self.session.pending_count() > 0 {
                        self.session.sync(true);
                    }
                    self.quit = true;
                }
            }
            IndexAction::Abort => self.quit = true,
            IndexAction::Down => self.session.select(self.session.sel.saturating_add(1)),
            IndexAction::Up => self.session.select(self.session.sel.saturating_sub(1)),
            IndexAction::PageDown => self.session.select(self.session.sel.saturating_add(page)),
            IndexAction::PageUp => self.session.select(self.session.sel.saturating_sub(page)),
            IndexAction::First => self.session.select(0),
            IndexAction::Last => self.session.select(usize::MAX),
            IndexAction::View => self.open_selected(),
            IndexAction::FoldThread => self.session.toggle_collapse(false),
            IndexAction::FoldAll => self.session.toggle_collapse(true),
            IndexAction::Delete => {
                if self.session.deny_readonly() {
                } else if apply_tagged {
                    self.session
                        .each_tagged("delete", |m| m.env.file.flags.deleted = true);
                } else if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("delete", &[i]);
                    self.session.msgs[i].env.file.flags.deleted = true;
                    self.session.msgs[i].dirty = true;
                    self.session.select(self.session.sel.saturating_add(1));
                }
            }
            IndexAction::Undelete => {
                if self.session.deny_readonly() {
                } else if apply_tagged {
                    self.session
                        .each_tagged("undelete", |m| m.env.file.flags.deleted = false);
                } else if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("undelete", &[i]);
                    self.session.msgs[i].env.file.flags.deleted = false;
                    self.session.msgs[i].dirty = true;
                    // mutt's $resolve (on by default): advance.
                    self.session.select(self.session.sel.saturating_add(1));
                }
            }
            IndexAction::Flag => {
                if self.session.deny_readonly() {
                } else if apply_tagged {
                    self.session.each_tagged("flag", |m| {
                        m.env.file.flags.flagged = !m.env.file.flags.flagged
                    });
                } else if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("flag", &[i]);
                    let flags = &mut self.session.msgs[i].env.file.flags;
                    flags.flagged = !flags.flagged;
                    self.session.msgs[i].dirty = true;
                    self.session.select(self.session.sel.saturating_add(1));
                }
            }
            IndexAction::ToggleNew => {
                if self.session.deny_readonly() {
                } else if apply_tagged {
                    self.session.each_tagged("toggle read", |m| {
                        m.env.file.flags.seen = !m.env.file.flags.seen;
                        m.env.file.is_new = false;
                    });
                } else if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("toggle read", &[i]);
                    let file = &mut self.session.msgs[i].env.file;
                    file.flags.seen = !file.flags.seen;
                    file.is_new = false;
                    self.session.msgs[i].dirty = true;
                    self.session.select(self.session.sel.saturating_add(1));
                }
            }
            IndexAction::Sync => {
                if self.session.deleted_count() > 0 {
                    let ask = self.session.ask_purge(false);
                    self.open_ask(ask);
                } else {
                    self.session.sync(true);
                }
            }
            IndexAction::Compose => self.start_compose(ComposeKind::New),
            IndexAction::Reply => self.start_compose(ComposeKind::Reply),
            IndexAction::GroupReply => self.start_compose(ComposeKind::GroupReply),
            IndexAction::ListReply => {
                let ask = self.session.start_list_reply();
                self.open_ask(ask);
            }
            IndexAction::Forward => self.start_compose(ComposeKind::Forward),
            IndexAction::Sort => {
                let ask = self.session.ask_sort();
                self.open_ask(Some(ask));
            }
            IndexAction::Limit => {
                let ask = self.session.ask_limit();
                self.open_ask(Some(ask));
            }
            IndexAction::Search => {
                let ask = self.session.ask_search(false);
                self.open_ask(Some(ask));
            }
            IndexAction::SearchReverse => {
                let ask = self.session.ask_search(true);
                self.open_ask(Some(ask));
            }
            IndexAction::SearchNext => self.session.search_next(),
            IndexAction::NextNew => self.session.jump_new(true),
            IndexAction::PrevNew => self.session.jump_new(false),
            IndexAction::DeletePattern => {
                let ask = self.session.ask_pattern(PatternOp::Delete);
                self.open_ask(ask);
            }
            IndexAction::UndeletePattern => {
                let ask = self.session.ask_pattern(PatternOp::Undelete);
                self.open_ask(ask);
            }
            IndexAction::TagPattern => {
                let ask = self.session.ask_pattern(PatternOp::Tag);
                self.open_ask(ask);
            }
            IndexAction::UntagPattern => {
                let ask = self.session.ask_pattern(PatternOp::Untag);
                self.open_ask(ask);
            }
            IndexAction::Attachments => self.open_attachments(),
            IndexAction::ChangeMailbox => {
                if self.session.ready_to_leave() {
                    self.prompt = Some(Prompt::line(
                        "Open mailbox (Tab completes): ",
                        String::new(),
                        LineKind::ChangeDir,
                    ));
                }
            }
            IndexAction::ChangeMailboxReadOnly => {
                if self.session.ready_to_leave() {
                    self.prompt = Some(Prompt::line(
                        "Open mailbox read-only: ",
                        String::new(),
                        LineKind::ChangeDirReadOnly,
                    ));
                }
            }
            IndexAction::Shell => {
                let ask = self.session.ask_shell();
                self.open_ask(Some(ask));
            }
            IndexAction::Redraw => self.redraw = true,
            IndexAction::Suspend => {
                self.session.request_suspend();
                self.run_requests_quietly();
            }
            IndexAction::Folders => self.open_folder_browser(),
            IndexAction::Print => {
                let ask = self.session.ask_print(apply_tagged);
                self.open_ask(ask);
            }
            IndexAction::SidebarToggle => {
                self.sidebar_visible = !self.sidebar_visible;
                self.refresh_sidebar();
            }
            IndexAction::SidebarNext | IndexAction::SidebarPrev => {
                if !self.sidebar_visible {
                    self.note("the sidebar is hidden; B shows it");
                } else if !self.sidebar.is_empty() {
                    self.sidebar_sel = if action == IndexAction::SidebarNext {
                        (self.sidebar_sel + 1).min(self.sidebar.len() - 1)
                    } else {
                        self.sidebar_sel.saturating_sub(1)
                    };
                }
            }
            IndexAction::SidebarOpen => {
                if !self.sidebar_visible {
                    self.note("the sidebar is hidden; B shows it");
                } else if self.session.ready_to_leave()
                    && let Some((spec, _)) = self.sidebar.get(self.sidebar_sel).cloned()
                {
                    self.open_mailbox_spec(&spec);
                }
            }
            IndexAction::EnterCommand => self.open_command_prompt(),
            IndexAction::Help => self.open_help(),
        }
    }

    // ---- pager ----

    /// Is the pager showing a single attachment part (not a message)?
    /// Message-motion keys refuse there, like mutt's "Not available".
    fn part_pager(&self) -> bool {
        matches!(&self.mode, Mode::Pager(p) if p.back.is_some())
    }

    fn handle_pager_key(&mut self, key: KeyEvent, width: usize, page: usize) {
        // $wrap narrows the text, so all row math follows it.
        let width = self.pager_wrap(width);
        if let Some(seq) = self.keymap.lookup_pager_macro(&key) {
            self.replay(seq.to_vec());
            return;
        }
        let Some(action) = self.keymap.lookup_pager(&key) else {
            return;
        };
        self.run_pager_action(action, width, page);
    }

    /// One pager action, however it arrived (key, macro, or `:exec`).
    fn run_pager_action(&mut self, action: PagerAction, width: usize, page: usize) {
        match action {
            PagerAction::Back => {
                // A part view returns to its attachment menu.
                if let Mode::Pager(p) = &mut self.mode
                    && let Some(menu) = p.back.take()
                {
                    self.mode = *menu;
                } else {
                    self.mode = Mode::Index;
                }
                return;
            }
            PagerAction::NextMsg | PagerAction::NextUndeleted => {
                if self.part_pager() {
                    self.error("Not available in this menu.");
                    return;
                }
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
                return;
            }
            PagerAction::PrevMsg | PagerAction::PrevUndeleted => {
                if self.part_pager() {
                    self.error("Not available in this menu.");
                    return;
                }
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
                return;
            }
            PagerAction::Delete => {
                if self.part_pager() {
                    self.error("Not available in this menu.");
                    return;
                }
                if self.session.deny_readonly() {
                    return;
                }
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("delete", &[i]);
                    self.session.msgs[i].env.file.flags.deleted = true;
                    self.session.msgs[i].dirty = true;
                }
                // mutt's $resolve: advance to the next undeleted.
                match self.session.step_message(true, true) {
                    Some(pos) => {
                        self.session.sel = pos;
                        self.open_selected();
                    }
                    None => self.mode = Mode::Index,
                }
                return;
            }
            PagerAction::Undelete | PagerAction::Flag | PagerAction::ToggleNew => {
                if self.part_pager() {
                    self.error("Not available in this menu.");
                    return;
                }
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
                return;
            }
            PagerAction::Tag => {
                if self.part_pager() {
                    self.error("Not available in this menu.");
                    return;
                }
                if let Some(&i) = self.session.visible.get(self.session.sel) {
                    self.session.push_undo("tag", &[i]);
                    self.session.msgs[i].env.tagged = !self.session.msgs[i].env.tagged;
                }
                return;
            }
            PagerAction::Redraw => {
                self.redraw = true;
                return;
            }
            PagerAction::Suspend => {
                self.session.request_suspend();
                self.run_requests_quietly();
                return;
            }
            PagerAction::Undo => {
                self.undo_last();
                return;
            }
            PagerAction::Search => {
                self.prompt = Some(Prompt::line(
                    "Search for: ",
                    self.pager_search_text.clone(),
                    LineKind::PagerSearch,
                ));
                return;
            }
            PagerAction::SearchNext => {
                self.pager_search_step(true);
                return;
            }
            PagerAction::SearchPrev => {
                self.pager_search_step(false);
                return;
            }
            PagerAction::Attachments => {
                self.open_attachments();
                return;
            }
            PagerAction::Compose => {
                self.start_compose(ComposeKind::New);
                return;
            }
            PagerAction::Reply => {
                self.start_compose(ComposeKind::Reply);
                return;
            }
            PagerAction::GroupReply => {
                self.start_compose(ComposeKind::GroupReply);
                return;
            }
            PagerAction::ListReply => {
                let ask = self.session.start_list_reply();
                self.open_ask(ask);
                return;
            }
            PagerAction::Forward => {
                self.start_compose(ComposeKind::Forward);
                return;
            }
            PagerAction::Print => {
                let ask = self.session.ask_print(false);
                self.open_ask(ask);
                return;
            }
            PagerAction::Save => {
                let ask = self.session.ask_copy(true, false);
                self.open_ask(ask);
                return;
            }
            PagerAction::Copy => {
                let ask = self.session.ask_copy(false, false);
                self.open_ask(ask);
                return;
            }
            PagerAction::Pipe => {
                let ask = self.session.ask_pipe(false);
                self.open_ask(ask);
                return;
            }
            PagerAction::Bounce => {
                let ask = self.session.ask_bounce(false);
                self.open_ask(ask);
                return;
            }
            PagerAction::Resend => {
                self.resend_current();
                return;
            }
            PagerAction::Edit => {
                self.mode = Mode::Index;
                self.start_raw_edit();
                return;
            }
            PagerAction::CreateAlias => {
                let ask = self.session.ask_alias();
                self.open_ask(ask);
                return;
            }
            PagerAction::EnterCommand => {
                self.open_command_prompt();
                return;
            }
            PagerAction::Help => {
                self.open_help();
                return;
            }
            _ => {}
        }
        // The mini-index (pager.index_lines) shrinks the pager
        // viewport; pager.context keeps overlap when paging.
        let page = page
            .saturating_sub(self.session.config.pager.index_lines as usize)
            .max(1);
        let step = page
            .saturating_sub(self.session.config.pager.context)
            .max(1);
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let lines = crate::ui::pager_line_count(
            &pager.view,
            width,
            pager.full_headers,
            &self.session.quote_re,
            pager.hide_quoted,
        );
        let max_scroll = lines.saturating_sub(page);
        // mutt's $pager_stop = no: paging past the end opens the next
        // message, except in a part view, which returns to its
        // attachment menu like mutt.
        if action == PagerAction::PageDown && pager.scroll >= max_scroll {
            if let Some(menu) = pager.back.take() {
                self.mode = *menu;
                return;
            }
            // mutt falls through to next-undeleted here.
            match self.session.step_message(true, true) {
                Some(pos) => {
                    self.session.sel = pos;
                    self.open_selected();
                }
                None => self.error("last message"),
            }
            return;
        }
        match action {
            PagerAction::Headers => {
                pager.full_headers = !pager.full_headers;
                pager.scroll = 0;
            }
            // max(scroll): a search may have over-scrolled past
            // max_scroll; stay put rather than jump back up.
            PagerAction::Down => {
                pager.scroll = (pager.scroll + 1).min(max_scroll.max(pager.scroll))
            }
            PagerAction::Up => pager.scroll = pager.scroll.saturating_sub(1),
            PagerAction::PageDown => pager.scroll = (pager.scroll + step).min(max_scroll),
            PagerAction::PageUp => pager.scroll = pager.scroll.saturating_sub(step),
            PagerAction::HalfDown => {
                pager.scroll = (pager.scroll + (page / 2).max(1)).min(max_scroll)
            }
            PagerAction::HalfUp => pager.scroll = pager.scroll.saturating_sub((page / 2).max(1)),
            PagerAction::Top => pager.scroll = 0,
            PagerAction::Bottom => pager.scroll = max_scroll,
            PagerAction::ToggleQuoted => {
                pager.hide_quoted = !pager.hide_quoted;
                // The row count changed: keep the scroll in range.
                let lines = crate::ui::pager_line_count(
                    &pager.view,
                    width,
                    pager.full_headers,
                    &self.session.quote_re,
                    pager.hide_quoted,
                );
                pager.scroll = pager.scroll.min(lines.saturating_sub(page));
            }
            PagerAction::SkipQuoted => {
                let rows = crate::ui::pager_rows(
                    &pager.view,
                    width,
                    pager.full_headers,
                    &self.session.quote_re,
                    pager.hide_quoted,
                );
                let quoted = |i: usize| matches!(rows[i].kind, crate::ui::RowKind::Quoted(_));
                // Past the next quoted block: find it, then leave it.
                let mut i = pager.scroll;
                while i < rows.len() && !quoted(i) {
                    i += 1;
                }
                while i < rows.len() && quoted(i) {
                    i += 1;
                }
                if i < rows.len() {
                    pager.scroll = i.min(max_scroll.max(pager.scroll));
                } else {
                    self.error("No more quoted text.");
                }
            }
            _ => {}
        }
    }

    /// Move the pager to the next (or previous) line matching the
    /// stored search, wrapping around with a note; the hit becomes the
    /// top line, like mutt.
    fn pager_search_step(&mut self, forward: bool) {
        let Some(matcher) = self.pager_search.clone() else {
            // n/N with nothing searched yet: ask for the pattern
            // first (mutt falls through to search the same way).
            self.prompt = Some(Prompt::line(
                "Search for: ",
                self.pager_search_text.clone(),
                LineKind::PagerSearch,
            ));
            return;
        };
        let width = self.pager_wrap(self.view_size.0);
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let lines = crate::ui::pager_text_lines(
            &pager.view,
            width,
            pager.full_headers,
            &self.session.quote_re,
            pager.hide_quoted,
        );
        match search_lines(&lines, &matcher, pager.scroll, forward) {
            Some((hit, wrapped)) => {
                // The hit becomes the top line even near the end (past
                // max_scroll), like mutt; otherwise close-to-the-end
                // hits would be indistinguishable and n would stall.
                pager.scroll = hit;
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

    // ---- attachments ----

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
            KeyCode::Char('|') => {
                self.prompt = Some(Prompt::line(
                    "Pipe part to command: ",
                    String::new(),
                    LineKind::PipePart,
                ));
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
                    self.prompt = Some(Prompt::line("Save to file: ", default, LineKind::SavePart));
                }
            }
            _ => {}
        }
    }

    fn open_attachments(&mut self) {
        let Some(&i) = self.session.visible.get(self.session.sel) else {
            return;
        };
        let msg_path = self.session.msgs[i].env.file.path.clone();
        match message::parts(&msg_path) {
            Ok(parts) if !parts.is_empty() => {
                let back = match mem::replace(&mut self.mode, Mode::Index) {
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

    fn view_part(&mut self) {
        // A fresh pager session, search-wise (see open_selected).
        self.pager_search = None;
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
        if !is_text {
            // A configured filter can still render it (auto_view).
            match self.session.display.filters.get(&mimetype).cloned() {
                Some(command) => {
                    match message::filter_part(&msg_path, index, &command) {
                        Ok(body) => {
                            let headers = vec![("Content-Type".to_string(), mimetype)];
                            let menu = std::mem::replace(&mut self.mode, Mode::Index);
                            self.mode = Mode::Pager(Pager {
                                view: message::MessageView {
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
                        Err(err) => self.error(format!("filter failed: {err:#}")),
                    }
                    return;
                }
                None => {
                    self.error(format!("{mimetype} is not text; save it with s"));
                    return;
                }
            }
        }
        match message::part_text(&msg_path, index) {
            Ok(body) => {
                let headers = vec![("Content-Type".to_string(), mimetype)];
                let menu = std::mem::replace(&mut self.mode, Mode::Index);
                self.mode = Mode::Pager(Pager {
                    view: message::MessageView {
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
            Err(err) => self.error(format!("cannot decode part: {err:#}")),
        }
    }

    /// The selected attachment's decoded bytes, from the menu state.
    fn selected_part_bytes(&mut self) -> Option<Vec<u8>> {
        let (msg_path, index) = match &self.mode {
            Mode::Attach { msg_path, sel, .. } => (msg_path.clone(), *sel),
            _ => return None,
        };
        match message::part_bytes(&msg_path, index) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                self.error(format!("cannot decode part: {err:#}"));
                None
            }
        }
    }

    /// `|` on the attachment menu: the decoded part to a command.
    fn pipe_part(&mut self, command: &str) {
        if command.is_empty() {
            self.error("no command given");
            return;
        }
        let Some(bytes) = self.selected_part_bytes() else {
            return;
        };
        match pipe_to(command, &bytes) {
            Ok(()) => self.note(format!("piped to {command}")),
            Err(err) => self.error(format!("pipe failed: {err:#}")),
        }
    }

    /// `p` on the attachment menu: the decoded part to print_command.
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
        match pipe_to(&command, &bytes) {
            Ok(()) => self.note(format!("printed via {command}")),
            Err(err) => self.error(format!("print failed: {err:#}")),
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
        let target = expand_tilde(input);
        if target.exists() {
            self.error(format!("{} exists, not overwriting", target.display()));
            return;
        }
        let result = message::part_bytes(&msg_path, index).and_then(|bytes| {
            std::fs::write(&target, &bytes)?;
            Ok(bytes.len())
        });
        match result {
            Ok(n) => self.note(format!("saved {n} bytes to {}", target.display())),
            Err(err) => self.error(format!("save failed: {err:#}")),
        }
    }

    // ---- folders ----

    fn handle_folders_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Folders { dirs, sel, .. } = &mut self.mode {
                    *sel = (*sel + 1).min(dirs.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Folders { sel, .. } = &mut self.mode {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
            KeyCode::Char('c') => {
                let buf = match &self.mode {
                    Mode::Folders {
                        root: Some(root), ..
                    } => root.display().to_string(),
                    _ => self
                        .session
                        .dir
                        .parent()
                        .unwrap_or(&self.session.dir)
                        .display()
                        .to_string(),
                };
                self.prompt = Some(Prompt::line("Browse directory: ", buf, LineKind::BrowseDir));
            }
            KeyCode::Char('C') => {
                self.prompt = Some(Prompt::line(
                    "Create maildir: ",
                    String::new(),
                    LineKind::CreateDir,
                ));
            }
            KeyCode::Enter => {
                let (spec, root) = match &self.mode {
                    Mode::Folders { dirs, sel, root } => {
                        (dirs.get(*sel).map(|d| d.0.clone()), root.clone())
                    }
                    _ => (None, None),
                };
                let Some(spec) = spec else { return };
                if spec == ".." {
                    if let Some(parent) = root.as_ref().and_then(|r| r.parent()) {
                        self.browse_dir(&parent.display().to_string());
                    }
                    return;
                }
                match spec.strip_suffix('/') {
                    // A plain directory: descend instead of opening.
                    Some(dir) => self.browse_dir(dir),
                    None => self.open_mailbox_spec(&spec),
                }
            }
            _ => {}
        }
    }

    /// List `input` in the folder browser: subdirectories only,
    /// maildirs openable with their new counts, plain directories
    /// marked with a trailing `/` to descend into, `..` on top.
    fn browse_dir(&mut self, input: &str) {
        let root = expand_tilde(input);
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(err) => {
                self.error(format!("cannot browse {}: {err}", root.display()));
                return;
            }
        };
        let inside_maildir = root.join("cur").is_dir();
        let mut dirs: Vec<(String, usize)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if inside_maildir && matches!(name.as_str(), "cur" | "new" | "tmp") {
                continue;
            }
            if path.join("cur").is_dir() {
                // Maildir++-style dot folders are maildirs, keep them.
                dirs.push((path.display().to_string(), maildir::new_count(&path)));
            } else if !name.starts_with('.') {
                dirs.push((format!("{}/", path.display()), 0));
            }
        }
        dirs.sort();
        dirs.insert(0, ("..".into(), 0));
        self.mode = Mode::Folders {
            dirs,
            sel: 0,
            root: Some(root),
        };
    }

    /// Create a maildir (cur/new/tmp) at `input`, relative to the
    /// browsed directory when the path isn't absolute.
    fn create_maildir(&mut self, input: &str) {
        if input.is_empty() {
            return;
        }
        let root = match &self.mode {
            Mode::Folders { root, .. } => root.clone(),
            _ => None,
        };
        let given = expand_tilde(input);
        let path = if given.is_absolute() {
            given
        } else {
            root.clone()
                .or_else(|| self.session.dir.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| self.session.dir.clone())
                .join(given)
        };
        for sub in ["cur", "new", "tmp"] {
            if let Err(err) = std::fs::create_dir_all(path.join(sub)) {
                self.error(format!("cannot create {}: {err}", path.display()));
                return;
            }
        }
        self.note(format!("created maildir {}", path.display()));
        if let Some(root) = root {
            // Re-list so the new maildir shows up.
            self.browse_dir(&root.display().to_string());
        }
    }

    /// `Q` submitted: run query_command and list what it found.
    fn run_query(&mut self, input: &str) {
        if input.is_empty() {
            return;
        }
        let Some(command) = self.session.config.mail.query_command.clone() else {
            return;
        };
        let results = alias::query(&command, input);
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
                if let Some(Prompt::Line { buf, cursor, .. }) = &mut self.prompt {
                    *buf = addr;
                    *cursor = buf.chars().count();
                }
            }
            _ => {}
        }
    }

    fn open_folder_browser(&mut self) {
        if !self.session.ready_to_leave() {
            return;
        }
        let dirs = match self.session.folder_candidates() {
            Ok(dirs) => dirs,
            Err(err) => {
                self.error(format!("cannot list folders: {err:#}"));
                return;
            }
        };
        if dirs.is_empty() {
            self.error("no maildirs found next to this one");
            return;
        }
        let sel = dirs
            .iter()
            .position(|d| d.0 == self.session.title || expand_tilde(&d.0) == self.session.dir)
            .unwrap_or(0);
        self.mode = Mode::Folders {
            dirs,
            sel,
            root: None,
        };
    }

    /// mutt's Esc c: open it, then refuse to write to it. `-R` is
    /// the session-wide version and outlives the switch either way.
    /// mutt's virtual mailboxes, near enough: a notmuch query
    /// mirrored into a maildir of symlinks, opened read-only.
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

    fn open_mailbox_read_only(&mut self, spec: &str) {
        self.open_mailbox_spec(spec);
        self.session.read_only = true;
        self.note(format!("{} (read-only)", self.session.title));
    }

    fn open_mailbox_spec(&mut self, spec: &str) {
        self.session.mark_old_unread();
        // Message-hook settings belong to the message being left, not
        // to the config the new mailbox inherits.
        self.session.clear_message_hooks();
        match self.session.switch_to(spec, Box::new(progress)) {
            Ok(warnings) => {
                // Whatever menu asked for the switch is done with: the
                // new mailbox opens on its index, at the top.
                self.mode = Mode::Index;
                self.index_offset = 0;
                self.tag_next = false;
                self.complete = None;
                if !warnings.is_empty() {
                    self.note(warnings.join("; "));
                }
                self.refresh_sidebar();
                self.session.run_folder_hooks();
            }
            Err(err) => self.error(format!("cannot open {spec}: {err:#}")),
        }
    }

    // ---- compose ----

    fn edit_draft(&mut self, terminal: &mut DefaultTerminal, compose: Compose) {
        let editor = self.session.config.mail.editor.clone().unwrap_or_else(|| {
            std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into())
        });
        ratatui::restore();
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("rmut-editor")
            .arg(&compose.path)
            .status();
        *terminal = ratatui::init();
        let _ = terminal.clear();
        match status {
            Ok(s) if s.success() => {
                self.session.set_draft(compose);
                self.open_compose_menu();
            }
            _ => {
                self.error(format!(
                    "editor failed; draft kept at {}",
                    compose.path.display()
                ));
            }
        }
    }

    /// Mutt's compose menu: entered after the editor, and again after
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
        let get = |n: &str| rmut_session::header_value(&head, n).unwrap_or_default();
        let from = match rmut_session::header_value(&head, "From") {
            Some(f) => f,
            None => self
                .session
                .current_identity(&[])
                .from_line()
                .unwrap_or_else(|| default_from(&maildir::hostname())),
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
            crate::ui::humanize_size(body_size)
        ));
        if let Some(orig) = &c.attach {
            let size = std::fs::metadata(orig).map(|m| m.len()).unwrap_or(0);
            out.push(format!(
                "{:<28} {:>8}  message/rfc822  forwarded original",
                orig.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                crate::ui::humanize_size(size)
            ));
        }
        let text = draft_full(c).unwrap_or_default();
        for a in compose::extract_attachments(&text).1 {
            let size = match std::fs::metadata(&a.path) {
                Ok(m) => crate::ui::humanize_size(m.len()),
                Err(_) => "missing!".into(),
            };
            out.push(format!(
                "{:<28} {:>8}  {}{}",
                a.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                size,
                a.mime
                    .as_deref()
                    .unwrap_or_else(|| compose::content_type(&a.path)),
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
                    self.pending_editor = Some(c);
                }
            }
            // Before plain t: ctrl+t edits the selected attachment's
            // content-type (mutt's edit-type).
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let sel = *sel;
                let ask = self.session.ask_attach_field(sel, true);
                self.open_ask(ask);
            }
            KeyCode::Char('t') => self.ask_header("To"),
            KeyCode::Char('c') => self.ask_header("Cc"),
            KeyCode::Char('b') => self.ask_header("Bcc"),
            KeyCode::Char('s') => self.ask_header("Subject"),
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
            KeyCode::Enter => self.view_compose_entry(),
            KeyCode::Char('D') => {
                let sel = *sel;
                self.session.detach(sel);
                // A row may have gone: keep the cursor on the table.
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
                // Leaving the menu; an answer that keeps the draft
                // asks for it back.
                self.mode = Mode::Index;
                let ask = self.session.ask_postpone();
                self.open_ask(ask);
            }
            _ => {}
        }
    }

    /// Enter in the compose menu: show the selected entry: the
    /// draft body, the forwarded original, or an attached file (text
    /// directly, other types through their [filters] command).
    fn view_compose_entry(&mut self) {
        let Mode::Compose { sel } = self.mode else {
            return;
        };
        let Some(c) = self.session.draft() else {
            return;
        };
        let has_orig = c.attach.is_some();
        let (title, text) = if sel == 0 {
            // The body is the file as edited, minus a header block
            // when edit_headers keeps one in the file.
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
                message::body_text(&path).unwrap_or_default(),
            )
        } else {
            let k = sel - 1 - usize::from(has_orig);
            let full = draft_full(c).unwrap_or_default();
            let Some(a) = compose::extract_attachments(&full).1.into_iter().nth(k) else {
                return;
            };
            let mimetype = a
                .mime
                .clone()
                .unwrap_or_else(|| compose::content_type(&a.path).to_string());
            let mimetype = mimetype.as_str();
            let name = a.path.display().to_string();
            match self.session.display.filters.get(mimetype).cloned() {
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

    /// Take the newest held message back to its compose menu, draft
    /// and all. Returns false when nothing was waiting.
    /// mutt has no undo; rmut's walks back the last step. A message
    /// still inside its $undo_send window is the most recent thing
    /// done, so it is what undo takes back first.
    fn undo_last(&mut self) {
        // A message still inside its $undo_send window is the most
        // recent thing done, so it is what undo takes back first.
        if self.session.cancel_send() {
            self.run_requests_quietly();
            return;
        }
        self.session.undo_last();
    }

    /// Recall a postponed draft: straight into the editor when there
    /// is one, a picker when there are several.
    fn recall_postponed(&mut self) {
        let files = self
            .session
            .postponed_dir()
            .and_then(|dir| maildir::scan(&dir).ok())
            .unwrap_or_default();
        let mut files = files;
        if files.is_empty() {
            self.error("no postponed messages");
            return;
        }
        if files.len() == 1 {
            let file = files.remove(0);
            self.pending_editor = self.session.recall_file(file.path);
            return;
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.path.metadata().and_then(|m| m.modified()).ok()));
        let drafts = files
            .into_iter()
            .map(|f| {
                let label = match message::envelope(f.clone()) {
                    Ok(env) => format!("{}  {}", message::format_index_date(env.date), env.subject),
                    Err(_) => f.path.display().to_string(),
                };
                (f.path, label)
            })
            .collect();
        self.mode = Mode::Postponed { drafts, sel: 0 };
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
                    self.pending_editor = self.session.recall_file(path);
                }
            }
            _ => {}
        }
    }

    // ---- shared helpers ----

    /// Queue a macro's keys in front of whatever is already queued (a
    /// macro fired mid-replay expands in place, like mutt's input
    /// stack). The cap breaks self-referencing macros.
    /// mutt's enter-command: `:` takes one config line for this
    /// session (the config file itself is never rewritten).
    fn open_command_prompt(&mut self) {
        self.prompt = Some(Prompt::line(":", String::new(), LineKind::EnterCommand));
    }

    /// Run one `:` line: apply every command it holds, then recompile
    /// whatever the change touched so it shows without a restart. The
    /// first failing command reports and stops the line, like mutt.
    fn run_command_line(&mut self, line: &str) {
        // The sidebar's runtime toggle is the front end's, so only a
        // config change moves it, not a recompile.
        let sidebar_before = self.session.config.sidebar.visible;
        let run = self.session.run_command_line(line);
        let mut reports = run.reports;
        reports.extend(run.warnings);
        reports.extend(self.run_requests());
        if self.session.config.sidebar.visible != sidebar_before {
            self.sidebar_visible = self.session.config.sidebar.visible;
        }
        if !reports.is_empty() {
            self.note(reports.join("; "));
        }
    }

    // ---- hooks (folder-hook, message-hook, reply-hook, fcc-hook) ----

    /// Rebuild what the front end derives from the config: the
    /// theme, the key tables and the colour rules. The session
    /// recompiles its own half and says so with a request.
    fn recompile_ui(&mut self) -> Vec<String> {
        let (derived, warnings) = Derived::from_config(&self.session.config);
        self.theme = derived.theme;
        self.keymap = derived.keymap;
        self.index_rules = derived.index_rules;
        self.body_rules = derived.body_rules;
        warnings
    }

    /// `bind` / `macro`: the action names live here, not in the config
    /// crate, so these are checked against the live tables and then
    /// written into the config for `recompile` to pick up. mutt key
    /// spellings (`\Cd`, `<esc>`) and mutt function names both work.
    fn bind_command(&mut self, cmd: &command::Command) -> Result<Option<String>, String> {
        let (menu, key) = match cmd {
            command::Command::Bind { menu, key, .. }
            | command::Command::Macro { menu, key, .. } => (*menu, key),
            _ => return Ok(None),
        };
        let key = rmut_core::muttrc::convert_key(key).unwrap_or_else(|| key.clone());
        if parse_key(&key).is_none() {
            return Err(format!("no such key {key:?}"));
        }
        let menus: &[command::Menu] = match menu {
            command::Menu::Generic => &[command::Menu::Index, command::Menu::Pager],
            command::Menu::Index => &[command::Menu::Index],
            command::Menu::Pager => &[command::Menu::Pager],
        };
        match cmd {
            command::Command::Bind { function, .. } => {
                let mut bound = false;
                for m in menus {
                    let Some(action) = resolve_function(*m, function) else {
                        continue;
                    };
                    let table = if *m == command::Menu::Index {
                        &mut self.session.config.keys.index
                    } else {
                        &mut self.session.config.keys.pager
                    };
                    table.insert(action, key.clone());
                    bound = true;
                }
                if !bound {
                    return Err(format!("no such function {function:?}"));
                }
            }
            command::Command::Macro { seq, .. } => {
                if parse_sequence(seq).is_none() {
                    return Err(format!("bad key sequence {seq:?}"));
                }
                for m in menus {
                    let table = if *m == command::Menu::Index {
                        &mut self.session.config.macros.index
                    } else {
                        &mut self.session.config.macros.pager
                    };
                    table.insert(key.clone(), seq.clone());
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// `push SEQUENCE`: keys into the input queue, the same path a
    /// macro takes.
    fn push_command(&mut self, seq: &str) -> Result<Option<String>, String> {
        let keys = parse_sequence(seq).ok_or_else(|| format!("bad key sequence {seq:?}"))?;
        self.replay(keys);
        Ok(None)
    }

    /// `exec FUNCTION`: run one action straight away, in whichever
    /// menu is on screen.
    fn exec_command(&mut self, function: &str) -> Result<Option<String>, String> {
        let (width, page) = self.view_size;
        match self.mode {
            Mode::Pager(_) => {
                let name = resolve_function(command::Menu::Pager, function)
                    .ok_or_else(|| format!("no such pager function {function:?}"))?;
                let action = PagerAction::from_name(&name)
                    .ok_or_else(|| format!("no such pager function {function:?}"))?;
                self.run_pager_action(action, self.pager_wrap(width), page);
            }
            _ => {
                let name = resolve_function(command::Menu::Index, function)
                    .ok_or_else(|| format!("no such index function {function:?}"))?;
                let action = IndexAction::from_name(&name)
                    .ok_or_else(|| format!("no such index function {function:?}"))?;
                self.run_index_action(action, false, page);
            }
        }
        Ok(None)
    }

    /// `rmut mailto:...`: a new draft with To/Cc/Bcc/Subject/body
    /// already filled in, straight into the editor, skipping the
    /// prompts the `m` key would ask.
    pub fn start_mailto(&mut self, m: &rmut_core::mailto::Mailto) {
        let from = self.session.compose_from(None, &m.to);
        let text = compose::draft_text(
            &compose::DraftHeaders {
                from,
                to: m.to.clone(),
                cc: m.cc.clone(),
                subject: m.subject.clone(),
                in_reply_to: None,
                references: None,
            },
            &m.body,
        );
        // DraftHeaders has no Bcc slot; it goes ahead of the blank
        // line, where the send path strips it off the wire copy.
        let text = match m.bcc.as_deref().filter(|b| !b.trim().is_empty()) {
            Some(bcc) => match text.split_once("\n\n") {
                Some((head, body)) => format!("{head}\nBcc: {bcc}\n\n{body}"),
                None => text,
            },
            None => text,
        };
        match self.session.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
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

    /// `rmut -p`: straight into the postponed picker.
    pub fn open_postponed(&mut self) {
        self.recall_postponed();
    }

    /// `rmut -y`: straight into the mailbox list.
    pub fn open_folders(&mut self) {
        self.open_folder_browser();
    }

    /// `rmut -e COMMAND`: one `:` line applied before the first draw.
    pub fn run_startup_command(&mut self, line: &str) {
        self.run_command_line(line);
    }

    fn replay(&mut self, seq: Vec<KeyEvent>) {
        if self.pending_keys.len() + seq.len() > 1000 {
            self.pending_keys.clear();
            self.error("macro expansion too deep, stopped");
            return;
        }
        for (i, key) in seq.into_iter().enumerate() {
            self.pending_keys.insert(i, key);
        }
    }

    /// mutt's edit function (`e`): the selected message's raw bytes go
    /// through $EDITOR, and a changed result replaces the original:
    /// in place for maildirs, append + delete-mark on IMAP.
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
        self.pending_raw_edit = self.session.selected_path();
    }

    /// mutt's `!`: stand the TUI down, run the command on the real
    /// terminal, and wait before painting over whatever it printed.
    fn run_shell(&mut self, terminal: &mut DefaultTerminal, command: &str) {
        ratatui::restore();
        let status = Command::new("sh").arg("-c").arg(command).status();
        {
            use std::io::BufRead as _;
            let mut out = std::io::stdout();
            let _ = write!(out, "\nPress Enter to continue... ");
            let _ = out.flush();
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
        }
        *terminal = ratatui::init();
        let _ = terminal.clear();
        match status {
            Ok(s) if s.success() => self.note(format!("{command} finished")),
            Ok(s) => self.error(format!("{command} exited with {s}")),
            Err(err) => self.error(format!("cannot run {command}: {err}")),
        }
    }

    /// mutt's Ctrl+Z: hand the terminal back and stop, the way any
    /// job does. The default SIGTSTP action does the stopping, so the
    /// shell's fg resumes us right here, and the screen is repainted.
    fn suspend(&mut self, terminal: &mut DefaultTerminal) {
        ratatui::restore();
        // SAFETY: raise(3) on the calling process, no handler of ours.
        unsafe { libc::raise(libc::SIGTSTP) };
        *terminal = ratatui::init();
        let _ = terminal.clear();
        self.redraw = true;
    }

    fn edit_raw(&mut self, terminal: &mut DefaultTerminal, path: PathBuf) {
        let original = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.error(format!("cannot read message: {err}"));
                return;
            }
        };
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
        let editor = self.session.config.mail.editor.clone().unwrap_or_else(|| {
            std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".into())
        });
        ratatui::restore();
        let status = Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("rmut-editor")
            .arg(&temp)
            .status();
        *terminal = ratatui::init();
        let _ = terminal.clear();
        let edited = std::fs::read(&temp).unwrap_or_default();
        let _ = std::fs::remove_file(&temp);
        if !matches!(status, Ok(s) if s.success()) {
            self.error("editor failed, message unchanged");
            return;
        }
        if edited == original {
            self.note("message unchanged");
            return;
        }
        self.session.store_edited(&path, &edited);
        self.refresh_sidebar();
    }

    /// Open a copy of the message as a new draft (mutt's resend): its
    /// To/Cc/Subject and body prefill the editor, then the normal send
    /// prompt takes over.
    fn resend_current(&mut self) {
        // Completes a partial IMAP file so the body is really there.
        if self.session.full_message_bytes().is_none() {
            return;
        }
        let Some(base) = self.session.compose_base() else {
            self.error("no message selected");
            return;
        };
        let body = message::body_text(&base.path).unwrap_or_default();
        let text = compose::draft_text(
            &compose::DraftHeaders {
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
                self.pending_editor = Some(Compose {
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

    fn open_selected(&mut self) {
        self.session.mark_read();
        // Opening a message ends the pager search, like mutt (whose
        // compiled search is per pager session); the text stays as
        // the next prompt's prefill.
        self.pager_search = None;
        let Some(&i) = self.session.visible.get(self.session.sel) else {
            return;
        };
        let path = self.session.msgs[i].env.file.path.clone();
        match self.session.load_view(&path) {
            Ok(view) => {
                self.mode = Mode::Pager(Pager {
                    view,
                    scroll: 0,
                    full_headers: false,
                    hide_quoted: false,
                    back: None,
                });
            }
            Err(err) => self.error(format!("cannot open message: {err:#}")),
        }
    }
}

/// Which functions `;` (tag-prefix) can hand the tagged set to. The
/// rest say so rather than quietly acting on one message: resend and
/// edit open a draft or an editor, of which rmut has one at a time.
fn takes_tagged(action: IndexAction) -> bool {
    use IndexAction::*;
    matches!(
        action,
        Delete | Undelete | Flag | ToggleNew | Tag | Save | Copy | Pipe | Print | Bounce
    )
}

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Set once the ratatui alternate screen is up: progress switches
/// from stderr lines (the initial open runs before ratatui::init) to
/// direct writes on the terminal's bottom row.
pub static TUI_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// A progress line clobbered the bottom row behind ratatui's back;
/// the next draw must repaint everything.
static PROGRESS_DIRTY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Transient "what am I doing" line for blocking IMAP work
/// (connecting, fetching flags/headers), so the UI never looks stuck.
pub fn progress(msg: &str) {
    use std::io::Write;
    use std::sync::atomic::Ordering;
    if TUI_ACTIVE.load(Ordering::Relaxed) {
        use ratatui::crossterm::{cursor, queue, style, terminal};
        let mut out = std::io::stdout();
        if let Ok((_, rows)) = terminal::size()
            && queue!(
                out,
                cursor::MoveTo(0, rows.saturating_sub(1)),
                terminal::Clear(terminal::ClearType::CurrentLine),
                style::Print(msg)
            )
            .is_ok()
        {
            let _ = out.flush();
            PROGRESS_DIRTY.store(true, Ordering::Relaxed);
        }
    } else {
        eprint!("\r\x1b[K{msg}");
    }
}

/// One [filters] command over a file on disk (compose menu view):
/// the file is its stdin, its stdout is the rendered text.
fn run_file_filter(command: &str, path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let out = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::from(file))
        .output()
        .with_context(|| format!("running {command}"))?;
    anyhow::ensure!(out.status.success(), "{command} exited with {}", out.status);
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The first line matching `m` strictly after (before, when searching
/// backwards) `from`, wrapping around; the flag reports the wrap. The
/// starting line itself is only reached by going all the way around.
fn search_lines(
    lines: &[String],
    m: &pattern::Matcher,
    from: usize,
    forward: bool,
) -> Option<(usize, bool)> {
    wrap_order(lines.len(), from, forward)
        .into_iter()
        .find(|&(idx, _)| m.is_match(&lines[idx]))
}

#[cfg(test)]
mod tests {
    #[test]
    fn search_lines_steps_and_wraps() {
        use super::search_lines;
        use rmut_core::pattern::Matcher;
        let lines: Vec<String> = ["alpha", "the needle", "beta", "a Needle too"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = Matcher::new("needle");
        // Forward from the top: the next hit, no wrap; case-insensitive.
        assert_eq!(search_lines(&lines, &m, 0, true), Some((1, false)));
        assert_eq!(search_lines(&lines, &m, 1, true), Some((3, false)));
        // Past the last hit it wraps to the first.
        assert_eq!(search_lines(&lines, &m, 3, true), Some((1, true)));
        // Backwards, with and without the wrap.
        assert_eq!(search_lines(&lines, &m, 3, false), Some((1, false)));
        assert_eq!(search_lines(&lines, &m, 1, false), Some((3, true)));
        // No match, and the empty pager.
        assert_eq!(search_lines(&lines, &Matcher::new("zzz"), 0, true), None);
        assert_eq!(search_lines(&[], &m, 0, true), None);
        // A regex argument works like the patterns do.
        let re = Matcher::new("^bet.");
        assert_eq!(search_lines(&lines, &re, 0, true), Some((2, false)));
    }
}
