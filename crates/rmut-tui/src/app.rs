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
use rmut_core::{alias, compose, hdrcache, maildir, mbox, message, pgp, smtp, thread};

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
        /// Paths or `imap:` specs (ready for `open_mailbox_spec`) with
        /// their new/unseen counts.
        dirs: Vec<(String, usize)>,
        sel: usize,
    },
    /// Picking one of several postponed drafts to recall.
    Postponed {
        drafts: Vec<(PathBuf, String)>,
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
    SaveMsg,
    CopyMsg,
    Pipe,
    BounceTo,
    ComposeTo,
    ComposeSubject,
    /// A file path to add as an `Attach:` line (send prompt `a`).
    AttachFile,
    /// The nick for create-alias; the address waits in `alias_addr`.
    AliasNick,
}

#[derive(Clone, Copy)]
pub enum KeyKind {
    Sort,
    Send,
    Security,
    Recall,
    Print,
    /// Confirm sending the message in `App::bounce_to`.
    Bounce,
    /// Confirm expunging deleted messages; `quit` leaves afterwards.
    Purge {
        quit: bool,
    },
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
    /// Original message to attach as message/rfc822 (forward =
    /// "attach", mutt's mime_forward).
    pub attach: Option<PathBuf>,
    /// Header block withheld from the editor (edit_headers = false);
    /// draft_full puts it back for send/postpone/attachments.
    pub hidden_head: Option<String>,
}

/// The draft as a full message: the file as edited, with any withheld
/// header block put back in front.
fn draft_full(c: &Compose) -> std::io::Result<String> {
    let text = std::fs::read_to_string(&c.path)?;
    Ok(match &c.hidden_head {
        Some(head) => format!("{}\n\n{}", head.trim_end(), text),
        None => text,
    })
}

const SEND_PROMPT: &str =
    "Send message? (y)es (e)dit (a)ttach (v)iew att. (s)ecurity (p)ostpone (q)discard";

/// Tab-completion state at an address prompt: candidates for the token
/// at `start`, `expect` being the whole buffer after the last
/// insertion (an edit in between restarts the match).
struct Complete {
    start: usize,
    candidates: Vec<String>,
    index: usize,
    expect: String,
}

pub struct App {
    pub dir: PathBuf,
    /// What the status line calls this mailbox: the path for local
    /// maildirs, the `imap:account/folder` spec for remote ones.
    pub title: String,
    /// Set when `dir` is the cache maildir of an IMAP folder.
    remote: Option<Remote>,
    /// Set when `dir` mirrors an mbox file; sync writes back into it.
    mbox: Option<mbox::Mbox>,
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
    /// My own addresses (identity, accounts, $EMAIL), lowercase, for
    /// the addressed-to-me index mark.
    pub me: Vec<String>,
    compose_setup: Option<ComposeSetup>,
    compose: Option<Compose>,
    pending_editor: Option<Compose>,
    /// Recipients waiting for the bounce confirmation.
    bounce_to: Option<String>,
    /// The sender address waiting for a create-alias nick.
    alias_addr: Option<String>,
    /// Background IDLE watcher for the open IMAP folder.
    idle: Option<remote::IdleWatch>,
    /// Address completion state at the To prompt (Tab cycles).
    complete: Option<Complete>,
    /// Keys queued by a macro, consumed before real terminal input.
    pending_keys: std::collections::VecDeque<KeyEvent>,
    /// Compiled [[color_index]] rules: (patterns, style patch); the
    /// first matching rule colors the line.
    pub index_rules: Vec<(Vec<Pattern>, ratatui::style::Style)>,
    /// The left mailbox pane: (spec, new/unseen count) entries.
    pub sidebar: Vec<(String, usize)>,
    pub sidebar_sel: usize,
    /// Which entry is the open mailbox (for its > marker).
    pub sidebar_open: Option<usize>,
    pub sidebar_visible: bool,
    /// New-mail counts of the other configured mailboxes at the last
    /// poll, to notice growth (mutt's `mailboxes` awareness).
    mailbox_new: HashMap<String, usize>,
    /// `;` was pressed: the next flag operation applies to tagged messages.
    tag_next: bool,
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
        // The header cache spares re-parsing every message on open.
        let (envelopes, skipped) = hdrcache::load_envelopes(dir)?;
        let mut msgs: Vec<Msg> = envelopes
            .into_iter()
            .map(|env| Msg { env, dirty: false })
            .collect();
        // Mutt's default sort: date, oldest first.
        msgs.sort_by_key(|m| m.env.date);
        let visible: Vec<usize> = (0..msgs.len()).collect();
        let sel = visible.len().saturating_sub(1);
        let (theme, mut warnings) = Theme::from_config(&config);
        let (keymap, key_warnings) = Keymap::with_config(
            &config.keys.index,
            &config.keys.pager,
            &config.macros.index,
            &config.macros.pager,
        );
        warnings.extend(key_warnings);
        if skipped > 0 {
            warnings.push(format!("{skipped} unreadable message(s) skipped"));
        }
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
        let status = (!warnings.is_empty()).then(|| warnings.join("; "));
        let count = msgs.len();
        let mut me: Vec<String> = config
            .identity
            .email
            .iter()
            .chain(config.accounts.iter().map(|a| &a.user))
            .chain(
                config
                    .accounts
                    .iter()
                    .filter_map(|a| a.identity.as_ref().and_then(|i| i.email.as_ref())),
            )
            .chain(config.identities.iter().filter_map(|r| r.email.as_ref()))
            .map(|a| a.to_lowercase())
            .collect();
        if let Ok(email) = std::env::var("EMAIL") {
            me.push(email.to_lowercase());
        }
        let config_sidebar_visible = config.sidebar.visible;
        let mut app = App {
            dir: dir.to_path_buf(),
            title: dir.display().to_string(),
            remote: None,
            mbox: None,
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
            me,
            compose_setup: None,
            compose: None,
            pending_editor: None,
            bounce_to: None,
            alias_addr: None,
            idle: None,
            complete: None,
            pending_keys: std::collections::VecDeque::new(),
            index_rules,
            sidebar: Vec::new(),
            sidebar_sel: 0,
            sidebar_open: None,
            sidebar_visible: config_sidebar_visible,
            mailbox_new: HashMap::new(),
            tag_next: false,
            quit: false,
        };
        if let Some(spec) = app.config.index.sort.clone() {
            match parse_sort(&spec) {
                Some((sort, rev)) => {
                    app.sort = sort;
                    app.sort_rev = rev;
                    app.resort(None);
                }
                None => {
                    let warn = format!("unknown sort {spec:?} in config");
                    app.status = Some(match app.status.take() {
                        Some(prev) => format!("{prev}; {warn}"),
                        None => warn,
                    });
                }
            }
        }
        Ok(app)
    }

    /// Open a mailbox by spec: an `imap:account[/folder]` string (the
    /// folder is mirrored into a cache maildir) or a local path.
    pub fn open_spec(spec: &str, config: Config) -> Result<Self> {
        let mut app = Self::open_spec_inner(spec, config)?;
        app.refresh_sidebar();
        Ok(app)
    }

    fn open_spec_inner(spec: &str, config: Config) -> Result<Self> {
        match remote::parse_spec(spec) {
            Some((account_name, mailbox)) => {
                let account = config
                    .account(account_name)
                    .with_context(|| format!("no account {account_name} in config"))?
                    .clone();
                let password = account_password(&account)?;
                let remote = Remote::open(&account, mailbox, &password, Box::new(progress))?;
                let cache = remote.cache.clone();
                let mut app = App::open(&cache, config)?;
                app.title = remote.spec.clone();
                // IDLE on a second connection; NOOP polling stays as
                // the fallback when the server doesn't support it.
                app.idle = Some(remote::idle_watch(&account, &remote.mailbox, &password));
                app.remote = Some(remote);
                Ok(app)
            }
            None => {
                let path = expand_tilde(spec);
                if path.is_file() {
                    return App::open_mbox(&path, config);
                }
                App::open(&path, config)
            }
        }
    }

    /// Open an mbox file (e.g. /var/mail/$USER) through its cache
    /// mirror; `$` sync writes changes back into the file.
    fn open_mbox(path: &Path, config: Config) -> Result<Self> {
        let mbox = mbox::Mbox::open(path)?;
        let cache = mbox.cache.clone();
        let mut app = App::open(&cache, config)?;
        app.title = path.display().to_string();
        app.mbox = Some(mbox);
        Ok(app)
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
            if PROGRESS_DIRTY.swap(false, std::sync::atomic::Ordering::Relaxed) {
                terminal.clear()?;
            }
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
                self.status = None;
                self.handle_key(key, size.width as usize, size.height as usize);
            }
            // The IDLE watcher makes server changes show up within a
            // loop tick instead of waiting out the poll interval.
            let idle_kick = self.idle.as_ref().is_some_and(|w| w.take_changed());
            if idle_kick || last_poll.elapsed() >= poll_every {
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
        } else if matches!(self.mode, Mode::Postponed { .. }) {
            self.handle_postponed_key(key);
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
        if let Some(mbox) = &mut self.mbox {
            // Re-mirror when the file changed; same rescan pickup.
            if let Err(err) = mbox.refresh() {
                self.status = Some(format!("mbox: {err:#}"));
            }
        }
        self.check_other_mailboxes();
        self.refresh_sidebar();
        let current = dir_mtimes(&self.dir);
        if current == self.dir_mtimes {
            return;
        }
        self.rescan();
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
        for spec in self.config.mail.mailboxes.clone() {
            let count = match remote::parse_spec(&spec) {
                Some((account, folder)) => match &mut self.remote {
                    Some(remote) if remote.account.name == account => remote.unseen(folder),
                    _ => 0, // other accounts: no connection just for a count
                },
                None => maildir::new_count(&expand_tilde(&spec)),
            };
            entries.push((spec, count));
        }
        let (title, dir) = (self.title.clone(), self.dir.clone());
        let open = |e: &(String, usize)| e.0 == title || expand_tilde(&e.0) == dir;
        if !entries.iter().any(&open) {
            entries.insert(0, (self.title.clone(), self.new_count()));
        }
        self.sidebar_open = entries.iter().position(&open);
        self.sidebar_sel = keep
            .and_then(|k| entries.iter().position(|e| e.0 == k))
            .or(self.sidebar_open)
            .unwrap_or(0);
        self.sidebar = entries;
    }

    /// Watch the other configured local mailboxes for growth in their
    /// new/ (mutt's `mailboxes`); the first poll only sets a baseline.
    fn check_other_mailboxes(&mut self) {
        let mut grew: Vec<String> = Vec::new();
        for spec in self.config.mail.mailboxes.clone() {
            if spec.starts_with("imap:") || spec == self.title {
                continue; // other accounts are not worth a connection
            }
            let dir = expand_tilde(&spec);
            if dir == self.dir || !dir.join("new").is_dir() {
                continue;
            }
            let count = maildir::new_count(&dir);
            let prev = self.mailbox_new.insert(spec.clone(), count);
            if prev.is_some_and(|p| count > p) {
                grew.push(spec);
            }
        }
        // The open mailbox's own announcement (from the rescan) wins.
        if !grew.is_empty() && self.status.is_none() {
            self.status = Some(format!("new mail in {}", grew.join(", ")));
        }
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
                // Leaving the attachment review resumes the send flow.
                if self.compose.is_some() {
                    self.reprompt_send();
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
                let kind = *kind;
                self.prompt = None;
                self.run_key_prompt(kind, key.code);
            }
            Some(Prompt::Line { buf, .. }) => match key.code {
                KeyCode::Esc => {
                    self.prompt = None;
                    self.compose_setup = None;
                    self.bounce_to = None;
                    self.alias_addr = None;
                    // Escaping a sub-prompt of the send flow (attach
                    // file) returns to the send prompt.
                    if self.compose.is_some() {
                        self.reprompt_send();
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char('u') if is_ctrl(&key) => buf.clear(),
                KeyCode::Char(c) if !is_ctrl(&key) => buf.push(c),
                KeyCode::Tab => self.tab_complete(),
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
            KeyKind::Purge { quit } => match code {
                // y expunges; n writes flag changes but keeps the
                // messages marked deleted, like mutt.
                KeyCode::Char('y') | KeyCode::Char('n') => {
                    self.sync(code == KeyCode::Char('y'));
                    if quit {
                        self.quit = true;
                    }
                }
                _ => {}
            },
            KeyKind::Send => match code {
                KeyCode::Char('y') => self.send_draft(),
                KeyCode::Char('e') => {
                    if let Some(c) = self.compose.take() {
                        self.pending_editor = Some(c);
                    }
                }
                KeyCode::Char('a') => {
                    self.prompt = Some(Prompt::Line {
                        label: "Attach file: ".into(),
                        buf: String::new(),
                        kind: LineKind::AttachFile,
                    });
                }
                KeyCode::Char('v') => self.review_attachments(),
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
            KeyKind::Bounce => {
                let to = self.bounce_to.take();
                if code == KeyCode::Char('y')
                    && let Some(to) = to
                {
                    self.bounce_current(&to);
                }
            }
        }
    }

    /// Tab at a prompt: at an address prompt, complete the token under
    /// the cursor against aliases and query_command; at a mailbox
    /// prompt (c, save, copy), complete the buffer against the folder
    /// candidates — and with nothing typed at the c prompt, open the
    /// folder browser instead. Repeated Tab cycles the candidates.
    fn tab_complete(&mut self) {
        let (buf_now, kind) = match &self.prompt {
            Some(Prompt::Line { buf, kind, .. }) => (buf.clone(), *kind),
            _ => return,
        };
        let is_addr = matches!(kind, LineKind::ComposeTo | LineKind::BounceTo);
        let is_mbox = matches!(
            kind,
            LineKind::ChangeDir | LineKind::SaveMsg | LineKind::CopyMsg
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
            if let Some(Prompt::Line { buf, .. }) = &mut app.prompt {
                *buf = text.to_string();
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
            self.status = Some(note);
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
            self.status = Some("nothing to complete".into());
            return;
        }
        let candidates = if is_addr {
            alias::complete(
                &word,
                &alias::load_default(),
                self.config.mail.query_command.as_deref(),
            )
        } else {
            let specs = match self.folder_candidates() {
                Ok(specs) => specs,
                Err(err) => {
                    self.status = Some(format!("cannot list folders: {err:#}"));
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
            0 => self.status = Some(format!("no matches for {word}")),
            n => {
                let next = format!("{}{}", &buf_now[..start], candidates[0]);
                set_buf(self, &next);
                if n > 1 {
                    self.status = Some(format!("match 1/{n} (Tab cycles)"));
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
        match kind {
            LineKind::Limit => {
                let keep = self.selected_path();
                if input.is_empty() || input == "all" {
                    self.limit = None;
                } else {
                    match pattern::parse(input) {
                        Ok(patterns) => self.limit = Some((input.to_string(), patterns)),
                        Err(err) => {
                            self.status = Some(format!("bad pattern: {err}"));
                            return;
                        }
                    }
                }
                self.rebuild_visible(keep);
                if self.visible.is_empty() {
                    self.status = Some("no messages match the limit".into());
                }
            }
            LineKind::Search => {
                if !input.is_empty() {
                    match pattern::parse(input) {
                        Ok(patterns) => self.last_search = Some(patterns),
                        Err(err) => {
                            self.status = Some(format!("bad pattern: {err}"));
                            return;
                        }
                    }
                }
                self.search_next();
            }
            LineKind::ChangeDir => self.open_mailbox_spec(input),
            LineKind::SavePart => self.save_part(input),
            LineKind::SaveMsg => self.copy_message(input, true),
            LineKind::CopyMsg => self.copy_message(input, false),
            LineKind::Pipe => self.pipe_message(input),
            LineKind::BounceTo => self.bounce_to_submitted(input),
            LineKind::AttachFile => self.attach_file_submitted(input),
            LineKind::AliasNick => self.create_alias(input),
            LineKind::ComposeTo => self.setup_to_submitted(input),
            LineKind::ComposeSubject => self.finish_compose_setup(input),
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
        match action {
            IndexAction::Tag => {
                if let Some(m) = self.cur_mut() {
                    m.env.tagged = !m.env.tagged;
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::TagPrefix => {
                if self.msgs.iter().any(|m| m.env.tagged) {
                    self.tag_next = true;
                    self.status = Some("apply next function to tagged messages".into());
                } else {
                    self.status = Some("no tagged messages".into());
                }
            }
            IndexAction::FetchMail => {
                self.check_new_mail();
                if self.status.is_none() {
                    self.status = Some("checked for new mail".into());
                }
            }
            IndexAction::Save => self.prompt_copy(true),
            IndexAction::Copy => self.prompt_copy(false),
            IndexAction::Pipe => self.prompt_pipe(),
            IndexAction::Bounce => self.prompt_bounce(),
            IndexAction::Resend => self.resend_current(),
            IndexAction::CreateAlias => self.prompt_create_alias(),
            IndexAction::Quit => {
                // Like mutt: flag changes are written silently; only
                // pending deletions raise a question (the purge one).
                if self.deleted_count() > 0 {
                    self.prompt_purge(true);
                } else {
                    if self.pending_count() > 0 {
                        self.sync(true);
                    }
                    self.quit = true;
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
                if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.deleted = true);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::Undelete => {
                if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.deleted = false);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                }
            }
            IndexAction::Flag => {
                if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.flagged = !m.env.file.flags.flagged);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.flagged = !m.env.file.flags.flagged;
                    m.dirty = true;
                }
            }
            IndexAction::ToggleNew => {
                if apply_tagged {
                    self.each_tagged(|m| {
                        m.env.file.flags.seen = !m.env.file.flags.seen;
                        m.env.file.is_new = false;
                    });
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.seen = !m.env.file.flags.seen;
                    m.env.file.is_new = false;
                    m.dirty = true;
                }
            }
            IndexAction::Sync => {
                if self.deleted_count() > 0 {
                    self.prompt_purge(false);
                } else {
                    self.sync(true);
                }
            }
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
                    label: "Limit (~f/~s/~b/~t/~c/~d/flags, ! | (), empty=all): ".into(),
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
                if self.ready_to_leave() {
                    self.prompt = Some(Prompt::Line {
                        label: "Open mailbox (Tab completes): ".into(),
                        buf: String::new(),
                        kind: LineKind::ChangeDir,
                    });
                }
            }
            IndexAction::Folders => self.open_folder_browser(),
            IndexAction::Print => self.confirm_print(),
            IndexAction::SidebarToggle => {
                self.sidebar_visible = !self.sidebar_visible;
                self.refresh_sidebar();
            }
            IndexAction::SidebarNext | IndexAction::SidebarPrev => {
                if !self.sidebar_visible {
                    self.status = Some("the sidebar is hidden — B shows it".into());
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
                    self.status = Some("the sidebar is hidden — B shows it".into());
                } else if self.ready_to_leave()
                    && let Some((spec, _)) = self.sidebar.get(self.sidebar_sel).cloned()
                {
                    self.open_mailbox_spec(&spec);
                }
            }
            IndexAction::Help => self.open_help(),
        }
    }

    // ---- pager ----

    fn handle_pager_key(&mut self, key: KeyEvent, width: usize, page: usize) {
        if let Some(seq) = self.keymap.lookup_pager_macro(&key) {
            self.replay(seq.to_vec());
            return;
        }
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
            PagerAction::Save => {
                self.prompt_copy(true);
                return;
            }
            PagerAction::Copy => {
                self.prompt_copy(false);
                return;
            }
            PagerAction::Pipe => {
                self.prompt_pipe();
                return;
            }
            PagerAction::Bounce => {
                self.prompt_bounce();
                return;
            }
            PagerAction::Resend => {
                self.resend_current();
                return;
            }
            PagerAction::CreateAlias => {
                self.prompt_create_alias();
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
            .saturating_sub(self.config.pager.index_lines as usize)
            .max(1);
        let step = page.saturating_sub(self.config.pager.context).max(1);
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
            PagerAction::PageDown => pager.scroll = (pager.scroll + step).min(max_scroll),
            PagerAction::PageUp => pager.scroll = pager.scroll.saturating_sub(step),
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
            // A configured filter can still render it (auto_view).
            match self.config.filters.get(&mimetype).cloned() {
                Some(command) => {
                    match message::filter_part(&msg_path, index, &command) {
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
                        Err(err) => self.status = Some(format!("filter failed: {err:#}")),
                    }
                    return;
                }
                None => {
                    self.status = Some(format!("{mimetype} is not text — save it with s"));
                    return;
                }
            }
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
                    Mode::Folders { dirs, sel } => dirs.get(*sel).map(|d| d.0.clone()),
                    _ => None,
                };
                if let Some(spec) = spec {
                    self.open_mailbox_spec(&spec);
                }
            }
            _ => {}
        }
    }

    /// Mailbox specs for the folder browser and for Tab completion at
    /// a mailbox prompt: the configured mailboxes, the open account's
    /// folders (IMAP LIST), and maildirs discovered next to the open
    /// one.
    fn folder_candidates(&mut self) -> Result<Vec<(String, usize)>> {
        // Local entries carry their new/ count; imap: specs of other
        // accounts show without one (no connection just for a count).
        let mut dirs: Vec<(String, usize)> = self
            .config
            .mail
            .mailboxes
            .iter()
            .filter(|m| m.starts_with("imap:") || expand_tilde(m).join("cur").is_dir())
            .map(|m| {
                let count = if m.starts_with("imap:") {
                    0
                } else {
                    maildir::new_count(&expand_tilde(m))
                };
                (m.clone(), count)
            })
            .collect();
        match &mut self.remote {
            Some(remote) => {
                let account = remote.account.name.clone();
                dirs.extend(
                    remote
                        .folders()?
                        .into_iter()
                        .map(|(f, unseen)| (format!("imap:{account}/{f}"), unseen)),
                );
            }
            None => dirs.extend(
                maildir::discover(&self.dir)
                    .iter()
                    .map(|p| (p.display().to_string(), maildir::new_count(p))),
            ),
        }
        dirs.sort();
        dirs.dedup_by(|a, b| {
            if a.0 == b.0 {
                b.1 = b.1.max(a.1);
                true
            } else {
                false
            }
        });
        Ok(dirs)
    }

    fn open_folder_browser(&mut self) {
        if !self.ready_to_leave() {
            return;
        }
        let dirs = match self.folder_candidates() {
            Ok(dirs) => dirs,
            Err(err) => {
                self.status = Some(format!("cannot list folders: {err:#}"));
                return;
            }
        };
        if dirs.is_empty() {
            self.status = Some("no maildirs found next to this one".into());
            return;
        }
        let sel = dirs
            .iter()
            .position(|d| d.0 == self.title || expand_tilde(&d.0) == self.dir)
            .unwrap_or(0);
        self.mode = Mode::Folders { dirs, sel };
    }

    /// Leaving the mailbox (c, sidebar open, folder browser): flag
    /// changes are written silently like q; only pending deletions
    /// block the switch. True when it is safe to go.
    fn ready_to_leave(&mut self) -> bool {
        if self.deleted_count() > 0 {
            self.status = Some("deleted messages pending — sync with $ or undelete first".into());
            return false;
        }
        if self.pending_count() > 0 {
            self.sync(false);
            if self.pending_count() > 0 {
                return false; // sync failed; its status says why
            }
        }
        true
    }

    fn open_mailbox_spec(&mut self, spec: &str) {
        // Another folder of the open account reuses the live session
        // (a SELECT) instead of a fresh connect+login round; a dead
        // session falls through to the full open below.
        if let Some((account_name, mailbox)) = remote::parse_spec(spec)
            && self
                .remote
                .as_ref()
                .is_some_and(|r| r.account.name == account_name)
            && self.remote.as_mut().unwrap().switch(mailbox).is_ok()
        {
            let remote = self.remote.take().unwrap();
            match App::open(&remote.cache.clone(), self.config.clone()) {
                Ok(mut app) => {
                    app.title = remote.spec.clone();
                    if let Ok(password) = account_password(&remote.account) {
                        app.idle = Some(remote::idle_watch(
                            &remote.account,
                            &remote.mailbox,
                            &password,
                        ));
                    }
                    app.remote = Some(remote);
                    app.sidebar_visible = self.sidebar_visible;
                    app.refresh_sidebar();
                    *self = app;
                }
                Err(err) => self.status = Some(format!("cannot open {spec}: {err:#}")),
            }
            return;
        }
        match App::open_spec(spec, self.config.clone()) {
            Ok(mut app) => {
                // The runtime sidebar toggle survives a mailbox switch.
                app.sidebar_visible = self.sidebar_visible;
                app.refresh_sidebar();
                *self = app;
            }
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
        let mut attach = None;
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
                ComposeKind::Forward if self.forward_attaches() => {
                    // The original goes along whole; nothing to quote.
                    attach = Some(b.path.clone());
                }
                ComposeKind::Forward => {
                    let orig = message::body_text(&b.path).unwrap_or_default();
                    body = compose::forward_body(&b.from_display, b.date, &b.subject, &orig);
                }
                ComposeKind::New => {}
            }
        }
        let from = self.compose_from(setup.base.as_ref(), &to);
        let text = compose::draft_text(
            &compose::DraftHeaders {
                from,
                to,
                cc,
                subject: subject.to_string(),
                in_reply_to,
                references,
            },
            &body,
        );
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: None,
                    security: self.default_security(),
                    attach,
                    hidden_head,
                });
            }
            Err(err) => self.status = Some(format!("cannot write draft: {err:#}")),
        }
    }

    /// mutt's edit_headers, but true by default: the header block is
    /// part of the editor buffer.
    fn edit_headers(&self) -> bool {
        self.config.mail.edit_headers.unwrap_or(true)
    }

    /// Write a fresh draft file for the editor: the whole text, or —
    /// with edit_headers = false — only the body, the header block
    /// withheld for draft_full to rejoin.
    fn stage_draft(&self, text: &str) -> Result<(PathBuf, Option<String>)> {
        if self.edit_headers() {
            return Ok((write_draft(text)?, None));
        }
        let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
        Ok((write_draft(body)?, Some(head.to_string())))
    }

    /// From line for a new draft: reverse_name picks the address the
    /// replied-to message came to; otherwise the layered identity
    /// (global, account, matching [[identities]] rules).
    fn compose_from(&self, base: Option<&ComposeBase>, to: &str) -> Option<String> {
        if self.config.identity.reverse_name
            && let Some(b) = base
            && let Some(from) = compose::reverse_from(&b.orig_to, &b.orig_cc, &self.me)
        {
            return Some(from);
        }
        let rcpts = compose::addresses(to);
        self.current_identity(&rcpts).from_line()
    }

    /// The identity in effect for this mailbox (and, when known, the
    /// draft's recipients).
    fn current_identity(&self, rcpts: &[String]) -> rmut_core::config::Identity {
        self.config
            .identity_for(&self.title, rcpts, self.remote.as_ref().map(|r| &r.account))
    }

    fn forward_attaches(&self) -> bool {
        self.config.mail.forward.as_deref() == Some("attach")
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
        let (security, attachments) = match &self.compose {
            Some(c) => {
                let files = draft_full(c)
                    .map(|text| compose::extract_attachments(&text).1.len())
                    .unwrap_or(0);
                (c.security, files + usize::from(c.attach.is_some()))
            }
            None => (Security::None, 0),
        };
        let mut label = SEND_PROMPT.to_string();
        if attachments > 0 {
            label += &format!(" [{attachments} attachment(s)]");
        }
        if security != Security::None {
            label += &format!(" [PGP: {}]", security.label());
        }
        label += ": ";
        self.prompt = Some(Prompt::Key {
            label,
            kind: KeyKind::Send,
        });
    }

    /// Add an `Attach:` line to the draft's header block without a
    /// trip through the editor (send prompt `a`).
    fn attach_file_submitted(&mut self, input: &str) {
        let input = input.trim();
        if !input.is_empty() {
            if !expand_tilde(input).is_file() {
                self.status = Some(format!("{input} is not a file"));
            } else if let Some(c) = &mut self.compose {
                // Quote paths with spaces the way extract_attachments
                // reads them back.
                let value = if input.contains(char::is_whitespace) && !input.starts_with('"') {
                    format!("\"{input}\"")
                } else {
                    input.to_string()
                };
                let result = match &mut c.hidden_head {
                    // Withheld headers: the Attach: line joins them.
                    Some(head) => {
                        *head = format!("{}\nAttach: {value}", head.trim_end());
                        Ok(())
                    }
                    None => std::fs::read_to_string(&c.path).and_then(|text| {
                        let updated = match text.split_once("\n\n") {
                            Some((head, body)) => format!("{head}\nAttach: {value}\n\n{body}"),
                            None => format!("{}\nAttach: {value}\n", text.trim_end()),
                        };
                        std::fs::write(&c.path, updated)
                    }),
                };
                if let Err(err) = result {
                    self.status = Some(format!("cannot attach: {err}"));
                }
            }
        }
        self.reprompt_send();
    }

    /// A read-only listing of the draft's attachments (send prompt
    /// `v`); closing it returns to the send prompt.
    fn review_attachments(&mut self) {
        let Some(c) = &self.compose else {
            return;
        };
        let text = draft_full(c).unwrap_or_default();
        let (_, files) = compose::extract_attachments(&text);
        let mut lines = vec!["Draft attachments".to_string(), String::new()];
        if let Some(orig) = &c.attach {
            lines.push(format!(
                "  {:<28} message/rfc822  forwarded original",
                orig.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(original)"),
            ));
        }
        for a in &files {
            let size = std::fs::metadata(&a.path).map(|m| m.len());
            let size = match size {
                Ok(n) => crate::ui::humanize_size(n),
                Err(_) => "missing!".into(),
            };
            lines.push(format!(
                "  {:<28} {:>8}  {}{}",
                a.path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                size,
                compose::content_type(&a.path),
                a.description
                    .as_deref()
                    .map(|d| format!("  ({d})"))
                    .unwrap_or_default(),
            ));
        }
        if files.is_empty() && c.attach.is_none() {
            lines.push("  (none — a at the send prompt adds one)".into());
        }
        lines.extend([String::new(), "q returns to the send prompt".into()]);
        self.mode = Mode::Help { lines, scroll: 0 };
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
        let raw = match draft_full(&compose_state) {
            Ok(r) => r,
            Err(err) => {
                self.status = Some(format!("cannot read draft: {err}"));
                return;
            }
        };
        let (raw, files) = compose::extract_attachments(&raw);
        let host = maildir::hostname();
        let from = self
            .current_identity(&[])
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
        let original = match &compose_state.attach {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    self.status = Some(format!("cannot attach the original: {err}"));
                    self.compose = Some(compose_state);
                    self.reprompt_send();
                    return;
                }
            },
            None => None,
        };
        let final_text = match self.secure_message(
            compose_state.security,
            final_text,
            &files,
            original.as_deref(),
        ) {
            Ok(t) => t,
            Err(err) => {
                self.status = Some(format!("{err:#} — e edits, s changes security"));
                self.compose = Some(compose_state);
                self.reprompt_send();
                return;
            }
        };
        let send_result = match self.smtp_account() {
            Some(account) => send_via_smtp(&account, &final_text),
            None => run_sendmail(
                final_text.as_bytes(),
                self.config.mail.sendmail.as_deref(),
                None,
            ),
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

    /// Assemble the outgoing message from a finalized draft: `files`
    /// and the forwarded `original` first turn it into multipart/mixed,
    /// then the chosen PGP treatment wraps whatever entity resulted.
    fn secure_message(
        &self,
        security: Security,
        text: String,
        files: &[compose::Attachment],
        original: Option<&[u8]>,
    ) -> Result<String> {
        let cfg = &self.config.pgp;
        // Encrypt to every recipient plus the sender, so the Fcc copy
        // stays readable.
        let recipients = |text: &str| -> Result<Vec<String>> {
            let (mut rcpts, _) = compose::smtp_envelope(text)?;
            if let Some(from) = compose::from_address(text) {
                rcpts.push(from);
            }
            rcpts.sort();
            rcpts.dedup();
            Ok(rcpts)
        };
        if files.is_empty() && original.is_none() {
            return match security {
                Security::None => Ok(text),
                Security::Sign => pgp::sign_message(cfg, &text),
                Security::Encrypt | Security::Both => pgp::encrypt_message(
                    cfg,
                    &recipients(&text)?,
                    security == Security::Both,
                    &text,
                ),
            };
        }
        let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
        let entity = compose::mixed_entity(body, files, original)?;
        match security {
            Security::None => Ok(format!("{}\nMIME-Version: 1.0\n{entity}", head.trim_end())),
            Security::Sign => pgp::sign_entity(cfg, head, entity.as_bytes()),
            Security::Encrypt | Security::Both => pgp::encrypt_entity(
                cfg,
                &recipients(&text)?,
                security == Security::Both,
                head,
                entity.as_bytes(),
            ),
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
            // The full message, headers included, so the recall (and
            // the postponed picker's subject) sees them.
            let bytes = draft_full(&compose_state)?.into_bytes();
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

    /// Recall a postponed draft: straight into the editor when there
    /// is one, a picker when there are several.
    fn recall_postponed(&mut self) {
        let files = self
            .postponed_dir()
            .and_then(|dir| maildir::scan(&dir).ok())
            .unwrap_or_default();
        let mut files = files;
        if files.is_empty() {
            self.status = Some("no postponed messages".into());
            return;
        }
        if files.len() == 1 {
            let file = files.remove(0);
            self.recall_file(file.path);
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

    fn recall_file(&mut self, source: PathBuf) {
        let result = std::fs::read_to_string(&source)
            .map_err(anyhow::Error::from)
            .and_then(|content| self.stage_draft(&content));
        match result {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: Some(source),
                    security: self.default_security(),
                    attach: None,
                    hidden_head,
                });
            }
            Err(err) => self.status = Some(format!("cannot recall: {err:#}")),
        }
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
                    self.recall_file(path);
                }
            }
            _ => {}
        }
    }

    // ---- shared helpers ----

    /// Queue a macro's keys in front of whatever is already queued (a
    /// macro fired mid-replay expands in place, like mutt's input
    /// stack). The cap breaks self-referencing macros.
    fn replay(&mut self, seq: Vec<KeyEvent>) {
        if self.pending_keys.len() + seq.len() > 1000 {
            self.pending_keys.clear();
            self.status = Some("macro expansion too deep — stopped".into());
            return;
        }
        for (i, key) in seq.into_iter().enumerate() {
            self.pending_keys.insert(i, key);
        }
    }

    fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.sel = index.min(self.visible.len() - 1);
    }

    /// Apply `f` to every tagged message, marking them dirty.
    fn each_tagged(&mut self, f: impl Fn(&mut Msg)) {
        let mut count = 0usize;
        for m in self.msgs.iter_mut().filter(|m| m.env.tagged) {
            f(m);
            m.dirty = true;
            count += 1;
        }
        self.status = Some(format!("applied to {count} tagged message(s)"));
    }

    fn prompt_copy(&mut self, delete: bool) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        let buf = self.config.mail.save.clone().unwrap_or_default();
        self.prompt = Some(Prompt::Line {
            label: if delete {
                "Save to mailbox: "
            } else {
                "Copy to mailbox: "
            }
            .into(),
            buf,
            kind: if delete {
                LineKind::SaveMsg
            } else {
                LineKind::CopyMsg
            },
        });
    }

    /// The selected message's raw bytes, completing a header-only IMAP
    /// cache file first. Failures land in the status line.
    fn full_message_bytes(&mut self) -> Option<Vec<u8>> {
        let &i = self.visible.get(self.sel)?;
        let path = self.msgs[i].env.file.path.clone();
        if let Some(remote) = &mut self.remote
            && remote::is_partial(&path)
            && let Err(err) = remote.fetch_body(&path)
        {
            self.status = Some(format!("cannot fetch message: {err:#}"));
            return None;
        }
        match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                self.status = Some(format!("cannot read message: {err}"));
                None
            }
        }
    }

    /// Copy the message to a mailbox (local maildir path or a folder
    /// of the open IMAP account); with `delete` the original is marked
    /// deleted afterwards — mutt's s versus C.
    fn copy_message(&mut self, input: &str, delete: bool) {
        if input.is_empty() {
            self.status = Some("no mailbox given".into());
            return;
        }
        let Some(&i) = self.visible.get(self.sel) else {
            return;
        };
        let flags = self.msgs[i].env.file.flags;
        let Some(bytes) = self.full_message_bytes() else {
            return;
        };
        let target = match remote::parse_spec(input) {
            Some((account, folder)) => match &mut self.remote {
                Some(remote) if remote.account.name == account => {
                    match remote.append_to(folder, flags, &bytes) {
                        Ok(folder) => format!("imap:{account}/{folder}"),
                        Err(err) => {
                            self.status = Some(format!("cannot save: {err:#}"));
                            return;
                        }
                    }
                }
                _ => {
                    self.status = Some("can only save to a folder of the open account".into());
                    return;
                }
            },
            None => {
                let dir = expand_tilde(input);
                let result = maildir::create(&dir)
                    .and_then(|()| maildir::deliver(&dir, &bytes, flags))
                    .map(|_| dir.display().to_string());
                match result {
                    Ok(shown) => shown,
                    Err(err) => {
                        self.status = Some(format!("cannot save: {err:#}"));
                        return;
                    }
                }
            }
        };
        if delete {
            if let Some(m) = self.cur_mut() {
                m.env.file.flags.deleted = true;
                m.dirty = true;
            }
            self.status = Some(format!("saved to {target} (original marked deleted)"));
        } else {
            self.status = Some(format!("copied to {target}"));
        }
    }

    /// Offer the selected message's sender for the alias file, with
    /// the address's local part as the suggested nick.
    fn prompt_create_alias(&mut self) {
        let Some(&i) = self.visible.get(self.sel) else {
            return;
        };
        let path = self.msgs[i].env.file.path.clone();
        let Some(from) = message::first_header(&path, "From") else {
            self.status = Some("the message has no From header".into());
            return;
        };
        let nick = compose::bare_address(&from)
            .and_then(|a| a.split('@').next().map(|l| l.to_lowercase()))
            .unwrap_or_default();
        self.alias_addr = Some(from.trim().to_string());
        self.prompt = Some(Prompt::Line {
            label: "Alias as (nick): ".into(),
            buf: nick,
            kind: LineKind::AliasNick,
        });
    }

    fn create_alias(&mut self, nick: &str) {
        let Some(addr) = self.alias_addr.take() else {
            return;
        };
        if nick.is_empty() || nick.contains(char::is_whitespace) {
            self.status = Some("the alias nick must be one word".into());
            return;
        }
        match alias::append(nick, &addr) {
            Ok(_) => self.status = Some(format!("added: alias {nick} {addr}")),
            Err(err) => self.status = Some(format!("cannot save the alias: {err:#}")),
        }
    }

    fn prompt_pipe(&mut self) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::Line {
            label: "Pipe to command: ".into(),
            buf: String::new(),
            kind: LineKind::Pipe,
        });
    }

    /// Pipe the raw message to a shell command, like mutt's |.
    fn pipe_message(&mut self, command: &str) {
        if command.is_empty() {
            self.status = Some("no command given".into());
            return;
        }
        let Some(bytes) = self.full_message_bytes() else {
            return;
        };
        match pipe_to(command, &bytes) {
            Ok(()) => self.status = Some(format!("piped to {command}")),
            Err(err) => self.status = Some(format!("pipe failed: {err:#}")),
        }
    }

    fn prompt_bounce(&mut self) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::Line {
            label: "Bounce message to: ".into(),
            buf: String::new(),
            kind: LineKind::BounceTo,
        });
    }

    fn bounce_to_submitted(&mut self, input: &str) {
        let to = alias::expand(input, &alias::load_default());
        if to.trim().is_empty() {
            self.status = Some("no recipients — bounce cancelled".into());
            return;
        }
        self.bounce_to = Some(to.clone());
        self.prompt = Some(Prompt::Key {
            label: format!("Bounce message to {to}? (y/n): "),
            kind: KeyKind::Bounce,
        });
    }

    /// Resend the message as-is to new recipients: Resent-* headers on
    /// top, the rest untouched.
    fn bounce_current(&mut self, to: &str) {
        let Some(bytes) = self.full_message_bytes() else {
            return;
        };
        let rcpts = compose::addresses(to);
        if rcpts.is_empty() {
            self.status = Some(format!("cannot parse the addresses in {to:?}"));
            return;
        }
        let host = maildir::hostname();
        let from = self
            .current_identity(&rcpts)
            .from_line()
            .unwrap_or_else(|| default_from(&host));
        let text = compose::bounce_text(
            &bytes,
            &from,
            to,
            &compose::rfc2822_now(),
            &compose::make_message_id(&host),
        );
        let envelope_from = compose::bare_address(&from).unwrap_or_else(|| from.clone());
        let result = match self.smtp_account() {
            Some(account) => account_password(&account).and_then(|password| {
                smtp::send(&account, &password, &envelope_from, &rcpts, text.as_bytes())
            }),
            None => run_sendmail(
                text.as_bytes(),
                self.config.mail.sendmail.as_deref(),
                Some(&rcpts),
            ),
        };
        match result {
            Ok(()) => self.status = Some(format!("message bounced to {to}")),
            Err(err) => self.status = Some(format!("bounce failed: {err:#}")),
        }
    }

    /// Open a copy of the message as a new draft (mutt's resend): its
    /// To/Cc/Subject and body prefill the editor, then the normal send
    /// prompt takes over.
    fn resend_current(&mut self) {
        // Completes a partial IMAP file so the body is really there.
        if self.full_message_bytes().is_none() {
            return;
        }
        let Some(base) = self.compose_base() else {
            self.status = Some("no message selected".into());
            return;
        };
        let body = message::body_text(&base.path).unwrap_or_default();
        let text = compose::draft_text(
            &compose::DraftHeaders {
                from: self.compose_from(None, &base.orig_to),
                to: base.orig_to.clone(),
                cc: (!base.orig_cc.trim().is_empty()).then(|| base.orig_cc.clone()),
                subject: base.subject.clone(),
                in_reply_to: None,
                references: None,
            },
            &body,
        );
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: None,
                    security: self.default_security(),
                    attach: None,
                    hidden_head,
                });
            }
            Err(err) => self.status = Some(format!("cannot write draft: {err:#}")),
        }
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
            // The body is here now: %l can show its line count.
            if let Some(m) = self.msgs.iter_mut().find(|m| m.env.file.path == path)
                && let Ok(raw) = std::fs::read(path)
            {
                m.env.lines = Some(message::body_lines(&raw));
            }
        }
        let mut view = message::load_with(path, &self.config.filters)?;
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
                Some((_, patterns)) => pattern::matches(patterns, &self.msgs[i].env, &self.me),
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
            let newest = self.config.index.sort_aux.as_deref() == Some("last-date-sent");
            let items = {
                let envs: Vec<&Envelope> = self.msgs.iter().map(|m| &m.env).collect();
                thread::thread_by(&envs, newest)
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
            if pattern::matches(&patterns, &self.msgs[self.visible[vi]].env, &self.me) {
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
    fn prompt_purge(&mut self, quit: bool) {
        self.prompt = Some(Prompt::Key {
            label: format!("Purge {} deleted message(s)? (y/n): ", self.deleted_count()),
            kind: KeyKind::Purge { quit },
        });
    }

    /// Copy every deleted message into the trash mailbox: UID COPY on
    /// the server for IMAP mailboxes, maildir delivery otherwise (the
    /// deleted mark is dropped on the copy).
    fn trash_deleted(&mut self, trash: &str) -> Result<()> {
        let deleted: Vec<PathBuf> = self
            .msgs
            .iter()
            .filter(|m| m.env.file.flags.deleted)
            .map(|m| m.env.file.path.clone())
            .collect();
        match (remote::parse_spec(trash), &mut self.remote) {
            (Some((account, folder)), Some(remote)) if remote.account.name == account => {
                remote.copy_to_folder(&deleted, folder)
            }
            (Some(_), _) => anyhow::bail!("trash must be a folder of the open account"),
            (None, Some(_)) => {
                anyhow::bail!("an IMAP mailbox needs an imap:account/folder trash")
            }
            (None, None) => {
                let dir = expand_tilde(trash);
                maildir::create(&dir)?;
                for m in self.msgs.iter().filter(|m| m.env.file.flags.deleted) {
                    let bytes = std::fs::read(&m.env.file.path)?;
                    let mut flags = m.env.file.flags;
                    flags.deleted = false;
                    maildir::deliver(&dir, &bytes, flags)?;
                }
                Ok(())
            }
        }
    }

    /// `purge` expunges deleted messages; without it they stay marked
    /// and only flag changes are written.
    fn sync(&mut self, purge: bool) {
        // $trash: purged messages move there first; a failed copy
        // aborts the purge. Purging inside the trash deletes for real.
        if purge
            && self.deleted_count() > 0
            && let Some(trash) = self.config.mail.trash.clone()
            && trash != self.title
            && expand_tilde(&trash) != self.dir
            && let Err(err) = self.trash_deleted(&trash)
        {
            self.status = Some(format!("trash failed: {err:#} — nothing purged"));
            return;
        }
        if let Some(remote) = &mut self.remote {
            let mut deletes: Vec<PathBuf> = Vec::new();
            let mut flag_pushes: Vec<(PathBuf, maildir::Flags)> = Vec::new();
            for m in &self.msgs {
                if m.env.file.flags.deleted {
                    if purge {
                        deletes.push(m.env.file.path.clone());
                    }
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
        if let Some(mbox) = &mut self.mbox {
            // The wanted end state per message id; untouched messages
            // keep whatever the file already says.
            let mut state: HashMap<String, Option<(maildir::Flags, bool)>> = HashMap::new();
            for m in &self.msgs {
                let Some(id) = mbox::id_of(&m.env.file.path) else {
                    continue;
                };
                if m.env.file.flags.deleted && purge {
                    state.insert(id, None);
                } else if m.pending() {
                    let is_new = m.env.file.is_new && !m.dirty;
                    state.insert(id, Some((m.env.file.flags, is_new)));
                }
            }
            if let Err(err) = mbox.write_back(&state) {
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
                if !purge {
                    return true; // stays marked for a later purge
                }
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

/// "[reverse-]date|from|subject|size|threads" from the config.
fn parse_sort(spec: &str) -> Option<(SortKey, bool)> {
    let (rev, name) = match spec.strip_prefix("reverse-") {
        Some(rest) => (true, rest),
        None => (false, spec),
    };
    let key = match name {
        "date" | "date-sent" | "date-received" => SortKey::Date,
        "from" => SortKey::From,
        "subject" => SortKey::Subject,
        "size" => SortKey::Size,
        "threads" => SortKey::Threads,
        _ => return None,
    };
    // Thread sort has no reverse variant.
    Some((key, rev && key != SortKey::Threads))
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

/// Run the account's password command once per session. OAuth tokens
/// expire, so those are fetched fresh for every connection instead.
fn account_password(account: &Account) -> Result<String> {
    if !matches!(account.auth_kind()?, rmut_core::config::AuthKind::Password) {
        return account.secret();
    }
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

/// With `rcpts` the addresses go on the command line (a bounce keeps
/// its Resent-To out of -t's reach); otherwise -t reads To/Cc/Bcc.
fn run_sendmail(bytes: &[u8], configured: Option<&str>, rcpts: Option<&[String]>) -> Result<()> {
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
    match rcpts {
        Some(rcpts) => {
            args.push("-oi".into());
            args.extend(rcpts.iter().cloned());
        }
        None => args.extend(["-t".into(), "-oi".into()]),
    }
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
