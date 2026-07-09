use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rmut_core::config::{Account, Config};
use rmut_core::message::Envelope;
use rmut_core::pattern::{self, Pattern};
use rmut_core::remote::{self, Remote};
use rmut_core::{alias, compose, maildir, message, pgp, smtp, thread};

use crate::keymap::{IndexAction, Keymap, PagerAction};
use crate::theme::Theme;

pub struct Msg {
    pub env: message::Envelope,
    /// Flags changed since the last sync (rename pending).
    pub dirty: bool,
}

impl Msg {
    pub fn pending(&self) -> bool {
        self.dirty || self.env.file.flags.deleted
    }
}

pub struct Pager {
    pub view: message::MessageView,
    pub scroll: usize,
    pub full_headers: bool,
}

pub enum Mode {
    Index,
    Pager(Pager),
    Attach {
        msg_path: PathBuf,
        parts: Vec<message::Part>,
        sel: usize,
        /// Pager to return to when the menu was opened from there.
        back: Option<Pager>,
    },
    Folders {
        /// Paths or `imap:` specs, ready for `open_mailbox_spec`.
        dirs: Vec<String>,
        sel: usize,
    },
    Help {
        lines: Vec<String>,
        scroll: usize,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Date,
    From,
    Subject,
    Size,
    Threads,
}

impl SortKey {
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Date => "date",
            SortKey::From => "from",
            SortKey::Subject => "subject",
            SortKey::Size => "size",
            SortKey::Threads => "threads",
        }
    }
}

#[derive(Clone, Copy)]
pub enum LineKind {
    Limit,
    Search,
    ChangeDir,
    SavePart,
    ComposeTo,
    ComposeSubject,
}

#[derive(Clone, Copy)]
pub enum KeyKind {
    Sort,
    Quit,
    Send,
    Security,
    Recall,
    Print,
}

pub enum Prompt {
    Line {
        label: String,
        buf: String,
        kind: LineKind,
    },
    Key {
        label: String,
        kind: KeyKind,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    GroupReply,
    Forward,
}

/// Snapshot of the message being replied to / forwarded, taken when the
/// compose flow starts.
pub struct ComposeBase {
    path: PathBuf,
    reply_to: String,
    orig_to: String,
    orig_cc: String,
    from_display: String,
    subject: String,
    date: i64,
    msg_id: Option<String>,
    references: Vec<String>,
}

pub struct ComposeSetup {
    kind: ComposeKind,
    base: Option<ComposeBase>,
    to: Option<String>,
}

/// PGP treatment for an outgoing draft, chosen at the send prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Security {
    None,
    Sign,
    Encrypt,
    Both,
}

impl Security {
    fn label(self) -> &'static str {
        match self {
            Security::None => "",
            Security::Sign => "sign",
            Security::Encrypt => "encrypt",
            Security::Both => "sign+encrypt",
        }
    }
}

/// A draft file going through editor → send/postpone/discard.
pub struct Compose {
    pub path: PathBuf,
    /// Postponed original to delete once the message is sent.
    pub recall_source: Option<PathBuf>,
    pub security: Security,
}

const SEND_PROMPT: &str = "Send message? (y)es (e)dit (s)ecurity (p)ostpone (q)discard";

pub struct App {
    pub dir: PathBuf,
    /// What the status line calls this mailbox: the path for local
    /// maildirs, the `imap:account/folder` spec for remote ones.
    pub title: String,
    /// Set when `dir` is the cache maildir of an IMAP folder.
    remote: Option<Remote>,
    pub msgs: Vec<Msg>,
    /// Indices into `msgs` after applying limit and thread folding.
    pub visible: Vec<usize>,
    /// Selection, as an index into `visible`.
    pub sel: usize,
    pub index_offset: usize,
    pub mode: Mode,
    pub prompt: Option<Prompt>,
    pub status: Option<String>,
    pub sort: SortKey,
    pub sort_rev: bool,
    pub limit: Option<(String, Vec<Pattern>)>,
    pub last_search: Option<Vec<Pattern>>,
    /// Per-message thread depth/root (aligned with `msgs`; identity when
    /// not sorted by threads).
    pub thread_depth: Vec<usize>,
    pub thread_root: Vec<usize>,
    /// Paths of collapsed thread roots.
    pub collapsed: HashSet<PathBuf>,
    pub config: Config,
    pub theme: Theme,
    pub keymap: Keymap,
    compose_setup: Option<ComposeSetup>,
    compose: Option<Compose>,
    pending_editor: Option<Compose>,
    /// mtimes of new/ and cur/ used for new-mail detection.
    dir_mtimes: (Option<SystemTime>, Option<SystemTime>),
    quit: bool,
}

fn dir_mtimes(dir: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let mtime = |p: PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    (mtime(dir.join("new")), mtime(dir.join("cur")))
}

impl App {
    pub fn open(dir: &Path, config: Config) -> Result<Self> {
        let mut msgs = Vec::new();
        let mut skipped = 0usize;
        for file in maildir::scan(dir)? {
            match message::envelope(file) {
                Ok(env) => msgs.push(Msg { env, dirty: false }),
                Err(_) => skipped += 1,
            }
        }
        // Mutt's default sort: date, oldest first.
        msgs.sort_by_key(|m| m.env.date);
        let visible: Vec<usize> = (0..msgs.len()).collect();
        let sel = visible.len().saturating_sub(1);
        let (theme, mut warnings) = Theme::from_config(&config);
        let (keymap, key_warnings) = Keymap::with_config(&config.keys.index, &config.keys.pager);
        warnings.extend(key_warnings);
        if skipped > 0 {
            warnings.push(format!("{skipped} unreadable message(s) skipped"));
        }
        let status = (!warnings.is_empty()).then(|| warnings.join("; "));
        let count = msgs.len();
        Ok(App {
            dir: dir.to_path_buf(),
            title: dir.display().to_string(),
            remote: None,
            msgs,
            visible,
            sel,
            index_offset: 0,
            mode: Mode::Index,
            prompt: None,
            status,
            sort: SortKey::Date,
            sort_rev: false,
            limit: None,
            last_search: None,
            thread_depth: vec![0; count],
            thread_root: (0..count).collect(),
            collapsed: HashSet::new(),
            dir_mtimes: dir_mtimes(dir),
            config,
            theme,
            keymap,
            compose_setup: None,
            compose: None,
            pending_editor: None,
            quit: false,
        })
    }

    /// Open a mailbox by spec: an `imap:account[/folder]` string (the
    /// folder is mirrored into a cache maildir) or a local path.
    pub fn open_spec(spec: &str, config: Config) -> Result<Self> {
        match remote::parse_spec(spec) {
            Some((account_name, mailbox)) => {
                let account = config
                    .account(account_name)
                    .with_context(|| format!("no account {account_name} in config"))?
                    .clone();
                let password = account_password(&account)?;
                let remote = Remote::open(&account, mailbox, &password)?;
                let cache = remote.cache.clone();
                let mut app = App::open(&cache, config)?;
                app.title = remote.spec.clone();
                app.remote = Some(remote);
                Ok(app)
            }
            None => App::open(&expand_tilde(spec), config),
        }
    }

    pub fn new_count(&self) -> usize {
        self.msgs.iter().filter(|m| m.env.file.is_new).count()
    }

    pub fn deleted_count(&self) -> usize {
        self.msgs
            .iter()
            .filter(|m| m.env.file.flags.deleted)
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.msgs.iter().filter(|m| m.pending()).count()
    }

    /// (depth, hidden-count-if-collapsed-root) for the index display.
    pub fn thread_info(&self, mi: usize) -> (usize, Option<usize>) {
        if self.sort != SortKey::Threads {
            return (0, None);
        }
        let depth = self.thread_depth.get(mi).copied().unwrap_or(0);
        if depth == 0 && self.collapsed.contains(&self.msgs[mi].env.file.path) {
            let hidden = self
                .thread_root
                .iter()
                .enumerate()
                .filter(|&(j, &r)| r == mi && j != mi)
                .count();
            if hidden > 0 {
                return (0, Some(hidden));
            }
        }
        (depth, None)
    }

    pub fn run(&mut self, mut terminal: DefaultTerminal) -> Result<()> {
        let poll_every = Duration::from_secs(self.config.mail.poll_seconds.unwrap_or(5).max(1));
        let mut last_poll = Instant::now();
        while !self.quit {
            terminal.draw(|frame| crate::ui::draw(frame, self))?;
            if event::poll(Duration::from_millis(1000))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                let size = terminal.size()?;
                self.status = None;
                self.handle_key(key, size.width as usize, size.height as usize);
            }
            if last_poll.elapsed() >= poll_every {
                last_poll = Instant::now();
                self.check_new_mail();
            }
            if let Some(compose) = self.pending_editor.take() {
                self.edit_draft(&mut terminal, compose);
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, width: usize, height: usize) {
        // Rows available for content: total minus help line and status line.
        let page = height.saturating_sub(2).max(1);
        if self.prompt.is_some() {
            self.handle_prompt_key(key);
        } else if matches!(self.mode, Mode::Index) {
            self.handle_index_key(key, page);
        } else if matches!(self.mode, Mode::Pager(_)) {
            self.handle_pager_key(key, width, page);
        } else if matches!(self.mode, Mode::Attach { .. }) {
            self.handle_attach_key(key);
        } else if matches!(self.mode, Mode::Help { .. }) {
            self.handle_help_key(key, page);
        } else {
            self.handle_folders_key(key);
        }
    }

    // ---- new-mail detection ----

    fn check_new_mail(&mut self) {
        if let Some(remote) = &mut self.remote {
            // NOOP + cache refresh; arrivals land in the cache maildir
            // and are picked up by the mtime rescan below.
            if let Err(err) = remote.check_new() {
                self.status = Some(format!("imap: {err:#}"));
            }
        }
        let current = dir_mtimes(&self.dir);
        if current == self.dir_mtimes {
            return;
        }
        self.rescan();
    }

    /// Re-read the maildir, keeping unsynced flag changes and deletion
    /// marks for messages that are still there.
    fn rescan(&mut self) {
        let Ok(files) = maildir::scan(&self.dir) else {
            return;
        };
        let keep = self.selected_path();
        let mut old: std::collections::HashMap<PathBuf, Msg> = self
            .msgs
            .drain(..)
            .map(|m| (m.env.file.path.clone(), m))
            .collect();
        let mut arrived = 0usize;
        for file in files {
            match old.remove(&file.path) {
                Some(prev) if prev.pending() => self.msgs.push(prev),
                Some(mut prev) => {
                    // Take fresh on-disk flags, keep the parsed envelope.
                    prev.env.file = file;
                    self.msgs.push(prev);
                }
                None => {
                    if let Ok(env) = message::envelope(file) {
                        if env.file.is_new {
                            arrived += 1;
                        }
                        self.msgs.push(Msg { env, dirty: false });
                    }
                }
            }
        }
        self.dir_mtimes = dir_mtimes(&self.dir);
        self.resort(keep);
        if arrived > 0 {
            self.status = Some(format!("new mail in {} (+{arrived})", self.title));
        }
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
                let kind = *kind;
                self.prompt = None;
                self.run_key_prompt(kind, key.code);
            }
            Some(Prompt::Line { buf, .. }) => match key.code {
                KeyCode::Esc => {
                    self.prompt = None;
                    self.compose_setup = None;
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char('u') if is_ctrl(&key) => buf.clear(),
                KeyCode::Char(c) if !is_ctrl(&key) => buf.push(c),
                KeyCode::Enter => {
                    if let Some(Prompt::Line { buf, kind, .. }) = self.prompt.take() {
                        self.run_line_prompt(kind, buf.trim());
                    }
                }
                _ => {}
            },
            None => {}
        }
    }

    fn run_key_prompt(&mut self, kind: KeyKind, code: KeyCode) {
        match kind {
            KeyKind::Sort => {
                let (sort, rev) = match code {
                    KeyCode::Char('d') => (SortKey::Date, false),
                    KeyCode::Char('D') => (SortKey::Date, true),
                    KeyCode::Char('f') => (SortKey::From, false),
                    KeyCode::Char('F') => (SortKey::From, true),
                    KeyCode::Char('s') => (SortKey::Subject, false),
                    KeyCode::Char('S') => (SortKey::Subject, true),
                    KeyCode::Char('z') => (SortKey::Size, false),
                    KeyCode::Char('Z') => (SortKey::Size, true),
                    KeyCode::Char('t') | KeyCode::Char('T') => (SortKey::Threads, false),
                    _ => return,
                };
                self.sort = sort;
                self.sort_rev = rev;
                self.apply_sort();
                self.status = Some(format!(
                    "sorted by {}{}",
                    sort.name(),
                    if rev { " (reverse)" } else { "" }
                ));
            }
            KeyKind::Quit => match code {
                KeyCode::Char('y') => {
                    self.sync();
                    self.quit = true;
                }
                KeyCode::Char('n') => self.quit = true,
                _ => {}
            },
            KeyKind::Send => match code {
                KeyCode::Char('y') => self.send_draft(),
                KeyCode::Char('e') => {
                    if let Some(c) = self.compose.take() {
                        self.pending_editor = Some(c);
                    }
                }
                KeyCode::Char('p') => self.postpone_draft(),
                KeyCode::Char('s') => {
                    self.prompt = Some(Prompt::Key {
                        label: "Security: (e)ncrypt (s)ign (b)oth (c)lear: ".into(),
                        kind: KeyKind::Security,
                    });
                }
                KeyCode::Char('q') => {
                    if let Some(c) = self.compose.take() {
                        let _ = std::fs::remove_file(&c.path);
                        self.status = Some("message discarded".into());
                    }
                }
                _ => self.reprompt_send(),
            },
            KeyKind::Security => {
                if let Some(c) = &mut self.compose {
                    c.security = match code {
                        KeyCode::Char('e') => Security::Encrypt,
                        KeyCode::Char('s') => Security::Sign,
                        KeyCode::Char('b') => Security::Both,
                        KeyCode::Char('c') => Security::None,
                        _ => c.security,
                    };
                }
                self.reprompt_send();
            }
            KeyKind::Recall => match code {
                KeyCode::Char('r') => self.recall_postponed(),
                KeyCode::Char('n') => self.continue_setup(ComposeKind::New, None),
                _ => {}
            },
            KeyKind::Print => {
                if code == KeyCode::Char('y') {
                    self.print_current();
                }
            }
        }
    }

    fn run_line_prompt(&mut self, kind: LineKind, input: &str) {
        match kind {
            LineKind::Limit => {
                let keep = self.selected_path();
                if input.is_empty() || input == "all" {
                    self.limit = None;
                } else {
                    self.limit = Some((input.to_string(), pattern::parse(input)));
                }
                self.rebuild_visible(keep);
                if self.visible.is_empty() {
                    self.status = Some("no messages match the limit".into());
                }
            }
            LineKind::Search => {
                if !input.is_empty() {
                    self.last_search = Some(pattern::parse(input));
                }
                self.search_next();
            }
            LineKind::ChangeDir => self.open_mailbox_spec(input),
            LineKind::SavePart => self.save_part(input),
            LineKind::ComposeTo => self.setup_to_submitted(input),
            LineKind::ComposeSubject => self.finish_compose_setup(input),
        }
    }

    // ---- index ----

    fn handle_index_key(&mut self, key: KeyEvent, page: usize) {
        let Some(action) = self.keymap.lookup_index(&key) else {
            return;
        };
        match action {
            IndexAction::Quit => {
                let pending = self.pending_count();
                if pending == 0 {
                    self.quit = true;
                } else {
                    self.prompt = Some(Prompt::Key {
                        label: format!(
                            "{pending} change(s) — save & quit (y) / discard & quit (n) / cancel: "
                        ),
                        kind: KeyKind::Quit,
                    });
                }
            }
            IndexAction::Abort => self.quit = true,
            IndexAction::Down => self.select(self.sel.saturating_add(1)),
            IndexAction::Up => self.select(self.sel.saturating_sub(1)),
            IndexAction::PageDown => self.select(self.sel.saturating_add(page)),
            IndexAction::PageUp => self.select(self.sel.saturating_sub(page)),
            IndexAction::First => self.select(0),
            IndexAction::Last => self.select(usize::MAX),
            IndexAction::View => self.open_selected(),
            IndexAction::FoldThread => self.toggle_collapse(false),
            IndexAction::FoldAll => self.toggle_collapse(true),
            IndexAction::Delete => {
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::Undelete => {
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                }
            }
            IndexAction::Flag => {
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.flagged = !m.env.file.flags.flagged;
                    m.dirty = true;
                }
            }
            IndexAction::ToggleNew => {
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.seen = !m.env.file.flags.seen;
                    m.env.file.is_new = false;
                    m.dirty = true;
                }
            }
            IndexAction::Sync => self.sync(),
            IndexAction::Compose => self.start_compose(ComposeKind::New),
            IndexAction::Reply => self.start_compose(ComposeKind::Reply),
            IndexAction::GroupReply => self.start_compose(ComposeKind::GroupReply),
            IndexAction::Forward => self.start_compose(ComposeKind::Forward),
            IndexAction::Sort => {
                self.prompt = Some(Prompt::Key {
                    label: "Sort: (d)ate (f)rom (s)ubject si(z)e (t)hreads, uppercase reverses: "
                        .into(),
                    kind: KeyKind::Sort,
                });
            }
            IndexAction::Limit => {
                let buf = self
                    .limit
                    .as_ref()
                    .map(|(s, _)| s.clone())
                    .unwrap_or_default();
                self.prompt = Some(Prompt::Line {
                    label: "Limit (~f/~s/~b/~N/~F/~D/~U or text, empty=all): ".into(),
                    buf,
                    kind: LineKind::Limit,
                });
            }
            IndexAction::Search => {
                self.prompt = Some(Prompt::Line {
                    label: "Search: ".into(),
                    buf: String::new(),
                    kind: LineKind::Search,
                });
            }
            IndexAction::SearchNext => self.search_next(),
            IndexAction::Attachments => self.open_attachments(),
            IndexAction::ChangeMailbox => {
                if self.pending_count() > 0 {
                    self.status = Some("pending changes — sync with $ first".into());
                } else {
                    self.prompt = Some(Prompt::Line {
                        label: "Open mailbox: ".into(),
                        buf: String::new(),
                        kind: LineKind::ChangeDir,
                    });
                }
            }
            IndexAction::Folders => self.open_folder_browser(),
            IndexAction::Print => self.confirm_print(),
            IndexAction::Help => self.open_help(),
        }
    }

    // ---- pager ----

    fn handle_pager_key(&mut self, key: KeyEvent, width: usize, page: usize) {
        let Some(action) = self.keymap.lookup_pager(&key) else {
            return;
        };
        match action {
            PagerAction::Back => {
                self.mode = Mode::Index;
                return;
            }
            PagerAction::NextMsg => {
                if self.sel + 1 < self.visible.len() {
                    self.sel += 1;
                    self.open_selected();
                } else {
                    self.status = Some("last message".into());
                }
                return;
            }
            PagerAction::PrevMsg => {
                if self.sel > 0 {
                    self.sel -= 1;
                    self.open_selected();
                } else {
                    self.status = Some("first message".into());
                }
                return;
            }
            PagerAction::Delete => {
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
                if self.sel + 1 < self.visible.len() {
                    self.sel += 1;
                    self.open_selected();
                } else {
                    self.mode = Mode::Index;
                }
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
            PagerAction::Forward => {
                self.start_compose(ComposeKind::Forward);
                return;
            }
            PagerAction::Print => {
                self.confirm_print();
                return;
            }
            PagerAction::Help => {
                self.open_help();
                return;
            }
            _ => {}
        }
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let lines = crate::ui::pager_line_count(&pager.view, width, pager.full_headers);
        let max_scroll = lines.saturating_sub(page);
        match action {
            PagerAction::Headers => {
                pager.full_headers = !pager.full_headers;
                pager.scroll = 0;
            }
            PagerAction::Down => pager.scroll = (pager.scroll + 1).min(max_scroll),
            PagerAction::Up => pager.scroll = pager.scroll.saturating_sub(1),
            PagerAction::PageDown => pager.scroll = (pager.scroll + page).min(max_scroll),
            PagerAction::PageUp => pager.scroll = pager.scroll.saturating_sub(page),
            PagerAction::Top => pager.scroll = 0,
            PagerAction::Bottom => pager.scroll = max_scroll,
            _ => {}
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
            KeyCode::Char('s') => {
                if let Mode::Attach { parts, sel, .. } = &self.mode {
                    let default = parts[*sel]
                        .filename
                        .clone()
                        .unwrap_or_else(|| format!("part-{}.bin", *sel + 1));
                    self.prompt = Some(Prompt::Line {
                        label: "Save to file: ".into(),
                        buf: default,
                        kind: LineKind::SavePart,
                    });
                }
            }
            _ => {}
        }
    }

    fn open_attachments(&mut self) {
        let Some(&i) = self.visible.get(self.sel) else {
            return;
        };
        let msg_path = self.msgs[i].env.file.path.clone();
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
            Ok(_) => self.status = Some("message has no parts".into()),
            Err(err) => self.status = Some(format!("cannot list parts: {err:#}")),
        }
    }

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
        if !is_text {
            self.status = Some(format!("{mimetype} is not text — save it with s"));
            return;
        }
        match message::part_text(&msg_path, index) {
            Ok(body) => {
                let headers = vec![("Content-Type".to_string(), mimetype)];
                self.mode = Mode::Pager(Pager {
                    view: message::MessageView {
                        brief: headers.clone(),
                        all: headers,
                        body,
                    },
                    scroll: 0,
                    full_headers: false,
                });
            }
            Err(err) => self.status = Some(format!("cannot decode part: {err:#}")),
        }
    }

    fn save_part(&mut self, input: &str) {
        let (msg_path, index) = match &self.mode {
            Mode::Attach { msg_path, sel, .. } => (msg_path.clone(), *sel),
            _ => return,
        };
        if input.is_empty() {
            self.status = Some("no filename given".into());
            return;
        }
        let target = expand_tilde(input);
        if target.exists() {
            self.status = Some(format!("{} exists — not overwriting", target.display()));
            return;
        }
        let result = message::part_bytes(&msg_path, index).and_then(|bytes| {
            std::fs::write(&target, &bytes)?;
            Ok(bytes.len())
        });
        match result {
            Ok(n) => self.status = Some(format!("saved {n} bytes to {}", target.display())),
            Err(err) => self.status = Some(format!("save failed: {err:#}")),
        }
    }

    // ---- folders ----

    fn handle_folders_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Folders { dirs, sel } = &mut self.mode {
                    *sel = (*sel + 1).min(dirs.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Folders { sel, .. } = &mut self.mode {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Char('q') | KeyCode::Esc => self.mode = Mode::Index,
            KeyCode::Enter => {
                let spec = match &self.mode {
                    Mode::Folders { dirs, sel } => dirs.get(*sel).cloned(),
                    _ => None,
                };
                if let Some(spec) = spec {
                    self.open_mailbox_spec(&spec);
                }
            }
            _ => {}
        }
    }

    fn open_folder_browser(&mut self) {
        if self.pending_count() > 0 {
            self.status = Some("pending changes — sync with $ first".into());
            return;
        }
        let mut dirs: Vec<String> = self
            .config
            .mail
            .mailboxes
            .iter()
            .filter(|m| m.starts_with("imap:") || expand_tilde(m).join("cur").is_dir())
            .cloned()
            .collect();
        match &mut self.remote {
            Some(remote) => match remote.folders() {
                Ok(folders) => {
                    let account = remote.account.name.clone();
                    dirs.extend(folders.into_iter().map(|f| format!("imap:{account}/{f}")));
                }
                Err(err) => {
                    self.status = Some(format!("cannot list folders: {err:#}"));
                    return;
                }
            },
            None => dirs.extend(
                maildir::discover(&self.dir)
                    .iter()
                    .map(|p| p.display().to_string()),
            ),
        }
        dirs.sort();
        dirs.dedup();
        if dirs.is_empty() {
            self.status = Some("no maildirs found next to this one".into());
            return;
        }
        let sel = dirs
            .iter()
            .position(|d| *d == self.title || expand_tilde(d) == self.dir)
            .unwrap_or(0);
        self.mode = Mode::Folders { dirs, sel };
    }

    fn open_mailbox_spec(&mut self, spec: &str) {
        match App::open_spec(spec, self.config.clone()) {
            Ok(app) => *self = app,
            Err(err) => self.status = Some(format!("cannot open {spec}: {err:#}")),
        }
    }

    // ---- compose ----

    fn compose_base(&self) -> Option<ComposeBase> {
        let &mi = self.visible.get(self.sel)?;
        let env = &self.msgs[mi].env;
        let view = message::load(&env.file.path).ok()?;
        let get = |name: &str| {
            view.all
                .iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let reply_to = {
            let rt = get("Reply-To");
            if rt.trim().is_empty() {
                get("From")
            } else {
                rt
            }
        };
        Some(ComposeBase {
            path: env.file.path.clone(),
            reply_to,
            orig_to: get("To"),
            orig_cc: get("Cc"),
            from_display: env.from.clone(),
            subject: env.subject.clone(),
            date: env.date,
            msg_id: env.msg_id.clone(),
            references: env.references.clone(),
        })
    }

    fn start_compose(&mut self, kind: ComposeKind) {
        let base = if kind == ComposeKind::New {
            None
        } else {
            match self.compose_base() {
                Some(b) => Some(b),
                None => {
                    self.status = Some("no message selected".into());
                    return;
                }
            }
        };
        if kind == ComposeKind::New && self.has_postponed() {
            self.prompt = Some(Prompt::Key {
                label: "(n)ew message or (r)ecall postponed? ".into(),
                kind: KeyKind::Recall,
            });
            return;
        }
        self.continue_setup(kind, base);
    }

    fn continue_setup(&mut self, kind: ComposeKind, base: Option<ComposeBase>) {
        let to_prefill = match kind {
            ComposeKind::Reply | ComposeKind::GroupReply => base
                .as_ref()
                .map(|b| b.reply_to.clone())
                .unwrap_or_default(),
            ComposeKind::New | ComposeKind::Forward => String::new(),
        };
        self.compose_setup = Some(ComposeSetup {
            kind,
            base,
            to: None,
        });
        self.prompt = Some(Prompt::Line {
            label: "To: ".into(),
            buf: to_prefill,
            kind: LineKind::ComposeTo,
        });
    }

    fn setup_to_submitted(&mut self, input: &str) {
        let to = alias::expand(input, &alias::load_default());
        let Some(setup) = &mut self.compose_setup else {
            return;
        };
        setup.to = Some(to);
        let subject_prefill = match (&setup.kind, &setup.base) {
            (ComposeKind::Reply | ComposeKind::GroupReply, Some(b)) => {
                compose::reply_subject(&b.subject)
            }
            (ComposeKind::Forward, Some(b)) => compose::forward_subject(&b.subject),
            _ => String::new(),
        };
        self.prompt = Some(Prompt::Line {
            label: "Subject: ".into(),
            buf: subject_prefill,
            kind: LineKind::ComposeSubject,
        });
    }

    fn finish_compose_setup(&mut self, subject: &str) {
        let Some(setup) = self.compose_setup.take() else {
            return;
        };
        let to = setup.to.unwrap_or_default();
        let mut cc = None;
        let mut in_reply_to = None;
        let mut references = None;
        let mut body = String::new();
        if let Some(b) = &setup.base {
            match setup.kind {
                ComposeKind::Reply | ComposeKind::GroupReply => {
                    let orig = message::body_text(&b.path).unwrap_or_default();
                    body = compose::quote(&compose::attribution(&b.from_display, b.date), &orig);
                    in_reply_to = b.msg_id.clone();
                    let mut refs = b.references.clone();
                    if let Some(id) = &b.msg_id
                        && !refs.contains(id)
                    {
                        refs.push(id.clone());
                    }
                    if !refs.is_empty() {
                        references = Some(refs.join(" "));
                    }
                    if setup.kind == ComposeKind::GroupReply {
                        let joined = [b.orig_to.as_str(), b.orig_cc.as_str()]
                            .iter()
                            .filter(|s| !s.trim().is_empty())
                            .copied()
                            .collect::<Vec<_>>()
                            .join(", ");
                        if !joined.is_empty() {
                            cc = Some(joined);
                        }
                    }
                }
                ComposeKind::Forward => {
                    let orig = message::body_text(&b.path).unwrap_or_default();
                    body = compose::forward_body(&b.from_display, b.date, &b.subject, &orig);
                }
                ComposeKind::New => {}
            }
        }
        let text = compose::draft_text(
            &compose::DraftHeaders {
                to,
                cc,
                subject: subject.to_string(),
                in_reply_to,
                references,
            },
            &body,
        );
        match write_draft(&text) {
            Ok(path) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: None,
                    security: self.default_security(),
                });
            }
            Err(err) => self.status = Some(format!("cannot write draft: {err:#}")),
        }
    }

    fn edit_draft(&mut self, terminal: &mut DefaultTerminal, compose: Compose) {
        let editor = self.config.mail.editor.clone().unwrap_or_else(|| {
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
                self.compose = Some(compose);
                self.reprompt_send();
            }
            _ => {
                self.status = Some(format!(
                    "editor failed — draft kept at {}",
                    compose.path.display()
                ));
            }
        }
    }

    fn reprompt_send(&mut self) {
        let security = self
            .compose
            .as_ref()
            .map(|c| c.security)
            .unwrap_or(Security::None);
        let label = match security {
            Security::None => format!("{SEND_PROMPT}: "),
            s => format!("{SEND_PROMPT} [PGP: {}]: ", s.label()),
        };
        self.prompt = Some(Prompt::Key {
            label,
            kind: KeyKind::Send,
        });
    }

    /// Initial security for a fresh draft, from the [pgp] config.
    fn default_security(&self) -> Security {
        match (
            self.config.pgp.sign_by_default,
            self.config.pgp.encrypt_by_default,
        ) {
            (true, true) => Security::Both,
            (true, false) => Security::Sign,
            (false, true) => Security::Encrypt,
            (false, false) => Security::None,
        }
    }

    fn send_draft(&mut self) {
        let Some(compose_state) = self.compose.take() else {
            return;
        };
        let raw = match std::fs::read_to_string(&compose_state.path) {
            Ok(r) => r,
            Err(err) => {
                self.status = Some(format!("cannot read draft: {err}"));
                return;
            }
        };
        let host = maildir::hostname();
        let from = self
            .config
            .identity
            .from_line()
            .unwrap_or_else(|| default_from(&host));
        let final_text = match compose::finalize(
            &raw,
            &from,
            &compose::make_message_id(&host),
            &compose::rfc2822_now(),
        ) {
            Ok(t) => t,
            Err(err) => {
                self.status = Some(format!("{err} — press e to edit"));
                self.compose = Some(compose_state);
                self.reprompt_send();
                return;
            }
        };
        let final_text = match self.apply_security(compose_state.security, final_text) {
            Ok(t) => t,
            Err(err) => {
                self.status = Some(format!("pgp: {err:#} — e edits, s changes security"));
                self.compose = Some(compose_state);
                self.reprompt_send();
                return;
            }
        };
        let send_result = match self.smtp_account() {
            Some(account) => send_via_smtp(&account, &final_text),
            None => run_sendmail(final_text.as_bytes(), self.config.mail.sendmail.as_deref()),
        };
        match send_result {
            Ok(()) => {
                let mut note = String::from("message sent");
                match &mut self.remote {
                    // Fcc goes to the account's Sent folder on the server.
                    Some(remote) => match remote.append_sent(final_text.as_bytes()) {
                        Ok(folder) => note += &format!(", copy in {folder}"),
                        Err(_) => note += ", Fcc to Sent failed",
                    },
                    None => {
                        let sent_dir = self
                            .config
                            .mail
                            .sent
                            .as_deref()
                            .map(expand_tilde)
                            .filter(|p| p.join("cur").is_dir())
                            .or_else(|| maildir::find_special(&self.dir, &["sent", "sent-mail"]));
                        match sent_dir {
                            Some(sent) => {
                                let flags = maildir::Flags {
                                    seen: true,
                                    ..Default::default()
                                };
                                match maildir::deliver(&sent, final_text.as_bytes(), flags) {
                                    Ok(_) => note += ", copy in Sent",
                                    Err(_) => note += ", Fcc to Sent failed",
                                }
                            }
                            None => note += " (no Sent maildir, no copy kept)",
                        }
                    }
                }
                let _ = std::fs::remove_file(&compose_state.path);
                if let Some(src) = &compose_state.recall_source {
                    let _ = std::fs::remove_file(src);
                }
                self.status = Some(note);
            }
            Err(err) => {
                self.status = Some(format!("send failed: {err:#}"));
                self.compose = Some(compose_state);
                self.reprompt_send();
            }
        }
    }

    /// Apply the chosen PGP treatment to a finalized draft.
    fn apply_security(&self, security: Security, text: String) -> Result<String> {
        let cfg = &self.config.pgp;
        match security {
            Security::None => Ok(text),
            Security::Sign => pgp::sign_message(cfg, &text),
            Security::Encrypt | Security::Both => {
                // Encrypt to every recipient plus the sender, so the
                // Fcc copy stays readable.
                let (mut rcpts, _) = compose::smtp_envelope(&text)?;
                if let Some(from) = compose::from_address(&text) {
                    rcpts.push(from);
                }
                rcpts.sort();
                rcpts.dedup();
                pgp::encrypt_message(cfg, &rcpts, security == Security::Both, &text)
            }
        }
    }

    /// Which account to submit outgoing mail through. Explicit sendmail
    /// configuration ($RMUT_SENDMAIL or mail.sendmail) wins; otherwise
    /// the open mailbox's account, or the first one with an smtp_host.
    fn smtp_account(&self) -> Option<Account> {
        if std::env::var("RMUT_SENDMAIL").is_ok() || self.config.mail.sendmail.is_some() {
            return None;
        }
        if let Some(remote) = &self.remote
            && remote.account.smtp_host.is_some()
        {
            return Some(remote.account.clone());
        }
        self.config
            .accounts
            .iter()
            .find(|a| a.smtp_host.is_some())
            .cloned()
    }

    fn postponed_dir(&self) -> Option<PathBuf> {
        self.config
            .mail
            .postponed
            .as_deref()
            .map(expand_tilde)
            .filter(|p| p.join("cur").is_dir())
            .or_else(|| {
                maildir::find_special(&self.dir, &["drafts", "postponed", "rmut-postponed"])
            })
    }

    fn has_postponed(&self) -> bool {
        self.postponed_dir()
            .and_then(|d| maildir::scan(&d).ok())
            .is_some_and(|files| !files.is_empty())
    }

    fn postpone_draft(&mut self) {
        let Some(compose_state) = self.compose.take() else {
            return;
        };
        let target = match self.postponed_dir() {
            Some(d) => Ok(d),
            None => {
                let d = self.dir.join(".rmut-postponed");
                maildir::create(&d).map(|()| d)
            }
        };
        let result = target.and_then(|dir| {
            let bytes = std::fs::read(&compose_state.path)?;
            let flags = maildir::Flags {
                draft: true,
                seen: true,
                ..Default::default()
            };
            maildir::deliver(&dir, &bytes, flags)
        });
        match result {
            Ok(path) => {
                let _ = std::fs::remove_file(&compose_state.path);
                self.status = Some(format!("postponed to {}", path.display()));
            }
            Err(err) => {
                self.status = Some(format!(
                    "postpone failed: {err:#} — draft at {}",
                    compose_state.path.display()
                ));
            }
        }
    }

    fn recall_postponed(&mut self) {
        let Some(dir) = self.postponed_dir() else {
            self.status = Some("no postponed messages".into());
            return;
        };
        let newest = maildir::scan(&dir).ok().and_then(|files| {
            files
                .into_iter()
                .max_by_key(|f| f.path.metadata().and_then(|m| m.modified()).ok())
        });
        let Some(file) = newest else {
            self.status = Some("no postponed messages".into());
            return;
        };
        let result = std::fs::read_to_string(&file.path)
            .map_err(anyhow::Error::from)
            .and_then(|content| write_draft(&content));
        match result {
            Ok(path) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: Some(file.path),
                    security: self.default_security(),
                });
            }
            Err(err) => self.status = Some(format!("cannot recall: {err:#}")),
        }
    }

    // ---- shared helpers ----

    fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.sel = index.min(self.visible.len() - 1);
    }

    fn cur_mut(&mut self) -> Option<&mut Msg> {
        let i = self.visible.get(self.sel).copied()?;
        self.msgs.get_mut(i)
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.visible
            .get(self.sel)
            .map(|&i| self.msgs[i].env.file.path.clone())
    }

    fn mark_read(&mut self) {
        if let Some(m) = self.cur_mut()
            && (m.env.file.is_new || !m.env.file.flags.seen)
        {
            m.env.file.is_new = false;
            m.env.file.flags.seen = true;
            m.dirty = true;
        }
    }

    /// Fetch (IMAP), parse, and PGP-process a message the way the
    /// pager shows it.
    fn load_view(&mut self, path: &Path) -> Result<message::MessageView> {
        // Cached IMAP messages start header-only; get the body now.
        if let Some(remote) = &mut self.remote
            && remote::is_partial(path)
        {
            remote.fetch_body(path).context("cannot fetch message")?;
        }
        let mut view = message::load(path)?;
        // PGP messages: decrypt/verify via gpg, prepend the verdict
        // line to whatever body ends up shown.
        if let Ok(raw) = std::fs::read(path)
            && let Some(p) = pgp::view(&self.config.pgp, &raw)
        {
            if let Some(body) = p.body {
                view.body = body;
            }
            view.body = format!("{}\n\n{}", p.note, view.body);
        }
        Ok(view)
    }

    fn open_selected(&mut self) {
        self.mark_read();
        let Some(&i) = self.visible.get(self.sel) else {
            return;
        };
        let path = self.msgs[i].env.file.path.clone();
        match self.load_view(&path) {
            Ok(view) => {
                self.mode = Mode::Pager(Pager {
                    view,
                    scroll: 0,
                    full_headers: false,
                });
            }
            Err(err) => self.status = Some(format!("cannot open message: {err:#}")),
        }
    }

    fn confirm_print(&mut self) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::Key {
            label: "Print message? (y/n): ".into(),
            kind: KeyKind::Print,
        });
    }

    /// Pipe the message as displayed (brief headers, decoded body) to
    /// the configured print command, lpr by default.
    fn print_current(&mut self) {
        let Some(&i) = self.visible.get(self.sel) else {
            return;
        };
        let path = self.msgs[i].env.file.path.clone();
        let view = match self.load_view(&path) {
            Ok(v) => v,
            Err(err) => {
                self.status = Some(format!("cannot print: {err:#}"));
                return;
            }
        };
        let mut text = String::new();
        for (name, value) in &view.brief {
            text += &format!("{name}: {value}\n");
        }
        text.push('\n');
        text += &view.body;
        let command = self
            .config
            .mail
            .print
            .clone()
            .unwrap_or_else(|| "lpr".into());
        match pipe_to(&command, text.as_bytes()) {
            Ok(()) => self.status = Some(format!("printed via {command}")),
            Err(err) => self.status = Some(format!("print failed: {err:#}")),
        }
    }

    fn toggle_collapse(&mut self, all: bool) {
        if self.sort != SortKey::Threads {
            self.status = Some("folding needs thread sort (o t)".into());
            return;
        }
        let keep;
        if all {
            keep = self.selected_path();
            if self.collapsed.is_empty() {
                self.collapsed = self
                    .msgs
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| self.thread_depth.get(i) == Some(&0))
                    .map(|(_, m)| m.env.file.path.clone())
                    .collect();
            } else {
                self.collapsed.clear();
            }
        } else {
            let Some(&mi) = self.visible.get(self.sel) else {
                return;
            };
            let root = self.thread_root.get(mi).copied().unwrap_or(mi);
            let path = self.msgs[root].env.file.path.clone();
            if !self.collapsed.remove(&path) {
                self.collapsed.insert(path.clone());
            }
            keep = Some(path);
        }
        self.rebuild_visible(keep);
    }

    /// Rebuild `visible` from the limit and collapsed threads, keeping
    /// the selection on the message at `keep` when still visible.
    fn rebuild_visible(&mut self, keep: Option<PathBuf>) {
        let mut visible = Vec::with_capacity(self.msgs.len());
        for i in 0..self.msgs.len() {
            let limit_ok = match &self.limit {
                Some((_, patterns)) => pattern::matches(patterns, &self.msgs[i].env),
                None => true,
            };
            if !limit_ok {
                continue;
            }
            if self.limit.is_none()
                && self.sort == SortKey::Threads
                && self.thread_depth.get(i).copied().unwrap_or(0) > 0
            {
                let root = self.thread_root.get(i).copied().unwrap_or(i);
                if self.collapsed.contains(&self.msgs[root].env.file.path) {
                    continue;
                }
            }
            visible.push(i);
        }
        self.visible = visible;
        self.sel = keep
            .and_then(|p| {
                self.visible
                    .iter()
                    .position(|&i| self.msgs[i].env.file.path == p)
            })
            .unwrap_or_else(|| self.visible.len().saturating_sub(1));
    }

    fn apply_sort(&mut self) {
        let keep = self.selected_path();
        self.resort(keep);
    }

    fn resort(&mut self, keep: Option<PathBuf>) {
        if self.sort == SortKey::Threads {
            let items = {
                let envs: Vec<&Envelope> = self.msgs.iter().map(|m| &m.env).collect();
                thread::thread(&envs)
            };
            let mut old: Vec<Option<Msg>> = self.msgs.drain(..).map(Some).collect();
            let mut new_pos = vec![0usize; old.len()];
            for (pos, item) in items.iter().enumerate() {
                new_pos[item.index] = pos;
            }
            self.msgs = items
                .iter()
                .map(|item| {
                    old[item.index]
                        .take()
                        .expect("thread order is a permutation")
                })
                .collect();
            self.thread_depth = items.iter().map(|item| item.depth).collect();
            self.thread_root = items.iter().map(|item| new_pos[item.root]).collect();
            self.sort_rev = false;
        } else {
            let (sort, rev) = (self.sort, self.sort_rev);
            self.msgs.sort_by(|a, b| {
                let ord = match sort {
                    SortKey::Date => a.env.date.cmp(&b.env.date),
                    SortKey::From => a.env.from.to_lowercase().cmp(&b.env.from.to_lowercase()),
                    SortKey::Subject => {
                        subject_key(&a.env.subject).cmp(&subject_key(&b.env.subject))
                    }
                    SortKey::Size => a.env.file.size.cmp(&b.env.file.size),
                    SortKey::Threads => unreachable!(),
                };
                if rev { ord.reverse() } else { ord }
            });
            self.thread_depth = vec![0; self.msgs.len()];
            self.thread_root = (0..self.msgs.len()).collect();
        }
        self.rebuild_visible(keep);
    }

    fn search_next(&mut self) {
        let Some(patterns) = self.last_search.clone() else {
            self.status = Some("no search pattern (use /)".into());
            return;
        };
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len();
        for step in 1..=n {
            let vi = (self.sel + step) % n;
            if pattern::matches(&patterns, &self.msgs[self.visible[vi]].env) {
                if vi <= self.sel {
                    self.status = Some("search wrapped".into());
                }
                self.sel = vi;
                return;
            }
        }
        self.status = Some("not found".into());
    }

    /// Write all pending changes to the maildir: T-flagged messages are
    /// removed, other dirty messages are renamed with their new flags.
    /// For an IMAP mailbox the changes go to the server first (UID
    /// STORE / EXPUNGE); the local pass then updates the cache to match.
    fn sync(&mut self) {
        if let Some(remote) = &mut self.remote {
            let mut deletes: Vec<PathBuf> = Vec::new();
            let mut flag_pushes: Vec<(PathBuf, maildir::Flags)> = Vec::new();
            for m in &self.msgs {
                if m.env.file.flags.deleted {
                    deletes.push(m.env.file.path.clone());
                } else if m.dirty {
                    flag_pushes.push((m.env.file.path.clone(), m.env.file.flags));
                }
            }
            let result = flag_pushes
                .iter()
                .try_for_each(|(path, flags)| remote.push_flags(path, *flags))
                .and_then(|()| {
                    if deletes.is_empty() {
                        Ok(())
                    } else {
                        remote.delete(&deletes)
                    }
                });
            if let Err(err) = result {
                // Nothing applied locally: everything stays pending.
                self.status = Some(format!("sync failed: {err:#}"));
                return;
            }
        }
        let keep = self.selected_path();
        let mut removed = 0usize;
        let mut saved = 0usize;
        let mut errors: Vec<String> = Vec::new();
        self.msgs.retain_mut(|m| {
            if m.env.file.flags.deleted {
                match maildir::remove(&m.env.file) {
                    Ok(()) => {
                        removed += 1;
                        false
                    }
                    Err(err) => {
                        errors.push(err.to_string());
                        true
                    }
                }
            } else {
                if m.dirty {
                    match maildir::store_flags(&m.env.file) {
                        Ok(path) => {
                            m.env.file.path = path;
                            m.env.file.is_new = false;
                            m.dirty = false;
                            saved += 1;
                        }
                        Err(err) => errors.push(err.to_string()),
                    }
                }
                true
            }
        });
        self.dir_mtimes = dir_mtimes(&self.dir);
        self.resort(keep);
        self.status = Some(if errors.is_empty() {
            format!("synced: {removed} deleted, {saved} updated")
        } else {
            format!("sync errors: {}", errors.join("; "))
        });
    }
}

/// Sort key for subjects: case-insensitive, Re:/Fwd: prefixes stripped.
fn subject_key(subject: &str) -> String {
    let mut key = subject.trim().to_lowercase();
    loop {
        let stripped = key
            .strip_prefix("re:")
            .or_else(|| key.strip_prefix("fwd:"))
            .or_else(|| key.strip_prefix("fw:"))
            .map(|rest| rest.trim_start().to_string());
        match stripped {
            Some(next) => key = next,
            None => break,
        }
    }
    key
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(input)
}

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn write_draft(text: &str) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "rmut-draft-{}-{}.eml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn default_from(hostname: &str) -> String {
    if let Ok(email) = std::env::var("EMAIL") {
        return email;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    format!("{user}@{hostname}")
}

/// Run the account's password command once per session.
fn account_password(account: &Account) -> Result<String> {
    static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    let map = cache.get_or_insert_with(HashMap::new);
    if let Some(password) = map.get(&account.name) {
        return Ok(password.clone());
    }
    let password = account.password()?;
    map.insert(account.name.clone(), password.clone());
    Ok(password)
}

fn send_via_smtp(account: &Account, text: &str) -> Result<()> {
    let from = compose::from_address(text).context("cannot parse the From address")?;
    let (rcpts, text) = compose::smtp_envelope(text)?;
    anyhow::ensure!(!rcpts.is_empty(), "no recipient addresses");
    let password = account_password(account)?;
    smtp::send(account, &password, &from, &rcpts, text.as_bytes())
}

/// Run a shell command with `bytes` on its stdin.
fn pipe_to(command: &str, bytes: &[u8]) -> Result<()> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {command}"))?;
    child
        .stdin
        .take()
        .context("no stdin on print child")?
        .write_all(bytes)?;
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "{command} exited with {status}");
    Ok(())
}

fn run_sendmail(bytes: &[u8], configured: Option<&str>) -> Result<()> {
    let command = std::env::var("RMUT_SENDMAIL")
        .ok()
        .or_else(|| configured.map(String::from));
    let (prog, mut args) = match command {
        Some(v) => {
            let mut it = v.split_whitespace().map(String::from);
            let p = it.next().context("sendmail command is empty")?;
            (p, it.collect::<Vec<_>>())
        }
        None => {
            let p = if Path::new("/usr/sbin/sendmail").exists() {
                "/usr/sbin/sendmail".to_string()
            } else {
                "sendmail".to_string()
            };
            (p, Vec::new())
        }
    };
    args.extend(["-t".into(), "-oi".into()]);
    let mut child = Command::new(&prog)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {prog}"))?;
    child
        .stdin
        .take()
        .context("no stdin on sendmail child")?
        .write_all(bytes)?;
    let status = child.wait()?;
    anyhow::ensure!(status.success(), "{prog} exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::subject_key;

    #[test]
    fn subject_key_strips_reply_prefixes() {
        assert_eq!(subject_key("Re: Re: Lunch"), "lunch");
        assert_eq!(subject_key("FWD: re: x"), "x");
        assert_eq!(subject_key("Redo"), "redo");
    }
}
