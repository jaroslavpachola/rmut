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
use rmut_core::{alias, command, compose, hdrcache, maildir, mbox, message, pgp, smtp, thread};

use crate::keymap::{IndexAction, Keymap, PagerAction, parse_key, parse_sequence};
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
    /// The pager's text search (`/` inside a message).
    PagerSearch,
    /// Pattern-wide operations (D/U/T/ctrl+t): every match gets the op.
    DeletePattern,
    UndeletePattern,
    TagPattern,
    UntagPattern,
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
    /// Compose menu header edits (t/c/b/s), back to the menu after.
    EditTo,
    EditCc,
    EditBcc,
    EditSubject,
    /// Compose menu attachment edits (d / ctrl+t) and the Fcc (f).
    EditDesc,
    EditType,
    EditFcc,
}

impl LineKind {
    /// History bucket, mutt-style: one shared list per input class.
    fn history_bucket(self) -> &'static str {
        match self {
            LineKind::Limit
            | LineKind::Search
            | LineKind::PagerSearch
            | LineKind::DeletePattern
            | LineKind::UndeletePattern
            | LineKind::TagPattern
            | LineKind::UntagPattern => "pattern",
            LineKind::ComposeTo
            | LineKind::BounceTo
            | LineKind::EditTo
            | LineKind::EditCc
            | LineKind::EditBcc => "address",
            LineKind::ChangeDir
            | LineKind::SaveMsg
            | LineKind::CopyMsg
            | LineKind::BrowseDir
            | LineKind::CreateDir
            | LineKind::EditFcc => "mailbox",
            LineKind::SavePart | LineKind::AttachFile => "file",
            LineKind::Pipe | LineKind::PipePart => "command",
            LineKind::Notmuch => "notmuch",
            LineKind::EnterCommand => "command",
            LineKind::ComposeSubject
            | LineKind::EditSubject
            | LineKind::AliasNick
            | LineKind::Query
            | LineKind::EditDesc
            | LineKind::EditType => "other",
        }
    }
}

#[derive(Clone, Copy)]
pub enum KeyKind {
    Sort,
    Security,
    Recall,
    Print,
    /// Confirm printing the selected attachment part.
    PrintPart,
    /// Confirm sending the message in `App::bounce_to`.
    Bounce,
    /// Confirm expunging deleted messages; `quit` leaves afterwards.
    Purge {
        quit: bool,
    },
    /// mutt's $reply_to (ask-yes): reply to the Reply-To address?
    ReplyTo,
    /// mutt's $abort_nosubject (ask-yes): no subject, abort?
    NoSubject,
    /// mime_forward = "ask": forward the original as an attachment?
    ForwardAttach,
    /// mutt's $include (ask-yes): quote the original in the reply?
    IncludeReply,
    /// Compose menu q, like mutt: postpone (yes) or discard (no)?
    PostponeAsk,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    New,
    Reply,
    GroupReply,
    /// mutt's list-reply: the mailing list is the only recipient.
    ListReply,
    Forward,
}

/// Snapshot of the message being replied to / forwarded, taken when the
/// compose flow starts.
pub struct ComposeBase {
    path: PathBuf,
    reply_to: String,
    /// The From header as written, the reply target when the
    /// Reply-To question is answered no.
    from_hdr: String,
    /// The message has a Reply-To differing from From, worth the
    /// mutt $reply_to (ask-yes) question.
    has_reply_to: bool,
    orig_to: String,
    orig_cc: String,
    /// List-Post's posting address, when the list published one.
    list_post: Option<String>,
    /// Mail-Followup-To as the sender wrote it, honored by a group
    /// reply (mutt does the same).
    followup_to: String,
    /// Bare author address, for the forward subject's %a.
    from_addr: String,
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
    /// Parked here while the include-original question is up.
    subject: Option<String>,
    /// mime_forward = "ask": the question's answer, once given.
    fwd_attach: Option<bool>,
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
    /// Fcc chosen in the compose menu (`f`): None = the default sent
    /// copy, Some("") = keep no copy, Some(path) = that maildir.
    pub fcc: Option<String>,
}

/// First value of a (single-line) header in a draft head block.
fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
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
    /// Set when `status` is an error: the bottom line renders it in
    /// the error color and a bell rang (mutt's color error + $beep).
    pub status_error: bool,
    pub sort: SortKey,
    pub sort_rev: bool,
    pub limit: Option<(String, Vec<Pattern>)>,
    pub last_search: Option<Vec<Pattern>>,
    /// The pager's text search, kept across messages so n/N carry
    /// over; the pager highlights its hits.
    pub(crate) pager_search: Option<pattern::Matcher>,
    /// Its raw text, prefilling the next Search for: prompt (mutt
    /// prefills from its searchbuf the same way).
    pub(crate) pager_search_text: String,
    /// Compiled $quote_regexp classifying quoted body lines.
    pub(crate) quote_re: regex_lite::Regex,
    /// ignore/unignore/hdr_order for the pager's brief header view.
    head_rules: message::HeaderRules,
    /// Compiled [[color_body]] rules: regex + style, in config order.
    pub(crate) body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
    /// Width and content rows from the last key dispatch, for actions
    /// (prompt submissions) that arrive without a size at hand.
    view_size: (usize, usize),
    /// Per-message thread depth/root (aligned with `msgs`; identity when
    /// not sorted by threads).
    pub thread_depth: Vec<usize>,
    pub thread_root: Vec<usize>,
    /// Paths of collapsed thread roots.
    pub collapsed: HashSet<PathBuf>,
    pub config: Config,
    pub theme: Theme,
    pub keymap: Keymap,
    /// My own addresses (identity, accounts, $EMAIL), lowercase; the
    /// exact half of `me()`, which adds `alternates` on top.
    pub my_addresses: Vec<String>,
    /// Compiled `mail.lists` + `mail.subscribed`, for `~l`, the `L`
    /// list-reply target, and Mail-Followup-To.
    pub lists: Vec<pattern::Matcher>,
    /// The subscribed half on its own: only it drops my address from
    /// a Mail-Followup-To.
    pub subscribed: Vec<pattern::Matcher>,
    /// Compiled `mail.alternates`: my other addresses, joined to `me`
    /// wherever rmut asks whether an address is mine.
    pub alternates: Vec<pattern::Matcher>,
    compose_setup: Option<ComposeSetup>,
    compose: Option<Compose>,
    pending_editor: Option<Compose>,
    /// Message whose raw bytes go through $EDITOR next loop tick
    /// (mutt's edit function).
    pending_raw_edit: Option<PathBuf>,
    /// Recipients waiting for the bounce confirmation.
    bounce_to: Option<String>,
    /// The sender address waiting for a create-alias nick.
    alias_addr: Option<String>,
    /// Attach-line index waiting for a d / ctrl+t compose-menu edit.
    attach_edit: Option<usize>,
    /// Background IDLE watcher for the open IMAP folder.
    idle: Option<remote::IdleWatch>,
    /// Background header mirror for the tail of a huge IMAP folder.
    backfill: Option<remote::Backfill>,
    /// Server-side `~b` results: term → matching UIDs, filled when a
    /// limit/search pattern with body terms is submitted on IMAP.
    body_hits: HashMap<String, HashSet<u32>>,
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
    /// New-mail counts of the other configured mailboxes at the last
    /// poll, to notice growth (mutt's `mailboxes` awareness).
    mailbox_new: HashMap<String, usize>,
    /// `rmut -R`: nothing is ever written, not even read marks.
    pub read_only: bool,
    /// `;` was pressed: the next flag operation applies to tagged messages.
    tag_next: bool,
    /// mtimes of new/ and cur/ used for new-mail detection.
    dir_mtimes: (Option<SystemTime>, Option<SystemTime>),
    quit: bool,
}

impl App {
    /// mutt's $wrap: the effective text width inside `width` columns
    /// (positive = wrap there, negative = a right margin).
    pub(crate) fn pager_wrap(&self, width: usize) -> usize {
        match self.config.pager.wrap {
            Some(n) if n > 0 => (n as usize).min(width),
            Some(n) if n < 0 => width.saturating_sub(n.unsigned_abs() as usize).max(20),
            _ => width,
        }
    }
}

/// mutt's $quote_regexp default.
pub(crate) fn default_quote_re() -> regex_lite::Regex {
    regex_lite::Regex::new(r"^([ \t]*[|>:}#])+").expect("default quote_regexp compiles")
}

fn dir_mtimes(dir: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let mtime = |p: PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    (mtime(dir.join("new")), mtime(dir.join("cur")))
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
    lists: Vec<pattern::Matcher>,
    subscribed: Vec<pattern::Matcher>,
    alternates: Vec<pattern::Matcher>,
    index_rules: Vec<(Vec<Pattern>, ratatui::style::Style)>,
    body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
    quote_re: regex_lite::Regex,
    head_rules: message::HeaderRules,
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
        let quote_re = match &config.pager.quote_regexp {
            Some(spec) => match regex_lite::Regex::new(spec) {
                Ok(re) => re,
                Err(err) => {
                    warnings.push(format!("bad quote_regexp {spec:?}: {err}"));
                    default_quote_re()
                }
            },
            None => default_quote_re(),
        };
        // [pager] ignore/unignore/hdr_order override the classic
        // five-header view field by field; entries are lowercased and
        // hdr_order accepts mutt's trailing colons.
        let mut head_rules = message::HeaderRules::default();
        let clean = |list: &Vec<String>| {
            list.iter()
                .map(|n| n.trim_end_matches(':').to_lowercase())
                .collect::<Vec<_>>()
        };
        if let Some(list) = &config.pager.ignore {
            head_rules.ignore = clean(list);
        }
        if let Some(list) = &config.pager.unignore {
            head_rules.unignore = clean(list);
        }
        if let Some(list) = &config.pager.hdr_order {
            head_rules.order = clean(list);
        }
        (
            Derived {
                theme,
                keymap,
                lists: config.list_matchers(),
                subscribed: config.subscribed_matchers(),
                alternates: config.alternate_matchers(),
                index_rules,
                body_rules,
                quote_re,
                head_rules,
            },
            warnings,
        )
    }
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
        // Like mutt: start on the first new message, else the last.
        let sel = msgs
            .iter()
            .position(|m| m.env.file.is_new)
            .unwrap_or(visible.len().saturating_sub(1));
        let (derived, mut warnings) = Derived::from_config(&config);
        let Derived {
            theme,
            keymap,
            lists,
            subscribed,
            alternates,
            index_rules,
            body_rules,
            quote_re,
            head_rules,
        } = derived;
        if skipped > 0 {
            warnings.push(format!("{skipped} unreadable message(s) skipped"));
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
            status_error: false,
            sort: SortKey::Date,
            sort_rev: false,
            limit: None,
            last_search: None,
            pager_search: None,
            pager_search_text: String::new(),
            quote_re,
            head_rules,
            body_rules,
            view_size: (80, 24),
            thread_depth: vec![0; count],
            thread_root: (0..count).collect(),
            collapsed: HashSet::new(),
            dir_mtimes: dir_mtimes(dir),
            config,
            theme,
            keymap,
            my_addresses: me,
            lists,
            subscribed,
            alternates,
            compose_setup: None,
            compose: None,
            pending_editor: None,
            pending_raw_edit: None,
            bounce_to: None,
            alias_addr: None,
            attach_edit: None,
            idle: None,
            backfill: None,
            body_hits: HashMap::new(),
            complete: None,
            pending_keys: std::collections::VecDeque::new(),
            history: HashMap::new(),
            index_rules,
            sidebar: Vec::new(),
            sidebar_sel: 0,
            sidebar_open: None,
            sidebar_visible: config_sidebar_visible,
            mailbox_new: HashMap::new(),
            read_only: false,
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
        // A huge IMAP folder mirrors its tail in the background.
        app.maybe_backfill();
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
                self.status_error = false;
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
            if let Some(path) = self.pending_raw_edit.take() {
                self.edit_raw(&mut terminal, path);
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent, width: usize, height: usize) {
        // Rows available for content: total minus help line and status line.
        let page = height.saturating_sub(2).max(1);
        self.view_size = (width, page);
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

    fn check_new_mail(&mut self) {
        let backfilling = self.backfill.as_ref().is_some_and(|b| !b.done());
        if let Some(remote) = &mut self.remote {
            // While the backfill streams headers in, skip the server
            // check, since a full reconcile would refetch its tail
            // synchronously; the rescan below integrates the files.
            if backfilling {
            } else if let Err(err) = remote.check_new() {
                self.error_status(format!("imap: {err:#}"));
            }
        }
        self.maybe_backfill();
        if let Some(mbox) = &mut self.mbox {
            // Re-mirror when the file changed; same rescan pickup.
            if let Err(err) = mbox.refresh() {
                self.error_status(format!("mbox: {err:#}"));
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

    /// Spawn (or finish) the background mirror for a huge folder's
    /// leftover headers; the poll rescan integrates them as they land.
    fn maybe_backfill(&mut self) {
        if self.backfill.as_ref().is_some_and(|b| !b.done()) {
            return;
        }
        self.backfill = None;
        let Some(remote) = &mut self.remote else {
            return;
        };
        if remote.pending_backfill.is_empty() {
            return;
        }
        let uids = mem::take(&mut remote.pending_backfill);
        let Ok(password) = account_password(&remote.account) else {
            return;
        };
        let count = uids.len();
        self.backfill = Some(remote::backfill(
            &remote.account,
            &remote.mailbox,
            &password,
            remote.cache.clone(),
            uids,
        ));
        self.status = Some(format!(
            "loading {count} older message(s) in the background"
        ));
    }

    /// Server-aware pattern match: `~b` terms resolved by UID SEARCH
    /// (when `resolve_body_terms` filled the sets) instead of local
    /// body reads, and the message's place in the list carried along
    /// for `~m` and `~=`.
    fn env_matches_at(&self, patterns: &[Pattern], env: &Envelope, pos: pattern::Position) -> bool {
        pattern::matches_in(
            patterns,
            env,
            self.scope(pos),
            Some(&|env: &Envelope, m: &pattern::Matcher| {
                let set = self.body_hits.get(m.raw())?;
                let uid = remote::uid_of(&env.file.path)?;
                Some(set.contains(&uid))
            }),
        )
    }

    /// What the pattern engine needs beyond one message: my addresses,
    /// the configured mailing lists, and the place in the list.
    pub(crate) fn scope(&self, position: pattern::Position) -> pattern::Scope<'_> {
        pattern::Scope {
            me: self.me(),
            lists: &self.lists,
            position,
        }
    }

    /// Which addresses are mine: the identity ones plus `alternates`.
    pub(crate) fn me(&self) -> pattern::Me<'_> {
        pattern::Me::new(&self.my_addresses, &self.alternates)
    }

    /// `~m` numbering and `~=` duplicate flags for every message, as
    /// the index stands right now: numbers are the ones on screen, so
    /// a range means what the user can actually see, and messages
    /// hidden by the current limit carry number 0 (never in range).
    fn positions(&self) -> Vec<pattern::Position> {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for m in &self.msgs {
            if let Some(id) = m.env.msg_id.as_deref() {
                *seen.entry(id).or_default() += 1;
            }
        }
        let mut numbers = vec![0usize; self.msgs.len()];
        for (n, &mi) in self.visible.iter().enumerate() {
            if let Some(slot) = numbers.get_mut(mi) {
                *slot = n + 1;
            }
        }
        let current = self
            .visible
            .get(self.sel)
            .and_then(|&mi| numbers.get(mi).copied())
            .unwrap_or(0);
        let last = self.visible.len();
        (0..self.msgs.len())
            .map(|i| pattern::Position {
                number: numbers[i],
                current,
                last,
                duplicate: self.msgs[i]
                    .env
                    .msg_id
                    .as_deref()
                    .is_some_and(|id| seen.get(id).copied().unwrap_or(0) > 1),
            })
            .collect()
    }

    /// On IMAP, ask the server about the pattern's `~b` terms up
    /// front. Only plain substrings go (regex or non-ASCII terms stay
    /// local; a server search is a literal match); a failed search
    /// just falls back to reading bodies locally.
    fn resolve_body_terms(&mut self, patterns: &[Pattern]) {
        let Some(remote) = &mut self.remote else {
            return;
        };
        for term in pattern::body_terms(patterns) {
            let simple = !term
                .chars()
                .any(|c| r".*+?[](){}|^$\".contains(c) || !c.is_ascii());
            if !simple || self.body_hits.contains_key(&term) {
                continue;
            }
            if let Ok(uids) = remote.search_body(&term) {
                self.body_hits.insert(term, uids.into_iter().collect());
            }
        }
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
            if let Some(p) = prev.filter(|&p| count > p) {
                self.run_new_mail_command(&spec, count - p);
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
            let title = self.title.clone();
            self.run_new_mail_command(&title, arrived);
        }
    }

    /// neomutt's new_mail_command: fire-and-forget shell hook on
    /// arrivals; %f = the mailbox, %n = how many.
    fn run_new_mail_command(&self, mailbox: &str, count: usize) {
        let Some(cmd) = &self.config.mail.new_mail_command else {
            return;
        };
        let cmd = cmd
            .replace("%f", &format!("'{}'", mailbox.replace('\'', r"'\''")))
            .replace("%n", &count.to_string());
        let _ = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
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
                let kind = *kind;
                self.prompt = None;
                self.run_key_prompt(kind, key.code);
            }
            Some(Prompt::Line { buf, cursor, .. }) => match key.code {
                KeyCode::Esc => {
                    self.prompt = None;
                    self.compose_setup = None;
                    self.bounce_to = None;
                    self.alias_addr = None;
                    // Escaping a sub-prompt of the send flow (attach
                    // file) returns to the compose menu.
                    if self.compose.is_some() {
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
            }
            KeyKind::PostponeAsk => match code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.mode = Mode::Index;
                    self.postpone_draft();
                }
                KeyCode::Char('n') => {
                    self.mode = Mode::Index;
                    if let Some(c) = self.compose.take() {
                        let _ = std::fs::remove_file(&c.path);
                        self.status = Some("message discarded".into());
                    }
                }
                _ => {} // back to the menu
            },
            KeyKind::Recall => match code {
                KeyCode::Char('r') => self.recall_postponed(),
                KeyCode::Char('n') => self.continue_setup(ComposeKind::New, None),
                _ => {}
            },
            KeyKind::ReplyTo => match code {
                KeyCode::Char('y') | KeyCode::Enter => self.open_to_prompt(true),
                KeyCode::Char('n') => self.open_to_prompt(false),
                _ => {
                    self.compose_setup = None;
                    self.status = Some("reply cancelled".into());
                }
            },
            KeyKind::NoSubject => match code {
                // ask-yes: Enter aborts, like mutt.
                KeyCode::Char('n') => self.subject_ready(String::new()),
                _ => {
                    self.compose_setup = None;
                    self.error_status("aborted (no subject)");
                }
            },
            KeyKind::IncludeReply => {
                let subject = self
                    .compose_setup
                    .as_mut()
                    .and_then(|s| s.subject.take())
                    .unwrap_or_default();
                match code {
                    KeyCode::Char('n') => self.finish_compose_setup(&subject, false),
                    KeyCode::Char('y') | KeyCode::Enter => {
                        self.finish_compose_setup(&subject, true)
                    }
                    _ => {
                        self.compose_setup = None;
                        self.status = Some("reply cancelled".into());
                    }
                }
            }
            KeyKind::ForwardAttach => {
                let subject = self
                    .compose_setup
                    .as_mut()
                    .and_then(|s| s.subject.take())
                    .unwrap_or_default();
                match code {
                    KeyCode::Char('n') => {
                        if let Some(s) = &mut self.compose_setup {
                            s.fwd_attach = Some(false);
                        }
                        self.finish_compose_setup(&subject, true);
                    }
                    // ask-yes: Enter takes the attachment.
                    KeyCode::Char('y') | KeyCode::Enter => {
                        if let Some(s) = &mut self.compose_setup {
                            s.fwd_attach = Some(true);
                        }
                        self.finish_compose_setup(&subject, true);
                    }
                    _ => {
                        self.compose_setup = None;
                        self.status = Some("forward cancelled".into());
                    }
                }
            }
            KeyKind::Print => {
                if code == KeyCode::Char('y') {
                    self.print_current();
                }
            }
            KeyKind::PrintPart => {
                if code == KeyCode::Char('y') {
                    self.print_part();
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
    /// candidates, and with nothing typed at the c prompt, open the
    /// folder browser instead. Repeated Tab cycles the candidates.
    fn tab_complete(&mut self) {
        let (buf_now, kind) = match &self.prompt {
            Some(Prompt::Line { buf, kind, .. }) => (buf.clone(), *kind),
            _ => return,
        };
        let is_addr = matches!(
            kind,
            LineKind::ComposeTo
                | LineKind::BounceTo
                | LineKind::EditTo
                | LineKind::EditCc
                | LineKind::EditBcc
        );
        let is_mbox = matches!(
            kind,
            LineKind::ChangeDir | LineKind::SaveMsg | LineKind::CopyMsg | LineKind::BrowseDir
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
            self.error_status("nothing to complete");
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
                    self.error_status(format!("cannot list folders: {err:#}"));
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
            0 => self.error_status(format!("no matches for {word}")),
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
                        Ok(patterns) => {
                            self.resolve_body_terms(&patterns);
                            self.limit = Some((input.to_string(), patterns));
                        }
                        Err(err) => {
                            self.error_status(format!("bad pattern: {err}"));
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
                        Ok(patterns) => {
                            self.resolve_body_terms(&patterns);
                            self.last_search = Some(patterns);
                        }
                        Err(err) => {
                            self.error_status(format!("bad pattern: {err}"));
                            return;
                        }
                    }
                }
                self.search_next();
            }
            LineKind::PagerSearch => {
                if !input.is_empty() {
                    self.pager_search = Some(pattern::Matcher::new(input));
                    self.pager_search_text = input.to_string();
                }
                if self.pager_search.is_some() {
                    self.pager_search_step(true);
                } else {
                    self.error_status("No search pattern.");
                }
            }
            LineKind::DeletePattern => self.apply_pattern(input, "deleted", |m| {
                if !m.env.file.flags.deleted {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
            }),
            LineKind::UndeletePattern => self.apply_pattern(input, "undeleted", |m| {
                if m.env.file.flags.deleted {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                }
            }),
            LineKind::TagPattern => self.apply_pattern(input, "tagged", |m| m.env.tagged = true),
            LineKind::UntagPattern => {
                self.apply_pattern(input, "untagged", |m| m.env.tagged = false)
            }
            LineKind::ChangeDir => self.open_mailbox_spec(input),
            LineKind::BrowseDir => self.browse_dir(input),
            LineKind::CreateDir => self.create_maildir(input),
            LineKind::SavePart => self.save_part(input),
            LineKind::SaveMsg => self.copy_message(input, true),
            LineKind::CopyMsg => self.copy_message(input, false),
            LineKind::Pipe => self.pipe_message(input),
            LineKind::PipePart => self.pipe_part(input),
            LineKind::Query => self.run_query(input),
            LineKind::Notmuch => self.notmuch_search(input),
            LineKind::EnterCommand => self.run_command_line(input),
            LineKind::BounceTo => self.bounce_to_submitted(input),
            LineKind::AttachFile => self.attach_file_submitted(input),
            LineKind::AliasNick => self.create_alias(input),
            LineKind::ComposeTo => self.setup_to_submitted(input),
            LineKind::ComposeSubject => self.subject_submitted(input),
            LineKind::EditTo => {
                self.set_draft_header("To", &alias::expand(input, &alias::load_default()))
            }
            LineKind::EditCc => {
                self.set_draft_header("Cc", &alias::expand(input, &alias::load_default()))
            }
            LineKind::EditBcc => {
                self.set_draft_header("Bcc", &alias::expand(input, &alias::load_default()))
            }
            LineKind::EditSubject => self.set_draft_header("Subject", input),
            LineKind::EditDesc => self.set_attach_field(input, false),
            LineKind::EditType => self.set_attach_field(input, true),
            LineKind::EditFcc => {
                if let Some(c) = &mut self.compose {
                    c.fcc = Some(input.trim().to_string());
                }
            }
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
            IndexAction::Edit => self.start_raw_edit(),
            IndexAction::CreateAlias => self.prompt_create_alias(),
            IndexAction::Query => {
                if self.config.mail.query_command.is_none() {
                    self.error_status("no query_command configured");
                } else {
                    self.prompt = Some(Prompt::line("Query: ", String::new(), LineKind::Query));
                }
            }
            IndexAction::Notmuch => {
                if self.config.mail.notmuch == Some(false) {
                    self.error_status("notmuch is disabled in the config");
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
                self.mark_old_unread();
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
                if self.deny_readonly() {
                } else if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.deleted = true);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::Undelete => {
                if self.deny_readonly() {
                } else if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.deleted = false);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                    // mutt's $resolve (on by default): advance.
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::Flag => {
                if self.deny_readonly() {
                } else if apply_tagged {
                    self.each_tagged(|m| m.env.file.flags.flagged = !m.env.file.flags.flagged);
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.flagged = !m.env.file.flags.flagged;
                    m.dirty = true;
                    self.select(self.sel.saturating_add(1));
                }
            }
            IndexAction::ToggleNew => {
                if self.deny_readonly() {
                } else if apply_tagged {
                    self.each_tagged(|m| {
                        m.env.file.flags.seen = !m.env.file.flags.seen;
                        m.env.file.is_new = false;
                    });
                } else if let Some(m) = self.cur_mut() {
                    m.env.file.flags.seen = !m.env.file.flags.seen;
                    m.env.file.is_new = false;
                    m.dirty = true;
                    self.select(self.sel.saturating_add(1));
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
            IndexAction::ListReply => self.start_list_reply(),
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
                self.prompt = Some(Prompt::line(
                    "Limit (~f/~s/~b/~t/~c/~d/flags, ! | (), empty=all): ",
                    buf,
                    LineKind::Limit,
                ));
            }
            IndexAction::Search => {
                self.prompt = Some(Prompt::line("Search: ", String::new(), LineKind::Search));
            }
            IndexAction::SearchNext => self.search_next(),
            IndexAction::NextNew => self.jump_new(true),
            IndexAction::PrevNew => self.jump_new(false),
            IndexAction::DeletePattern => {
                if !self.deny_readonly() {
                    self.prompt = Some(Prompt::line(
                        "Delete messages matching: ",
                        String::new(),
                        LineKind::DeletePattern,
                    ));
                }
            }
            IndexAction::UndeletePattern => {
                if !self.deny_readonly() {
                    self.prompt = Some(Prompt::line(
                        "Undelete messages matching: ",
                        String::new(),
                        LineKind::UndeletePattern,
                    ));
                }
            }
            IndexAction::TagPattern => {
                self.prompt = Some(Prompt::line(
                    "Tag messages matching: ",
                    String::new(),
                    LineKind::TagPattern,
                ));
            }
            IndexAction::UntagPattern => {
                self.prompt = Some(Prompt::line(
                    "Untag messages matching: ",
                    String::new(),
                    LineKind::UntagPattern,
                ));
            }
            IndexAction::Attachments => self.open_attachments(),
            IndexAction::ChangeMailbox => {
                if self.ready_to_leave() {
                    self.prompt = Some(Prompt::line(
                        "Open mailbox (Tab completes): ",
                        String::new(),
                        LineKind::ChangeDir,
                    ));
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
                    self.status = Some("the sidebar is hidden; B shows it".into());
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
                    self.status = Some("the sidebar is hidden; B shows it".into());
                } else if self.ready_to_leave()
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

    /// The next/previous visible position from the selection; mutt's
    /// next-/previous-undeleted skips messages flagged for deletion.
    fn step_message(&self, forward: bool, skip_deleted: bool) -> Option<usize> {
        let mut pos = self.sel;
        loop {
            pos = if forward {
                pos + 1
            } else {
                pos.checked_sub(1)?
            };
            let &i = self.visible.get(pos)?;
            if !skip_deleted || !self.msgs[i].env.file.flags.deleted {
                return Some(pos);
            }
        }
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
                    self.error_status("Not available in this menu.");
                    return;
                }
                match self.step_message(true, action == PagerAction::NextUndeleted) {
                    Some(pos) => {
                        self.sel = pos;
                        self.open_selected();
                    }
                    None => self.error_status("last message"),
                }
                return;
            }
            PagerAction::PrevMsg | PagerAction::PrevUndeleted => {
                if self.part_pager() {
                    self.error_status("Not available in this menu.");
                    return;
                }
                match self.step_message(false, action == PagerAction::PrevUndeleted) {
                    Some(pos) => {
                        self.sel = pos;
                        self.open_selected();
                    }
                    None => self.error_status("first message"),
                }
                return;
            }
            PagerAction::Delete => {
                if self.part_pager() {
                    self.error_status("Not available in this menu.");
                    return;
                }
                if self.deny_readonly() {
                    return;
                }
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
                // mutt's $resolve: advance to the next undeleted.
                match self.step_message(true, true) {
                    Some(pos) => {
                        self.sel = pos;
                        self.open_selected();
                    }
                    None => self.mode = Mode::Index,
                }
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
                self.start_list_reply();
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
            PagerAction::Edit => {
                self.mode = Mode::Index;
                self.start_raw_edit();
                return;
            }
            PagerAction::CreateAlias => {
                self.prompt_create_alias();
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
            .saturating_sub(self.config.pager.index_lines as usize)
            .max(1);
        let step = page.saturating_sub(self.config.pager.context).max(1);
        let Mode::Pager(pager) = &mut self.mode else {
            return;
        };
        let lines = crate::ui::pager_line_count(
            &pager.view,
            width,
            pager.full_headers,
            &self.quote_re,
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
            match self.step_message(true, true) {
                Some(pos) => {
                    self.sel = pos;
                    self.open_selected();
                }
                None => self.error_status("last message"),
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
                    &self.quote_re,
                    pager.hide_quoted,
                );
                pager.scroll = pager.scroll.min(lines.saturating_sub(page));
            }
            PagerAction::SkipQuoted => {
                let rows = crate::ui::pager_rows(
                    &pager.view,
                    width,
                    pager.full_headers,
                    &self.quote_re,
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
                    self.error_status("No more quoted text.");
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
            &self.quote_re,
            pager.hide_quoted,
        );
        match search_lines(&lines, &matcher, pager.scroll, forward) {
            Some((hit, wrapped)) => {
                // The hit becomes the top line even near the end (past
                // max_scroll), like mutt; otherwise close-to-the-end
                // hits would be indistinguishable and n would stall.
                pager.scroll = hit;
                if wrapped {
                    self.status = Some(if forward {
                        "Search wrapped to top.".into()
                    } else {
                        "Search wrapped to bottom.".into()
                    });
                }
            }
            None => self.error_status("Not found."),
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
            Ok(_) => self.error_status("message has no parts"),
            Err(err) => self.error_status(format!("cannot list parts: {err:#}")),
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
            match self.config.filters.get(&mimetype).cloned() {
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
                        Err(err) => self.error_status(format!("filter failed: {err:#}")),
                    }
                    return;
                }
                None => {
                    self.error_status(format!("{mimetype} is not text; save it with s"));
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
            Err(err) => self.error_status(format!("cannot decode part: {err:#}")),
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
                self.error_status(format!("cannot decode part: {err:#}"));
                None
            }
        }
    }

    /// `|` on the attachment menu: the decoded part to a command.
    fn pipe_part(&mut self, command: &str) {
        if command.is_empty() {
            self.error_status("no command given");
            return;
        }
        let Some(bytes) = self.selected_part_bytes() else {
            return;
        };
        match pipe_to(command, &bytes) {
            Ok(()) => self.status = Some(format!("piped to {command}")),
            Err(err) => self.error_status(format!("pipe failed: {err:#}")),
        }
    }

    /// `p` on the attachment menu: the decoded part to print_command.
    fn print_part(&mut self) {
        let Some(bytes) = self.selected_part_bytes() else {
            return;
        };
        let command = self
            .config
            .mail
            .print
            .clone()
            .unwrap_or_else(|| "lpr".into());
        match pipe_to(&command, &bytes) {
            Ok(()) => self.status = Some(format!("printed via {command}")),
            Err(err) => self.error_status(format!("print failed: {err:#}")),
        }
    }

    fn save_part(&mut self, input: &str) {
        let (msg_path, index) = match &self.mode {
            Mode::Attach { msg_path, sel, .. } => (msg_path.clone(), *sel),
            _ => return,
        };
        if input.is_empty() {
            self.error_status("no filename given");
            return;
        }
        let target = expand_tilde(input);
        if target.exists() {
            self.error_status(format!("{} exists, not overwriting", target.display()));
            return;
        }
        let result = message::part_bytes(&msg_path, index).and_then(|bytes| {
            std::fs::write(&target, &bytes)?;
            Ok(bytes.len())
        });
        match result {
            Ok(n) => self.status = Some(format!("saved {n} bytes to {}", target.display())),
            Err(err) => self.error_status(format!("save failed: {err:#}")),
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
                    _ => self.dir.parent().unwrap_or(&self.dir).display().to_string(),
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
                self.error_status(format!("cannot browse {}: {err}", root.display()));
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
                .or_else(|| self.dir.parent().map(Path::to_path_buf))
                .unwrap_or_else(|| self.dir.clone())
                .join(given)
        };
        for sub in ["cur", "new", "tmp"] {
            if let Err(err) = std::fs::create_dir_all(path.join(sub)) {
                self.error_status(format!("cannot create {}: {err}", path.display()));
                return;
            }
        }
        self.status = Some(format!("created maildir {}", path.display()));
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
        let Some(command) = self.config.mail.query_command.clone() else {
            return;
        };
        let results = alias::query(&command, input);
        if results.is_empty() {
            self.status = Some("query returned nothing".into());
            return;
        }
        self.mode = Mode::Query { results, sel: 0 };
    }

    /// `X` submitted: `notmuch search --output=files` into a virtual
    /// read-only mailbox: the hits are symlinked into a cache
    /// maildir (the real copies stay where they are), so viewing,
    /// replying, copying, and piping work while flag changes and
    /// deletes stay refused.
    fn notmuch_search(&mut self, query: &str) {
        if query.is_empty() {
            return;
        }
        let out = Command::new("notmuch")
            .args(["search", "--output=files", "--limit=1000", "--", query])
            .output();
        let out = match out {
            Ok(out) if out.status.success() => out,
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                self.error_status(format!("notmuch: {}", err.trim()));
                return;
            }
            Err(err) => {
                self.error_status(format!("notmuch: {err}"));
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let files: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        if files.is_empty() {
            self.error_status("notmuch: no matches");
            return;
        }
        if !self.ready_to_leave() {
            return;
        }
        let dir = remote::cache_base().join("notmuch");
        let build = || -> Result<()> {
            for sub in ["cur", "new", "tmp"] {
                std::fs::create_dir_all(dir.join(sub))?;
            }
            for entry in std::fs::read_dir(dir.join("cur"))?.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
            for (i, file) in files.iter().enumerate() {
                let base = Path::new(file)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| format!("{i}"));
                // A unique prefix avoids collisions across source
                // dirs; the :2, flag suffix stays parseable.
                let _ = std::os::unix::fs::symlink(
                    file,
                    dir.join("cur").join(format!("{i:04}.{base}")),
                );
            }
            Ok(())
        };
        if let Err(err) = build() {
            self.status = Some(format!("notmuch mirror: {err:#}"));
            return;
        }
        let count = files.len();
        self.open_mailbox_spec(&dir.display().to_string());
        if self.dir == dir {
            // The virtual mailbox: never write through the symlinks.
            self.read_only = true;
            self.title = format!("notmuch: {query}");
            self.status = Some(format!("{count} matching message(s)"));
        }
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
                self.error_status(format!("cannot list folders: {err:#}"));
                return;
            }
        };
        if dirs.is_empty() {
            self.error_status("no maildirs found next to this one");
            return;
        }
        let sel = dirs
            .iter()
            .position(|d| d.0 == self.title || expand_tilde(&d.0) == self.dir)
            .unwrap_or(0);
        self.mode = Mode::Folders {
            dirs,
            sel,
            root: None,
        };
    }

    /// mutt's $mark_old (on by default): when leaving the mailbox,
    /// unread new mail ages to old: moved out of new/ without the
    /// seen flag, shown as O and no longer counted as new.
    fn mark_old_unread(&mut self) {
        if self.read_only {
            return;
        }
        for m in &mut self.msgs {
            if m.env.file.is_new
                && !m.env.file.flags.seen
                && !m.env.file.flags.deleted
                && let Ok(path) = maildir::store_flags(&m.env.file)
            {
                m.env.file.path = path;
                m.env.file.is_new = false;
            }
        }
    }

    /// Leaving the mailbox (c, sidebar open, folder browser): flag
    /// changes are written silently like q; only pending deletions
    /// block the switch. True when it is safe to go.
    fn ready_to_leave(&mut self) -> bool {
        if self.deleted_count() > 0 {
            self.error_status("deleted messages pending; sync with $ or undelete first");
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
        self.mark_old_unread();
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
                Err(err) => self.error_status(format!("cannot open {spec}: {err:#}")),
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
            Err(err) => self.error_status(format!("cannot open {spec}: {err:#}")),
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
        let from_hdr = get("From");
        let (reply_to, has_reply_to) = {
            let rt = get("Reply-To");
            if rt.trim().is_empty() {
                (from_hdr.clone(), false)
            } else {
                let differs = rt.trim() != from_hdr.trim();
                (rt, differs)
            }
        };
        Some(ComposeBase {
            path: env.file.path.clone(),
            reply_to,
            from_hdr,
            has_reply_to,
            orig_to: get("To"),
            orig_cc: get("Cc"),
            list_post: compose::list_post_address(&get("List-Post")),
            followup_to: get("Mail-Followup-To"),
            from_addr: compose::addresses(&get("From"))
                .into_iter()
                .next()
                .unwrap_or_default(),
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
                    self.error_status("no message selected");
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
        // mutt's $autoedit (with edit_headers): no prompts, no
        // questions: the defaults land in the draft and the editor
        // opens; everything stays editable there and in the menu.
        if self.config.mail.autoedit && self.edit_headers() {
            let to = match (&kind, &base) {
                (ComposeKind::Reply | ComposeKind::GroupReply, Some(b)) => b.reply_to.clone(),
                (ComposeKind::ListReply, Some(b)) => self.list_target(b).unwrap_or_default(),
                _ => String::new(),
            };
            let subject = match (&kind, &base) {
                (
                    ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply,
                    Some(b),
                ) => compose::reply_subject(&b.subject),
                (ComposeKind::Forward, Some(b)) => {
                    compose::forward_subject(&b.from_addr, &b.subject)
                }
                _ => String::new(),
            };
            self.compose_setup = Some(ComposeSetup {
                kind,
                base,
                to: Some(to),
                subject: None,
                fwd_attach: None,
            });
            self.finish_compose_setup(&subject, true);
            return;
        }
        let ask_reply_to = matches!(kind, ComposeKind::Reply | ComposeKind::GroupReply)
            && base.as_ref().is_some_and(|b| b.has_reply_to);
        self.compose_setup = Some(ComposeSetup {
            kind,
            base,
            to: None,
            subject: None,
            fwd_attach: None,
        });
        if ask_reply_to {
            // mutt's $reply_to = ask-yes.
            let addr = self
                .compose_setup
                .as_ref()
                .and_then(|s| s.base.as_ref())
                .map(|b| b.reply_to.clone())
                .unwrap_or_default();
            self.prompt = Some(Prompt::Key {
                label: format!("Reply to {addr}? (y/n): "),
                kind: KeyKind::ReplyTo,
            });
            return;
        }
        self.open_to_prompt(true);
    }

    /// The To prompt, prefilled for replies with Reply-To (the
    /// question's yes) or the plain From (its no).
    /// Where a list reply goes: the list's own List-Post address when
    /// it published one, else the first To/Cc address that matches a
    /// configured list.
    fn list_target(&self, base: &ComposeBase) -> Option<String> {
        if let Some(addr) = &base.list_post {
            return Some(addr.clone());
        }
        let mut candidates = compose::addresses(&base.orig_to);
        candidates.extend(compose::addresses(&base.orig_cc));
        candidates
            .into_iter()
            .find(|a| self.lists.iter().any(|m| m.is_match(a)))
    }

    /// mutt's $followup_to: mail going to a known list carries a
    /// Mail-Followup-To, so replies land on the list. Being subscribed
    /// leaves my own address out, since the list copy is the one I get.
    fn followup_header(&self, to: &str, cc: Option<&str>, from: &str) -> Option<String> {
        if self.lists.is_empty() {
            return None;
        }
        let mut rcpts = compose::addresses(to);
        rcpts.extend(compose::addresses(cc.unwrap_or_default()));
        if !rcpts
            .iter()
            .any(|a| self.lists.iter().any(|m| m.is_match(a)))
        {
            return None;
        }
        let subscribed = rcpts
            .iter()
            .any(|a| self.subscribed.iter().any(|m| m.is_match(a)));
        let value = compose::followup_to(to, cc.unwrap_or_default(), self.me(), subscribed, from);
        (!value.is_empty()).then_some(value)
    }

    /// `L`: reply to the mailing list. Refuses when the message names
    /// no list rmut knows of, rather than quietly replying to the
    /// author, which is the mistake list-reply exists to prevent.
    fn start_list_reply(&mut self) {
        let Some(base) = self.compose_base() else {
            self.error_status("no message selected");
            return;
        };
        if self.list_target(&base).is_none() {
            self.error_status(if self.lists.is_empty() {
                "no mailing lists configured (mail.lists / mail.subscribed)"
            } else {
                "not a message from a known mailing list"
            });
            return;
        }
        self.continue_setup(ComposeKind::ListReply, Some(base));
    }

    fn open_to_prompt(&mut self, use_reply_to: bool) {
        let Some(setup) = &self.compose_setup else {
            return;
        };
        let to_prefill = match setup.kind {
            ComposeKind::Reply | ComposeKind::GroupReply => setup
                .base
                .as_ref()
                .map(|b| {
                    if use_reply_to {
                        b.reply_to.clone()
                    } else {
                        b.from_hdr.clone()
                    }
                })
                .unwrap_or_default(),
            ComposeKind::ListReply => setup
                .base
                .as_ref()
                .and_then(|b| self.list_target(b))
                .unwrap_or_default(),
            ComposeKind::New | ComposeKind::Forward => String::new(),
        };
        // mutt's $fast_reply: replies take the prefills without the
        // To and Subject prompts (forwards still need a recipient).
        if self.config.mail.fast_reply
            && matches!(
                setup.kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            )
            && setup.base.is_some()
        {
            let subject = setup
                .base
                .as_ref()
                .map(|b| compose::reply_subject(&b.subject))
                .unwrap_or_default();
            if let Some(setup) = &mut self.compose_setup {
                setup.to = Some(to_prefill);
            }
            self.subject_submitted(&subject);
            return;
        }
        self.prompt = Some(Prompt::line("To: ", to_prefill, LineKind::ComposeTo));
    }

    fn setup_to_submitted(&mut self, input: &str) {
        let to = alias::expand(input, &alias::load_default());
        let Some(setup) = &mut self.compose_setup else {
            return;
        };
        setup.to = Some(to);
        let subject_prefill = match (&setup.kind, &setup.base) {
            (ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply, Some(b)) => {
                compose::reply_subject(&b.subject)
            }
            (ComposeKind::Forward, Some(b)) => compose::forward_subject(&b.from_addr, &b.subject),
            _ => String::new(),
        };
        // $fast_reply also skips the Subject prompt on forwards.
        if self.config.mail.fast_reply && !subject_prefill.is_empty() {
            self.subject_submitted(&subject_prefill);
            return;
        }
        self.prompt = Some(Prompt::line(
            "Subject: ",
            subject_prefill,
            LineKind::ComposeSubject,
        ));
    }

    /// After the Subject prompt: mutt's $abort_nosubject (ask-yes) on
    /// an empty subject, then on replies mutt's $include (ask-yes).
    fn subject_submitted(&mut self, input: &str) {
        if input.trim().is_empty() {
            self.prompt = Some(Prompt::Key {
                label: "No subject, abort? (y/n): ".into(),
                kind: KeyKind::NoSubject,
            });
            return;
        }
        self.subject_ready(input.to_string());
    }

    fn subject_ready(&mut self, subject: String) {
        let is_reply = self.compose_setup.as_ref().is_some_and(|s| {
            matches!(
                s.kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            ) && s.base.is_some()
        });
        let ask_fwd = self.config.mail.forward.as_deref() == Some("ask")
            && self
                .compose_setup
                .as_ref()
                .is_some_and(|s| s.kind == ComposeKind::Forward && s.base.is_some());
        if is_reply {
            if let Some(setup) = &mut self.compose_setup {
                setup.subject = Some(subject);
            }
            self.prompt = Some(Prompt::Key {
                label: "Include message in reply? (y/n): ".into(),
                kind: KeyKind::IncludeReply,
            });
        } else if ask_fwd {
            // mime_forward = "ask": whole original vs inline quote.
            if let Some(setup) = &mut self.compose_setup {
                setup.subject = Some(subject);
            }
            self.prompt = Some(Prompt::Key {
                label: "Forward as attachment? (y/n): ".into(),
                kind: KeyKind::ForwardAttach,
            });
        } else {
            self.finish_compose_setup(&subject, true);
        }
    }

    fn finish_compose_setup(&mut self, subject: &str, include: bool) {
        let Some(setup) = self.compose_setup.take() else {
            return;
        };
        let mut to = setup.to.unwrap_or_default();
        let mut cc = None;
        let mut in_reply_to = None;
        let mut references = None;
        let mut body = String::new();
        let mut attach = None;
        if let Some(b) = &setup.base {
            match setup.kind {
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply => {
                    if include {
                        let orig = message::body_text(&b.path).unwrap_or_default();
                        body =
                            compose::quote(&compose::attribution(&b.from_display, b.date), &orig);
                    }
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
                        // mutt honors a sender's Mail-Followup-To: it
                        // is exactly the recipient set they asked for,
                        // so it replaces To and leaves Cc alone.
                        if !b.followup_to.trim().is_empty() {
                            to = b.followup_to.trim().to_string();
                        } else {
                            // Everyone else on the original, minus the
                            // recipient already in To and (mutt's
                            // $metoo off) my own addresses: replying to
                            // all should not mail me a copy.
                            let joined = compose::group_recipients(
                                &b.orig_to,
                                &b.orig_cc,
                                &to,
                                self.me(),
                                self.config.mail.metoo,
                            );
                            if !joined.is_empty() {
                                cc = Some(joined);
                            }
                        }
                    }
                }
                ComposeKind::Forward
                    if setup.fwd_attach.unwrap_or_else(|| self.forward_attaches()) =>
                {
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
        let followup =
            self.followup_header(&to, cc.as_deref(), from.as_deref().unwrap_or_default());
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
        // DraftHeaders has no Mail-Followup-To slot; it goes ahead of
        // the blank line, where edit_headers shows it like any other.
        let text = match followup {
            Some(value) => match text.split_once("\n\n") {
                Some((head, rest)) => format!("{head}\nMail-Followup-To: {value}\n\n{rest}"),
                None => text,
            },
            None => text,
        };
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: None,
                    security: self.default_security(),
                    attach,
                    hidden_head,
                    fcc: None,
                });
            }
            Err(err) => self.error_status(format!("cannot write draft: {err:#}")),
        }
    }

    /// mutt's edit_headers (default false, like mutt): whether the
    /// header block is part of the editor buffer.
    fn edit_headers(&self) -> bool {
        self.config.mail.edit_headers.unwrap_or(false)
    }

    /// Write a fresh draft file for the editor: the whole text (with
    /// any `my_hdr` merged in), or (with edit_headers = false) only
    /// the body, the header block withheld for draft_full to rejoin.
    fn stage_draft(&self, text: &str) -> Result<(PathBuf, Option<String>)> {
        // mutt's my_hdr lands here, so every draft the TUI opens
        // carries it: with edit_headers the editor shows the lines,
        // without it they ride along in the withheld head.
        let text = compose::apply_my_hdr(text, &self.config.mail.my_hdr);
        if self.edit_headers() {
            return Ok((write_draft(&text)?, None));
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
            && let Some(from) = compose::reverse_from(&b.orig_to, &b.orig_cc, self.me())
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
                self.open_compose_menu();
            }
            _ => {
                self.error_status(format!(
                    "editor failed; draft kept at {}",
                    compose.path.display()
                ));
            }
        }
    }

    /// Mutt's compose menu: entered after the editor, and again after
    /// every sub-prompt, until y sends, P/q postpones, or q discards.
    fn open_compose_menu(&mut self) {
        if self.compose.is_some() {
            let sel = match self.mode {
                Mode::Compose { sel } => sel,
                _ => 0,
            };
            self.mode = Mode::Compose { sel };
        }
    }

    /// The draft's header block, wherever it currently lives.
    fn draft_head(&self) -> String {
        let Some(c) = &self.compose else {
            return String::new();
        };
        match &c.hidden_head {
            Some(head) => head.clone(),
            None => {
                let text = std::fs::read_to_string(&c.path).unwrap_or_default();
                match text.split_once("\n\n") {
                    Some((head, _)) => head.to_string(),
                    None => text.trim_end().to_string(),
                }
            }
        }
    }

    /// Rewrite the draft's header block in place (hidden or in-file).
    fn edit_draft_head(&mut self, f: impl Fn(&str) -> String) {
        let Some(c) = &mut self.compose else {
            return;
        };
        match &mut c.hidden_head {
            Some(head) => *head = f(head),
            None => {
                if let Ok(text) = std::fs::read_to_string(&c.path) {
                    let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
                    let _ = std::fs::write(&c.path, format!("{}\n\n{body}", f(head)));
                }
            }
        }
    }

    /// Replace (or add, or with an empty value drop) one header.
    fn set_draft_header(&mut self, name: &str, value: &str) {
        let value = value.trim().to_string();
        let name = name.to_string();
        self.edit_draft_head(|head| {
            let mut lines: Vec<&str> = head
                .lines()
                .filter(|l| {
                    !l.split_once(':')
                        .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(&name))
                })
                .collect();
            let added = format!("{name}: {value}");
            if !value.is_empty() {
                lines.push(&added);
            }
            lines.join("\n")
        });
    }

    fn draft_header(&self, name: &str) -> String {
        header_value(&self.draft_head(), name).unwrap_or_default()
    }

    /// Header lines the compose menu shows; From falls back to the
    /// identity that send would use.
    pub fn compose_header_lines(&self) -> Vec<(&'static str, String)> {
        let head = self.draft_head();
        let get = |n: &str| header_value(&head, n).unwrap_or_default();
        let from = match header_value(&head, "From") {
            Some(f) => f,
            None => self
                .current_identity(&[])
                .from_line()
                .unwrap_or_else(|| default_from(&maildir::hostname())),
        };
        let security = self
            .compose
            .as_ref()
            .map(|c| c.security.label())
            .filter(|l| !l.is_empty())
            .unwrap_or("none");
        let fcc = match self.compose.as_ref().and_then(|c| c.fcc.clone()) {
            Some(fcc) if fcc.is_empty() => "(no copy)".into(),
            Some(fcc) => fcc,
            None if self.config.mail.copy == Some(false) => "(no copy)".into(),
            None => {
                let default = self.default_fcc();
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
        let Some(c) = &self.compose else {
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
                self.send_draft();
            }
            KeyCode::Char('e') => {
                self.mode = Mode::Index;
                if let Some(c) = self.compose.take() {
                    self.pending_editor = Some(c);
                }
            }
            // Before plain t: ctrl+t edits the selected attachment's
            // content-type (mutt's edit-type).
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.prompt_attach_edit(true);
            }
            KeyCode::Char('t') => self.edit_header_prompt("To"),
            KeyCode::Char('c') => self.edit_header_prompt("Cc"),
            KeyCode::Char('b') => self.edit_header_prompt("Bcc"),
            KeyCode::Char('s') => self.edit_header_prompt("Subject"),
            KeyCode::Char('d') => self.prompt_attach_edit(false),
            KeyCode::Char('f') => {
                let buf = match self.compose.as_ref().and_then(|c| c.fcc.clone()) {
                    Some(fcc) => fcc,
                    None => self.default_fcc(),
                };
                self.prompt = Some(Prompt::line("Fcc: ", buf, LineKind::EditFcc));
            }
            KeyCode::Char('a') => {
                self.prompt = Some(Prompt::line(
                    "Attach file: ",
                    String::new(),
                    LineKind::AttachFile,
                ));
            }
            KeyCode::Enter => self.view_compose_entry(),
            KeyCode::Char('D') => self.detach_selected(),
            KeyCode::Char('p') => {
                self.prompt = Some(Prompt::Key {
                    label: "Security: (e)ncrypt (s)ign (b)oth (c)lear: ".into(),
                    kind: KeyKind::Security,
                });
            }
            KeyCode::Char('P') => {
                self.mode = Mode::Index;
                self.postpone_draft();
            }
            KeyCode::Char('q') => {
                self.prompt = Some(Prompt::Key {
                    label: "Postpone this message? (y/n): ".into(),
                    kind: KeyKind::PostponeAsk,
                });
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
        let Some(c) = &self.compose else {
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
            match self.config.filters.get(mimetype).cloned() {
                Some(command) => match run_file_filter(&command, &a.path) {
                    Ok(text) => (name, text),
                    Err(err) => {
                        self.error_status(format!("filter failed: {err:#}"));
                        return;
                    }
                },
                None if mimetype.starts_with("text/") => match std::fs::read_to_string(&a.path) {
                    Ok(text) => (name, text),
                    Err(err) => {
                        self.error_status(format!("cannot read {name}: {err}"));
                        return;
                    }
                },
                None => {
                    self.status = Some(format!("no [filters] entry for {mimetype}"));
                    return;
                }
            }
        };
        let mut lines = vec![title, String::new()];
        lines.extend(text.lines().map(String::from));
        self.mode = Mode::Help { lines, scroll: 0 };
    }

    /// The sent copy's default target, as the Fcc line shows it.
    fn default_fcc(&self) -> String {
        match &self.remote {
            Some(remote) => format!(
                "imap:{}/{}",
                remote.account.name, remote.account.sent_folder
            ),
            None => self.config.mail.sent.clone().unwrap_or_default(),
        }
    }

    /// d / ctrl+t on the compose menu: prompt for the selected
    /// attachment's description or content-type.
    fn prompt_attach_edit(&mut self, is_type: bool) {
        let Mode::Compose { sel } = self.mode else {
            return;
        };
        let fixed = 1 + usize::from(self.compose.as_ref().is_some_and(|c| c.attach.is_some()));
        if sel < fixed {
            self.error_status("only Attach: files can be edited");
            return;
        }
        let k = sel - fixed;
        let attachment = self
            .compose
            .as_ref()
            .and_then(|c| draft_full(c).ok())
            .map(|full| compose::extract_attachments(&full).1)
            .and_then(|mut atts| (k < atts.len()).then(|| atts.swap_remove(k)));
        let Some(a) = attachment else { return };
        self.attach_edit = Some(k);
        if is_type {
            let buf = a
                .mime
                .clone()
                .unwrap_or_else(|| compose::content_type(&a.path).to_string());
            self.prompt = Some(Prompt::line("Content-Type: ", buf, LineKind::EditType));
        } else {
            let buf = a.description.clone().unwrap_or_default();
            self.prompt = Some(Prompt::line("Description: ", buf, LineKind::EditDesc));
        }
    }

    /// The submitted d / ctrl+t edit: rewrite the k-th Attach: line
    /// with the new description or content-type.
    fn set_attach_field(&mut self, input: &str, is_type: bool) {
        let Some(k) = self.attach_edit.take() else {
            return;
        };
        let value = input.trim().to_string();
        self.edit_draft_head(|head| {
            let mut seen = 0usize;
            head.lines()
                .map(|l| {
                    let is_attach = l
                        .split_once(':')
                        .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("attach"));
                    if is_attach {
                        // Count like extract_attachments (empty-value
                        // lines don't), so k matches the menu entry.
                        let (_, mut atts) = compose::extract_attachments(l);
                        if let Some(mut a) = atts.pop() {
                            let idx = seen;
                            seen += 1;
                            if idx == k {
                                let new = (!value.is_empty()).then(|| value.clone());
                                if is_type {
                                    a.mime = new;
                                } else {
                                    a.description = new;
                                }
                                return compose::attach_line(&a);
                            }
                        }
                    }
                    l.to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
    }

    fn edit_header_prompt(&mut self, name: &'static str) {
        let kind = match name {
            "To" => LineKind::EditTo,
            "Cc" => LineKind::EditCc,
            "Bcc" => LineKind::EditBcc,
            _ => LineKind::EditSubject,
        };
        self.prompt = Some(Prompt::line(
            format!("{name}: "),
            self.draft_header(name),
            kind,
        ));
    }

    /// D in the compose menu: drop the selected Attach: line (the
    /// body and a forwarded original cannot be detached).
    fn detach_selected(&mut self) {
        let Mode::Compose { sel } = self.mode else {
            return;
        };
        let fixed = 1 + usize::from(self.compose.as_ref().is_some_and(|c| c.attach.is_some()));
        if sel < fixed {
            self.error_status("only Attach: files can be detached");
            return;
        }
        let k = sel - fixed;
        self.edit_draft_head(|head| {
            let mut seen = 0usize;
            head.lines()
                .filter(|l| {
                    let is_attach = l
                        .split_once(':')
                        .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("attach"));
                    if is_attach {
                        seen += 1;
                        seen - 1 != k
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        let len = self.compose_entries().len();
        if let Mode::Compose { sel } = &mut self.mode {
            *sel = (*sel).min(len.saturating_sub(1));
        }
    }

    /// Add an `Attach:` line to the draft's header block without a
    /// trip through the editor (send prompt `a`).
    fn attach_file_submitted(&mut self, input: &str) {
        let input = input.trim();
        if !input.is_empty() {
            if !expand_tilde(input).is_file() {
                self.error_status(format!("{input} is not a file"));
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
                    self.error_status(format!("cannot attach: {err}"));
                }
            }
        }
        self.open_compose_menu();
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
                self.error_status(format!("cannot read draft: {err}"));
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
                self.error_status(format!("{err}; press e to edit"));
                self.compose = Some(compose_state);
                self.open_compose_menu();
                return;
            }
        };
        let original = match &compose_state.attach {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    self.error_status(format!("cannot attach the original: {err}"));
                    self.compose = Some(compose_state);
                    self.open_compose_menu();
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
                self.error_status(format!("{err:#}; e edits, s changes security"));
                self.compose = Some(compose_state);
                self.open_compose_menu();
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
                // The menu's Fcc wins; empty means keep no copy, and
                // $copy = no makes that the default.
                let skip_copy = compose_state.fcc.as_deref() == Some("")
                    || (compose_state.fcc.is_none() && self.config.mail.copy == Some(false));
                if skip_copy {
                    // Nothing kept, on request.
                } else if let Some(fcc) = compose_state
                    .fcc
                    .as_deref()
                    .filter(|f| Some(*f) != Some(self.default_fcc().as_str()))
                {
                    // An explicit Fcc: a local maildir path.
                    let dir = expand_tilde(fcc);
                    let flags = maildir::Flags {
                        seen: true,
                        ..Default::default()
                    };
                    if dir.join("cur").is_dir()
                        && maildir::deliver(&dir, final_text.as_bytes(), flags).is_ok()
                    {
                        note += &format!(", copy in {fcc}");
                    } else {
                        note += &format!(", Fcc to {fcc} failed");
                    }
                } else {
                    match &mut self.remote {
                        // Fcc goes to the account's Sent folder on the
                        // server.
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
                                .or_else(|| {
                                    maildir::find_special(&self.dir, &["sent", "sent-mail"])
                                });
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
                }
                let _ = std::fs::remove_file(&compose_state.path);
                if let Some(src) = &compose_state.recall_source {
                    let _ = std::fs::remove_file(src);
                }
                self.status = Some(note);
            }
            Err(err) => {
                self.error_status(format!("send failed: {err:#}"));
                self.compose = Some(compose_state);
                self.open_compose_menu();
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
                self.error_status(format!(
                    "postpone failed: {err:#}; draft at {}",
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
            self.error_status("no postponed messages");
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
                    fcc: None,
                });
            }
            Err(err) => self.error_status(format!("cannot recall: {err:#}")),
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
    /// mutt's enter-command: `:` takes one config line for this
    /// session (the config file itself is never rewritten).
    fn open_command_prompt(&mut self) {
        self.prompt = Some(Prompt::line(":", String::new(), LineKind::EnterCommand));
    }

    /// Run one `:` line: apply every command it holds, then recompile
    /// whatever the change touched so it shows without a restart. The
    /// first failing command reports and stops the line, like mutt.
    fn run_command_line(&mut self, line: &str) {
        let commands = match command::parse(line) {
            Ok(commands) => commands,
            Err(err) => return self.error_status(err),
        };
        let sort_before = (
            self.config.index.sort.clone(),
            self.config.index.sort_aux.clone(),
        );
        let sidebar_before = self.config.sidebar.visible;
        let mut reports = Vec::new();
        for cmd in &commands {
            let outcome = match cmd {
                command::Command::Bind { .. } | command::Command::Macro { .. } => {
                    self.bind_command(cmd)
                }
                command::Command::Alias { nick, expansion } => self.alias_command(nick, expansion),
                command::Command::Push(seq) => self.push_command(seq),
                command::Command::Exec(function) => self.exec_command(function),
                config_command => command::apply(&mut self.config, config_command),
            };
            match outcome {
                Ok(Some(text)) => reports.push(text),
                Ok(None) => {}
                Err(err) => return self.error_status(err),
            }
        }
        if commands.is_empty() {
            return;
        }
        reports.extend(self.recompile());
        if sort_before
            != (
                self.config.index.sort.clone(),
                self.config.index.sort_aux.clone(),
            )
        {
            if let Some(spec) = self.config.index.sort.clone()
                && let Some((sort, rev)) = parse_sort(&spec)
            {
                self.sort = sort;
                self.sort_rev = rev;
            }
            self.apply_sort();
        }
        if self.config.sidebar.visible != sidebar_before {
            self.sidebar_visible = self.config.sidebar.visible;
        }
        if !reports.is_empty() {
            self.status = Some(reports.join("; "));
        }
    }

    /// Recompile the config-derived state (theme, key tables, color
    /// rules, quote regexp, header rules) and hand back any warnings.
    fn recompile(&mut self) -> Vec<String> {
        let (derived, warnings) = Derived::from_config(&self.config);
        self.theme = derived.theme;
        self.keymap = derived.keymap;
        self.lists = derived.lists;
        self.subscribed = derived.subscribed;
        self.alternates = derived.alternates;
        self.index_rules = derived.index_rules;
        self.body_rules = derived.body_rules;
        self.quote_re = derived.quote_re;
        self.head_rules = derived.head_rules;
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
                        &mut self.config.keys.index
                    } else {
                        &mut self.config.keys.pager
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
                        &mut self.config.macros.index
                    } else {
                        &mut self.config.macros.pager
                    };
                    table.insert(key.clone(), seq.clone());
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// `alias NICK ADDRESS`: appended to the alias file, like the `a`
    /// key, so it outlives the session.
    fn alias_command(&mut self, nick: &str, expansion: &str) -> Result<Option<String>, String> {
        if nick.contains(char::is_whitespace) {
            return Err("the alias nick must be one word".into());
        }
        match alias::append(nick, expansion) {
            Ok(_) => Ok(Some(format!("added: alias {nick} {expansion}"))),
            Err(err) => Err(format!("cannot save the alias: {err:#}")),
        }
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
        let from = self.compose_from(None, &m.to);
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
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                self.pending_editor = Some(Compose {
                    path,
                    recall_source: None,
                    security: self.default_security(),
                    attach: None,
                    hidden_head,
                    fcc: None,
                });
            }
            Err(err) => self.error_status(format!("cannot write draft: {err:#}")),
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
            self.error_status("macro expansion too deep, stopped");
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
        // Save marks the original deleted; a plain copy is fine.
        if delete && self.deny_readonly() {
            return;
        }
        let buf = self.config.mail.save.clone().unwrap_or_default();
        self.prompt = Some(Prompt::line(
            if delete {
                "Save to mailbox: "
            } else {
                "Copy to mailbox: "
            },
            buf,
            if delete {
                LineKind::SaveMsg
            } else {
                LineKind::CopyMsg
            },
        ));
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
            self.error_status(format!("cannot fetch message: {err:#}"));
            return None;
        }
        match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                self.error_status(format!("cannot read message: {err}"));
                None
            }
        }
    }

    /// Copy the message to a mailbox (local maildir path or a folder
    /// of the open IMAP account); with `delete` the original is marked
    /// deleted afterwards, mutt's s versus C.
    fn copy_message(&mut self, input: &str, delete: bool) {
        if input.is_empty() {
            self.error_status("no mailbox given");
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
                            self.error_status(format!("cannot save: {err:#}"));
                            return;
                        }
                    }
                }
                _ => {
                    self.error_status("can only save to a folder of the open account");
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
                        self.error_status(format!("cannot save: {err:#}"));
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
            self.error_status("the message has no From header");
            return;
        };
        let nick = compose::bare_address(&from)
            .and_then(|a| a.split('@').next().map(|l| l.to_lowercase()))
            .unwrap_or_default();
        self.alias_addr = Some(from.trim().to_string());
        self.prompt = Some(Prompt::line("Alias as (nick): ", nick, LineKind::AliasNick));
    }

    fn create_alias(&mut self, nick: &str) {
        let Some(addr) = self.alias_addr.take() else {
            return;
        };
        if nick.is_empty() || nick.contains(char::is_whitespace) {
            self.error_status("the alias nick must be one word");
            return;
        }
        match alias::append(nick, &addr) {
            Ok(_) => self.status = Some(format!("added: alias {nick} {addr}")),
            Err(err) => self.error_status(format!("cannot save the alias: {err:#}")),
        }
    }

    fn prompt_pipe(&mut self) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::line(
            "Pipe to command: ",
            String::new(),
            LineKind::Pipe,
        ));
    }

    /// Pipe the raw message to a shell command, like mutt's |.
    fn pipe_message(&mut self, command: &str) {
        if command.is_empty() {
            self.error_status("no command given");
            return;
        }
        let Some(bytes) = self.full_message_bytes() else {
            return;
        };
        match pipe_to(command, &bytes) {
            Ok(()) => self.status = Some(format!("piped to {command}")),
            Err(err) => self.error_status(format!("pipe failed: {err:#}")),
        }
    }

    fn prompt_bounce(&mut self) {
        if self.visible.get(self.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::line(
            "Bounce message to: ",
            String::new(),
            LineKind::BounceTo,
        ));
    }

    fn bounce_to_submitted(&mut self, input: &str) {
        let to = alias::expand(input, &alias::load_default());
        if to.trim().is_empty() {
            self.status = Some("no recipients, bounce cancelled".into());
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
            self.error_status(format!("cannot parse the addresses in {to:?}"));
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
            Err(err) => self.error_status(format!("bounce failed: {err:#}")),
        }
    }

    /// mutt's edit function (`e`): the selected message's raw bytes go
    /// through $EDITOR, and a changed result replaces the original:
    /// in place for maildirs, append + delete-mark on IMAP.
    fn start_raw_edit(&mut self) {
        if self.deny_readonly() {
            return;
        }
        if self.mbox.is_some() {
            self.error_status("editing in place is not supported for mbox spools");
            return;
        }
        if self.full_message_bytes().is_none() {
            return;
        }
        self.pending_raw_edit = self.selected_path();
    }

    fn edit_raw(&mut self, terminal: &mut DefaultTerminal, path: PathBuf) {
        let original = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                self.error_status(format!("cannot read message: {err}"));
                return;
            }
        };
        let temp = match write_draft("").and_then(|p| {
            std::fs::write(&p, &original)?;
            Ok(p)
        }) {
            Ok(p) => p,
            Err(err) => {
                self.error_status(format!("cannot write edit copy: {err:#}"));
                return;
            }
        };
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
            .arg(&temp)
            .status();
        *terminal = ratatui::init();
        let _ = terminal.clear();
        let edited = std::fs::read(&temp).unwrap_or_default();
        let _ = std::fs::remove_file(&temp);
        if !matches!(status, Ok(s) if s.success()) {
            self.error_status("editor failed, message unchanged");
            return;
        }
        if edited == original {
            self.status = Some("message unchanged".into());
            return;
        }
        match &mut self.remote {
            Some(remote) => {
                // Like mutt on IMAP: the edited copy is appended and
                // the original marked deleted, purged on the next $.
                let flags = self.msgs[self.visible[self.sel]].env.file.flags;
                let mailbox = remote.mailbox.clone();
                if let Err(err) = remote.append_to(&mailbox, flags, &edited) {
                    self.error_status(format!("cannot store the edited copy: {err:#}"));
                    return;
                }
                if let Some(m) = self.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
                self.check_new_mail();
                self.status =
                    Some("edited copy appended; original marked deleted ($ purges)".into());
            }
            None => {
                if let Err(err) = std::fs::write(&path, &edited) {
                    self.error_status(format!("cannot write message: {err}"));
                    return;
                }
                self.rescan();
                self.status = Some("message edited".into());
            }
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
            self.error_status("no message selected");
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
                    fcc: None,
                });
            }
            Err(err) => self.error_status(format!("cannot write draft: {err:#}")),
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

    /// True (with a status note) when a mutating operation must be
    /// refused because of `rmut -R`.
    fn deny_readonly(&mut self) -> bool {
        if self.read_only {
            self.error_status("Mailbox is read-only.");
        }
        self.read_only
    }

    fn mark_read(&mut self) {
        if self.read_only {
            return;
        }
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
        let mut view = message::load_with(path, &self.config.filters, &self.head_rules)?;
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

    /// An error status: rendered in the error color with a bell,
    /// unlike informational notes (mutt's mutt_error vs mutt_message).
    fn error_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_error = true;
        if self.config.ui.beep {
            use std::io::Write as _;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
    }

    fn open_selected(&mut self) {
        self.mark_read();
        // Opening a message ends the pager search, like mutt (whose
        // compiled search is per pager session); the text stays as
        // the next prompt's prefill.
        self.pager_search = None;
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
                    hide_quoted: false,
                    back: None,
                });
            }
            Err(err) => self.error_status(format!("cannot open message: {err:#}")),
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
                self.error_status(format!("cannot print: {err:#}"));
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
            Err(err) => self.error_status(format!("print failed: {err:#}")),
        }
    }

    fn toggle_collapse(&mut self, all: bool) {
        if self.sort != SortKey::Threads {
            self.error_status("folding needs thread sort (o t)");
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
        let positions = self.positions();
        for (i, pos) in positions.iter().enumerate() {
            let limit_ok = match &self.limit {
                Some((_, patterns)) => self.env_matches_at(patterns, &self.msgs[i].env, *pos),
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
            self.error_status("no search pattern (use /)");
            return;
        };
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len();
        let positions = self.positions();
        for step in 1..=n {
            let vi = (self.sel + step) % n;
            let mi = self.visible[vi];
            if self.env_matches_at(&patterns, &self.msgs[mi].env, positions[mi]) {
                if vi <= self.sel {
                    self.status = Some("search wrapped".into());
                }
                self.sel = vi;
                return;
            }
        }
        self.error_status("not found");
    }

    /// Tab / Alt+Tab: jump to the next (previous) new-or-unread
    /// message, wrapping around with a note (mutt's
    /// next-new-then-unread).
    fn jump_new(&mut self, forward: bool) {
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        for (vi, wrapped) in wrap_order(n, self.sel, forward) {
            let m = &self.msgs[self.visible[vi]];
            if m.env.file.is_new || !m.env.file.flags.seen {
                if wrapped {
                    self.status = Some("search wrapped".into());
                }
                self.select(vi);
                return;
            }
        }
        self.error_status("no new or unread messages");
    }

    /// Apply `f` to every message matching `input`, within the active
    /// limit (members of folded threads included; folding is display
    /// only), and report the count.
    fn apply_pattern(&mut self, input: &str, verb: &'static str, f: impl Fn(&mut Msg)) {
        if input.is_empty() {
            return;
        }
        let patterns = match pattern::parse(input) {
            Ok(p) => p,
            Err(err) => {
                self.error_status(format!("bad pattern: {err}"));
                return;
            }
        };
        self.resolve_body_terms(&patterns);
        let positions = self.positions();
        let mut count = 0;
        for (i, pos) in positions.iter().enumerate() {
            let in_limit = match &self.limit {
                Some((_, l)) => self.env_matches_at(l, &self.msgs[i].env, *pos),
                None => true,
            };
            if in_limit && self.env_matches_at(&patterns, &self.msgs[i].env, *pos) {
                f(&mut self.msgs[i]);
                count += 1;
            }
        }
        self.status = Some(format!("{count} {verb}"));
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
        if self.deny_readonly() {
            return;
        }
        // $trash: purged messages move there first; a failed copy
        // aborts the purge. Purging inside the trash deletes for real.
        if purge
            && self.deleted_count() > 0
            && let Some(trash) = self.config.mail.trash.clone()
            && trash != self.title
            && expand_tilde(&trash) != self.dir
            && let Err(err) = self.trash_deleted(&trash)
        {
            self.error_status(format!("trash failed: {err:#}; nothing purged"));
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
                self.error_status(format!("sync failed: {err:#}"));
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
                self.error_status(format!("sync failed: {err:#}"));
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

pub(crate) fn send_via_smtp(account: &Account, text: &str) -> Result<()> {
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

/// With `rcpts` the addresses go on the command line (a bounce keeps
/// its Resent-To out of -t's reach); otherwise -t reads To/Cc/Bcc.
pub(crate) fn run_sendmail(
    bytes: &[u8],
    configured: Option<&str>,
    rcpts: Option<&[String]>,
) -> Result<()> {
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

/// Visit order for a wrapping scan over `n` entries starting after
/// (before, when backwards) `sel`: each index paired with a flag set
/// once the walk passed the end (start). The starting index comes
/// last, so a lone match under the cursor still counts as a wrap.
fn wrap_order(n: usize, sel: usize, forward: bool) -> Vec<(usize, bool)> {
    (1..=n)
        .map(|step| {
            if forward {
                ((sel + step) % n, sel + step >= n)
            } else {
                ((sel + n - (step % n)) % n, step > sel)
            }
        })
        .collect()
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
    use super::subject_key;

    #[test]
    fn subject_key_strips_reply_prefixes() {
        assert_eq!(subject_key("Re: Re: Lunch"), "lunch");
        assert_eq!(subject_key("FWD: re: x"), "x");
        assert_eq!(subject_key("Redo"), "redo");
    }

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

    #[test]
    fn wrap_order_visits_everything_once() {
        use super::wrap_order;
        // Forward from 1 of 4: 2, 3, then around to 0 and back to 1.
        assert_eq!(
            wrap_order(4, 1, true),
            vec![(2, false), (3, false), (0, true), (1, true)]
        );
        // Backwards from 1: 0, then around past the start.
        assert_eq!(
            wrap_order(4, 1, false),
            vec![(0, false), (3, true), (2, true), (1, true)]
        );
        assert_eq!(wrap_order(0, 0, true), vec![]);
        // A single entry: the walk comes straight back, marked wrapped.
        assert_eq!(wrap_order(1, 0, true), vec![(0, true)]);
    }
}
