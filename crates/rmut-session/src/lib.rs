//! rmut's mail session: an open mailbox and the operations over it.
//!
//! Everything here works without a screen. A front end owns the menus,
//! the keys and the drawing, and drives a [`Session`] for the rest:
//! what is in the mailbox, what is selected, what a mark or a sync
//! did. Outcomes leave as [`Notice`]s through the sink the front end
//! installs, so an operation never has to know how it will be shown.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::mem;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use rmut_core::config::{Account, Config};
use rmut_core::message::Envelope;
use rmut_core::notice::{Notice, NoticeSink};
use rmut_core::pattern::{self, Pattern};
use rmut_core::remote::{self, Remote};
use rmut_core::{alias, compose, hdrcache, maildir, mbox, message, pgp, smtp, thread};

mod ask;
mod commands;
mod drafts;

pub use ask::{Answer, Ask, AskKind, Key, PatternOp, Request, Wants};
pub use commands::CommandRun;

/// How many undo steps to keep, and how many message snapshots in
/// total: a pattern delete over a huge mailbox is one step but very
/// many marks, so both are bounded and the oldest steps go first.
const UNDO_MAX_STEPS: usize = 32;
const UNDO_MAX_MARKS: usize = 100_000;

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

/// One message's state before a step touched it. The path is the
/// identity: it only changes when the mailbox is written, and a write
/// drops the whole stack.
#[derive(Clone)]
pub struct MsgMark {
    pub path: PathBuf,
    pub flags: maildir::Flags,
    pub is_new: bool,
    pub tagged: bool,
    pub dirty: bool,
}

/// One undoable step: what it was, the messages as they stood before
/// it, and where the cursor was.
pub struct UndoStep {
    pub what: String,
    pub marks: Vec<MsgMark>,
    /// The message the cursor was on, by path: a resort or a limit
    /// can move it, so the position alone would not find it again.
    pub sel: Option<PathBuf>,
    /// Files the step created (a save or copy's delivered message),
    /// removed again when it is undone.
    pub created: Vec<PathBuf>,
    /// Something the undo cannot take back, said out loud when it
    /// runs (a copy that went to an IMAP folder).
    pub note: Option<String>,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ThreadOp {
    Delete,
    Undelete,
    Tag,
}

/// A compiled hook: the pattern its message must match, and what the
/// hook carries (an enter-command line, or an Fcc mailbox).
pub struct Hook {
    patterns: Vec<Pattern>,
    /// The command line to run, or the mailbox to file the copy in.
    pub value: String,
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
    pub path: PathBuf,
    pub reply_to: String,
    /// The From header as written, the reply target when the
    /// Reply-To question is answered no.
    pub from_hdr: String,
    /// The message has a Reply-To differing from From, worth the
    /// mutt $reply_to (ask-yes) question.
    pub has_reply_to: bool,
    pub orig_to: String,
    pub orig_cc: String,
    /// List-Post's posting address, when the list published one.
    pub list_post: Option<String>,
    /// Mail-Followup-To as the sender wrote it, honored by a group
    /// reply (mutt does the same).
    pub followup_to: String,
    /// Bare author address, for the forward subject's %a.
    pub from_addr: String,
    pub from_display: String,
    pub subject: String,
    pub date: i64,
    pub msg_id: Option<String>,
    pub references: Vec<String>,
}

pub struct ComposeSetup {
    pub kind: ComposeKind,
    pub base: Option<ComposeBase>,
    pub to: Option<String>,
    /// Parked here while the include-original question is up.
    pub subject: Option<String>,
    /// mime_forward = "ask": the question's answer, once given.
    pub fwd_attach: Option<bool>,
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
    pub fn label(self) -> &'static str {
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

/// A message that has been sent but is waiting out $undo_send before
/// it goes anywhere. The finalized text is ready to transmit; the
/// draft it came from is kept whole, so cancelling puts the compose
/// menu back exactly as it was.
pub struct Held {
    pub state: Compose,
    pub text: String,
    /// The Fcc target as it was decided at send time (menu, then
    /// fcc-hook); None means the default sent copy.
    pub fcc: Option<String>,
    /// What the status line calls it: the subject, or the recipients.
    pub label: String,
    pub due: Instant,
}

/// One open mailbox and everything rmut knows about it.
pub struct Session {
    /// The maildir on disk: the mailbox itself, or the cache mirror of
    /// an IMAP folder or an mbox file.
    pub dir: PathBuf,
    /// What this mailbox is called: the path for local maildirs, the
    /// `imap:account/folder` spec for remote ones.
    pub title: String,
    /// Set when `dir` is the cache maildir of an IMAP folder. Public
    /// while the operations that reach for it (opening another
    /// folder, saving to one, listing them) still live in the front
    /// end; they move here in their own step.
    pub remote: Option<Remote>,
    /// Set when `dir` mirrors an mbox file; sync writes back into it.
    pub mbox: Option<mbox::Mbox>,
    pub msgs: Vec<Msg>,
    /// Indices into `msgs` after applying limit and thread folding.
    pub visible: Vec<usize>,
    /// Selection, as an index into `visible`.
    pub sel: usize,
    pub sort: SortKey,
    pub sort_rev: bool,
    pub limit: Option<(String, Vec<Pattern>)>,
    pub last_search: Option<Vec<Pattern>>,
    /// Which way the last index search went, so `n` repeats in the
    /// same direction (mutt's search / search-reverse pair).
    pub search_rev: bool,
    /// Per-message thread depth/root (aligned with `msgs`; identity
    /// when not sorted by threads).
    pub thread_depth: Vec<usize>,
    pub thread_root: Vec<usize>,
    /// Paths of collapsed thread roots.
    pub collapsed: HashSet<PathBuf>,
    /// Undo stack, oldest first: delete/flag/tag/read marks and the
    /// copies a save made, back to the state before each step.
    undo: Vec<UndoStep>,
    pub config: Config,
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
    /// Server-side `~b` results: term -> matching UIDs, filled when a
    /// limit/search pattern with body terms is submitted on IMAP.
    body_hits: HashMap<String, HashSet<u32>>,
    /// Background IDLE watcher for the open IMAP folder.
    pub idle: Option<remote::IdleWatch>,
    /// Background header mirror for the tail of a huge IMAP folder.
    backfill: Option<remote::Backfill>,
    /// New-mail counts of the other configured mailboxes at the last
    /// poll, to notice growth (mutt's `mailboxes` awareness).
    mailbox_new: HashMap<String, usize>,
    /// `rmut -R`: nothing is ever written, not even read marks.
    pub read_only: bool,
    /// `-R`: the whole session is read-only, so a mailbox switch
    /// cannot quietly make it writable again. Alt+c sets `read_only`
    /// for one mailbox without touching this.
    pub read_only_session: bool,
    /// mtimes of new/ and cur/ used for new-mail detection.
    pub(crate) dir_mtimes: (Option<SystemTime>, Option<SystemTime>),
    /// Compiled hook tables (patterns plus the line or mailbox each
    /// carries). The front end still runs a hook's command line,
    /// because one can rebind a key; which hooks match a message is
    /// the session's answer to give.
    pub message_hooks: Vec<Hook>,
    pub reply_hooks: Vec<Hook>,
    fcc_hooks: Vec<Hook>,
    crypt_hooks: Vec<(pattern::Matcher, String)>,
    /// ignore/unignore/hdr_order and the [filters] table: how a
    /// message's parts turn into the text a reader sees.
    pub display: message::Display,
    /// Messages sent but still inside their $undo_send window, oldest
    /// first. They go out when the timer runs out or rmut leaves.
    outbox: Vec<Held>,
    /// Compiled $quote_regexp classifying quoted body lines: a pager
    /// colours by it, and the attachment reminder skips them.
    pub quote_re: regex_lite::Regex,
    /// Compiled $abort_noattach_regex, for the attachment reminder.
    attach_re: regex_lite::Regex,
    /// The compose flow in progress: what is being answered about
    /// the draft that has not been written yet.
    setup: Option<ComposeSetup>,
    /// The draft in hand: written, and being looked at. A front end
    /// shows it however it shows drafts; the session owns what it
    /// says.
    draft: Option<Compose>,
    /// The attachment reminder has been answered for this draft: the
    /// next send goes through without asking again.
    attach_confirmed: bool,
    /// The config as it was before the active message-hooks changed
    /// it, so leaving the message puts every setting back.
    hook_base: Option<Box<Config>>,
    /// Which message-hooks are in force right now, by index; a change
    /// here is what triggers restore-and-reapply.
    active_message_hooks: Vec<usize>,
    /// What the session wants the front end to do, oldest first.
    requests: Vec<Request>,
    /// Where outcomes go. Nothing is kept until a front end installs
    /// a sink, so a session used as a library is silent by default.
    notices: Box<dyn NoticeSink>,
}

/// The sink a session starts with: nothing said, nothing kept.
struct Silence;

impl NoticeSink for Silence {
    fn notice(&mut self, _notice: Notice) {}

    fn latest(&self) -> Option<&Notice> {
        None
    }

    fn clear(&mut self) {}
}

impl Session {
    /// Open a maildir.
    ///
    /// Warnings come back rather than going out as notices: at this
    /// point no front end has installed a sink, and they are its to
    /// show together with whatever its own setup had to say.
    pub fn open(dir: &Path, config: Config) -> Result<(Session, Vec<String>)> {
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
        let mut warnings = Vec::new();
        if skipped > 0 {
            warnings.push(format!("{skipped} unreadable message(s) skipped"));
        }
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
        let mut session = Session {
            dir: dir.to_path_buf(),
            title: dir.display().to_string(),
            remote: None,
            mbox: None,
            msgs,
            visible,
            sel,
            sort: SortKey::Date,
            sort_rev: false,
            limit: None,
            last_search: None,
            search_rev: false,
            thread_depth: vec![0; count],
            thread_root: (0..count).collect(),
            collapsed: HashSet::new(),
            undo: Vec::new(),
            lists: config.list_matchers(),
            subscribed: config.subscribed_matchers(),
            alternates: config.alternate_matchers(),
            my_addresses: me,
            body_hits: HashMap::new(),
            idle: None,
            backfill: None,
            mailbox_new: HashMap::new(),
            read_only: false,
            read_only_session: false,
            dir_mtimes: dir_mtimes(dir),
            message_hooks: Vec::new(),
            reply_hooks: Vec::new(),
            fcc_hooks: Vec::new(),
            crypt_hooks: Vec::new(),
            display: display_from_config(&config),
            outbox: Vec::new(),
            quote_re: default_quote_re(),
            attach_re: default_attach_re(),
            config,
            setup: None,
            draft: None,
            attach_confirmed: false,
            hook_base: None,
            active_message_hooks: Vec::new(),
            requests: Vec::new(),
            notices: Box::new(Silence),
        };
        let mut hook_warnings = Vec::new();
        session.quote_re = quote_re_from_config(&session.config, &mut hook_warnings);
        session.attach_re = attach_re_from_config(&session.config, &mut hook_warnings);
        session.compile_hooks_from_config(&mut hook_warnings);
        hook_warnings.append(&mut warnings);
        let mut warnings = hook_warnings;
        if let Some(spec) = session.config.index.sort.clone() {
            match parse_sort(&spec) {
                Some((sort, rev)) => {
                    session.sort = sort;
                    session.sort_rev = rev;
                    session.resort(None);
                }
                None => warnings.push(format!("unknown sort {spec:?} in config")),
            }
        }
        Ok((session, warnings))
    }

    /// Open a mailbox by spec: an `imap:account[/folder]` string (the
    /// folder is mirrored into a cache maildir) or a local path.
    ///
    /// `progress` is how the front end shows slow network work; a
    /// library caller that has nothing to show passes a closure that
    /// throws the lines away.
    pub fn open_spec(
        spec: &str,
        config: Config,
        progress: remote::Progress,
    ) -> Result<(Session, Vec<String>)> {
        match remote::parse_spec(spec) {
            Some((account_name, mailbox)) => {
                let account = config
                    .account(account_name)
                    .with_context(|| format!("no account {account_name} in config"))?
                    .clone();
                let password = account_password(&account)?;
                let remote = Remote::open(&account, mailbox, &password, progress)?;
                let cache = remote.cache.clone();
                let (mut session, warnings) = Session::open(&cache, config)?;
                session.title = remote.spec.clone();
                // IDLE on a second connection; NOOP polling stays as
                // the fallback when the server doesn't support it.
                session.idle = Some(remote::idle_watch(&account, &remote.mailbox, &password));
                session.remote = Some(remote);
                Ok((session, warnings))
            }
            None => {
                let path = expand_tilde(spec);
                if path.is_file() {
                    return Session::open_mbox(&path, config);
                }
                Session::open(&path, config)
            }
        }
    }

    /// Open an mbox file (e.g. /var/mail/$USER) through its cache
    /// mirror; `$` sync writes changes back into the file.
    pub fn open_mbox(path: &Path, config: Config) -> Result<(Session, Vec<String>)> {
        let mbox = mbox::Mbox::open(path)?;
        let cache = mbox.cache.clone();
        let (mut session, warnings) = Session::open(&cache, config)?;
        session.title = path.display().to_string();
        session.mbox = Some(mbox);
        Ok((session, warnings))
    }

    /// Recompile what the session derives from the config, after a
    /// command changed it.
    pub fn recompile(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        self.display = display_from_config(&self.config);
        self.quote_re = quote_re_from_config(&self.config, &mut warnings);
        self.attach_re = attach_re_from_config(&self.config, &mut warnings);
        self.compile_hooks_from_config(&mut warnings);
        self.lists = self.config.list_matchers();
        self.subscribed = self.config.subscribed_matchers();
        self.alternates = self.config.alternate_matchers();
        warnings
    }

    /// The hook tables, compiled from the config; a bad pattern warns
    /// and drops rather than failing the whole table.
    fn compile_hooks_from_config(&mut self, warnings: &mut Vec<String>) {
        self.message_hooks = compile_hooks(
            "message-hook",
            self.config
                .message_hooks
                .iter()
                .map(|h| (&h.pattern, &h.command)),
            warnings,
        );
        self.reply_hooks = compile_hooks(
            "reply-hook",
            self.config
                .reply_hooks
                .iter()
                .map(|h| (&h.pattern, &h.command)),
            warnings,
        );
        self.fcc_hooks = compile_hooks(
            "fcc-hook",
            self.config
                .fcc_hooks
                .iter()
                .map(|h| (&h.pattern, &h.mailbox)),
            warnings,
        );
        self.crypt_hooks = self
            .config
            .crypt_hooks
            .iter()
            .map(|h| (pattern::Matcher::new(&h.address), h.key.clone()))
            .collect();
    }

    /// Open another mailbox in place: the same session, pointed
    /// somewhere else.
    ///
    /// The config, the `-R` flag and the notice sink stay; everything
    /// about the old mailbox goes. Another folder of the open account
    /// reuses the live connection (a SELECT) instead of a fresh
    /// connect and login; a dead session falls through to the full
    /// open. Warnings come back for the front end to show, as at
    /// startup.
    pub fn switch_to(&mut self, spec: &str, progress: remote::Progress) -> Result<Vec<String>> {
        let reuse = match remote::parse_spec(spec) {
            Some((account, mailbox))
                if self
                    .remote
                    .as_ref()
                    .is_some_and(|r| r.account.name == account)
                    && self
                        .remote
                        .as_mut()
                        .is_some_and(|r| r.switch(mailbox).is_ok()) =>
            {
                self.remote.take()
            }
            _ => None,
        };
        let config = self.config.clone();
        let (mut next, warnings) = match &reuse {
            Some(remote) => Session::open(&remote.cache.clone(), config)?,
            None => Session::open_spec(spec, config, progress)?,
        };
        if let Some(remote) = reuse {
            next.title = remote.spec.clone();
            // A second connection for IDLE, as the first open makes.
            if let Ok(password) = account_password(&remote.account) {
                next.idle = Some(remote::idle_watch(
                    &remote.account,
                    &remote.mailbox,
                    &password,
                ));
            }
            next.remote = Some(remote);
        }
        // A -R session stays read-only whatever it opens; Alt+c sets
        // read_only for one mailbox and does not survive the switch.
        next.read_only_session = self.read_only_session;
        next.read_only = self.read_only_session;
        next.notices = mem::replace(&mut self.notices, Box::new(Silence));
        *self = next;
        Ok(warnings)
    }

    /// Hand the session the front end's notice sink. Until this is
    /// called nothing said is kept.
    pub fn install_notices(&mut self, sink: Box<dyn NoticeSink>) {
        self.notices = sink;
    }

    /// Forget the last notice: the front end has shown it, or the
    /// user has moved on.
    pub fn clear_notice(&mut self) {
        self.notices.clear();
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

    pub fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.sel = index.min(self.visible.len() - 1);
    }

    pub fn cur_mut(&mut self) -> Option<&mut Msg> {
        let i = self.visible.get(self.sel).copied()?;
        self.msgs.get_mut(i)
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.visible
            .get(self.sel)
            .map(|&i| self.msgs[i].env.file.path.clone())
    }

    /// True (with a status note) when a mutating operation must be
    /// refused because of `rmut -R`.
    pub fn deny_readonly(&mut self) -> bool {
        if self.read_only {
            self.error("Mailbox is read-only.");
        }
        self.read_only
    }

    pub fn mark_read(&mut self) {
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

    /// What the pattern engine needs beyond one message: my addresses,
    /// the configured mailing lists, and the place in the list.
    pub fn scope(&self, position: pattern::Position) -> pattern::Scope<'_> {
        pattern::Scope {
            me: self.me(),
            lists: &self.lists,
            position,
        }
    }

    /// Which addresses are mine: the identity ones plus `alternates`.
    pub fn me(&self) -> pattern::Me<'_> {
        pattern::Me::new(&self.my_addresses, &self.alternates)
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
    pub fn resolve_body_terms(&mut self, patterns: &[Pattern]) {
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

    /// Re-read the maildir, keeping unsynced flag changes and deletion
    /// marks for messages that are still there.
    pub fn rescan(&mut self) {
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
            self.note(format!("new mail in {} (+{arrived})", self.title));
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

    /// Look for new mail: the server (or the mbox file) first, then
    /// the other configured mailboxes, then this one's own maildir.
    ///
    /// `between` runs after the other mailboxes have been counted and
    /// before this mailbox is rescanned, which is where a front end
    /// showing those counts wants to redraw them.
    pub fn check_new_mail(&mut self, between: &mut dyn FnMut(&mut Session)) {
        let backfilling = self.backfill.as_ref().is_some_and(|b| !b.done());
        if let Some(remote) = &mut self.remote {
            // While the backfill streams headers in, skip the server
            // check, since a full reconcile would refetch its tail
            // synchronously; the rescan below integrates the files.
            if backfilling {
            } else if let Err(err) = remote.check_new() {
                self.error(format!("imap: {err:#}"));
            }
        }
        self.maybe_backfill();
        if let Some(mbox) = &mut self.mbox {
            // Re-mirror when the file changed; same rescan pickup.
            if let Err(err) = mbox.refresh() {
                self.error(format!("mbox: {err:#}"));
            }
        }
        self.check_other_mailboxes();
        between(self);
        if dir_mtimes(&self.dir) == self.dir_mtimes {
            return;
        }
        self.rescan();
    }

    /// Whether the open IMAP folder's IDLE watcher has seen a change
    /// since it was last asked.
    pub fn idle_kick(&self) -> bool {
        self.idle.as_ref().is_some_and(|w| w.take_changed())
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
        if !grew.is_empty() && self.notice().is_none() {
            self.note(format!("new mail in {}", grew.join(", ")));
        }
    }

    /// Spawn (or finish) the background mirror for a huge folder's
    /// leftover headers; the poll rescan integrates them as they land.
    pub fn maybe_backfill(&mut self) {
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
        self.note(format!(
            "loading {count} older message(s) in the background"
        ));
    }

    /// Apply `f` to every tagged message, marking them dirty.
    pub fn each_tagged(&mut self, what: &str, f: impl Fn(&mut Msg)) {
        let tagged: Vec<usize> = (0..self.msgs.len())
            .filter(|&i| self.msgs[i].env.tagged)
            .collect();
        self.push_undo(what, &tagged);
        for &i in &tagged {
            let m = &mut self.msgs[i];
            f(m);
            m.dirty = true;
        }
        self.note(format!("applied to {} tagged message(s)", tagged.len()));
    }

    /// The selected message's raw bytes, completing a header-only IMAP
    /// cache file first. Failures land in the status line.
    /// The messages an operation applies to: the tagged set when `;`
    /// asked for it, otherwise the one under the cursor. Tagged means
    /// tagged anywhere, limit or no limit, like the other tagged ops.
    /// What an operation applies to: the tagged set when `;` asked
    /// for it, otherwise the message under the cursor.
    pub fn op_targets(&self, tagged: bool) -> Vec<usize> {
        if tagged {
            return (0..self.msgs.len())
                .filter(|&i| self.msgs[i].env.tagged)
                .collect();
        }
        self.visible.get(self.sel).copied().into_iter().collect()
    }

    /// Remember `indices` as they stand, so `z` can put them back.
    /// Steps with nothing in them are not worth a slot.
    pub fn push_undo(&mut self, what: &str, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let marks = indices.iter().map(|&i| self.mark(i)).collect();
        self.push_undo_step(UndoStep {
            what: what.to_string(),
            marks,
            sel: self.selected_path(),
            created: Vec::new(),
            note: None,
        });
    }

    pub fn push_undo_step(&mut self, step: UndoStep) {
        self.undo.push(step);
        // Oldest first out, on either bound.
        while self.undo.len() > UNDO_MAX_STEPS
            || (self.undo.len() > 1
                && self.undo.iter().map(|s| s.marks.len()).sum::<usize>() > UNDO_MAX_MARKS)
        {
            self.undo.remove(0);
        }
    }

    pub fn mark(&self, i: usize) -> MsgMark {
        let m = &self.msgs[i];
        MsgMark {
            path: m.env.file.path.clone(),
            flags: m.env.file.flags,
            is_new: m.env.file.is_new,
            tagged: m.env.tagged,
            dirty: m.dirty,
        }
    }

    /// Walk back the last step: the marks it saved go back onto the
    /// messages that still carry those paths, and any file it created
    /// is removed. Writing the mailbox drops the stack, so a step here
    /// is always one that has not reached disk.
    /// Walk back the last step on the stack.
    pub fn undo_last(&mut self) {
        let Some(step) = self.undo.pop() else {
            self.note("nothing to undo");
            return;
        };
        let mut restored = 0usize;
        let by_path: HashMap<&Path, usize> = self
            .msgs
            .iter()
            .enumerate()
            .map(|(i, m)| (m.env.file.path.as_path(), i))
            .collect();
        let mut wanted: Vec<(usize, MsgMark)> = Vec::new();
        for mark in &step.marks {
            if let Some(&i) = by_path.get(mark.path.as_path()) {
                wanted.push((i, mark.clone()));
            }
        }
        for (i, mark) in wanted {
            let m = &mut self.msgs[i];
            m.env.file.flags = mark.flags;
            m.env.file.is_new = mark.is_new;
            m.env.tagged = mark.tagged;
            m.dirty = mark.dirty;
            restored += 1;
        }
        let mut failed = Vec::new();
        for path in &step.created {
            if let Err(err) = std::fs::remove_file(path) {
                failed.push(format!("{}: {err}", path.display()));
            }
        }
        if let Some(vi) = step.sel.and_then(|p| {
            self.visible
                .iter()
                .position(|&i| self.msgs[i].env.file.path == p)
        }) {
            self.sel = vi;
        }
        let mut note = format!("undone: {} ({restored} message(s))", step.what);
        if let Some(extra) = &step.note {
            note += &format!("; {extra}");
        }
        if !failed.is_empty() {
            self.error(format!("{note}; could not remove {}", failed.join("; ")));
        } else {
            self.note(note);
        }
    }

    /// The raw message on disk, fetched first when the IMAP cache
    /// holds headers only.
    pub fn message_bytes(&mut self, i: usize) -> Option<Vec<u8>> {
        let path = self.msgs.get(i)?.env.file.path.clone();
        if let Some(remote) = &mut self.remote
            && remote::is_partial(&path)
            && let Err(err) = remote.fetch_body(&path)
        {
            self.error(format!("cannot fetch message: {err:#}"));
            return None;
        }
        match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                self.error(format!("cannot read message: {err}"));
                None
            }
        }
    }

    pub fn full_message_bytes(&mut self) -> Option<Vec<u8>> {
        let &i = self.visible.get(self.sel)?;
        self.message_bytes(i)
    }

    /// Every message the operation applies to, back to back.
    pub fn op_bytes(&mut self, tagged: bool) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for i in self.op_targets(tagged) {
            let bytes = self.message_bytes(i)?;
            out.extend_from_slice(&bytes);
            if !bytes.ends_with(b"\n") {
                out.push(b'\n');
            }
        }
        Some(out)
    }

    /// Rebuild `visible` from the limit and collapsed threads, keeping
    /// the selection on the message at `keep` when still visible.
    pub fn rebuild_visible(&mut self, keep: Option<PathBuf>) {
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

    pub fn apply_sort(&mut self) {
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

    pub fn toggle_collapse(&mut self, all: bool) {
        if self.sort != SortKey::Threads {
            self.error("folding needs thread sort (o t)");
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

    /// The next/previous visible position from the selection; mutt's
    /// next-/previous-undeleted skips messages flagged for deletion.
    pub fn step_message(&self, forward: bool, skip_deleted: bool) -> Option<usize> {
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

    /// Tab / Alt+Tab: jump to the next (previous) new-or-unread
    /// message, wrapping around with a note (mutt's
    /// next-new-then-unread).
    /// The messages a thread operation applies to: the selected
    /// message's whole thread, or (with `sub`) the selected message
    /// and its replies. None when the index is not thread-sorted,
    /// which is also when mutt refuses.
    fn thread_targets(&mut self, sub: bool) -> Option<Vec<usize>> {
        if self.sort != SortKey::Threads {
            self.error("thread operations need thread sort (o t)");
            return None;
        }
        let &mi = self.visible.get(self.sel)?;
        if !sub {
            let root = self.thread_root.get(mi).copied().unwrap_or(mi);
            return Some(
                (0..self.msgs.len())
                    .filter(|&i| self.thread_root.get(i).copied().unwrap_or(i) == root)
                    .collect(),
            );
        }
        // Thread sort lays the messages out depth-first, so a
        // message's replies are the run after it that stays deeper.
        let depth = self.thread_depth.get(mi).copied().unwrap_or(0);
        let mut out = vec![mi];
        for i in (mi + 1)..self.msgs.len() {
            if self.thread_depth.get(i).copied().unwrap_or(0) <= depth {
                break;
            }
            out.push(i);
        }
        Some(out)
    }

    /// mutt's delete-thread / undelete-thread / tag-thread and their
    /// subthread halves: one keystroke, one undo step, however many
    /// messages hang off it.
    pub fn thread_mark(&mut self, sub: bool, op: ThreadOp) {
        if op != ThreadOp::Tag && self.deny_readonly() {
            return;
        }
        let Some(&mi) = self.visible.get(self.sel) else {
            return;
        };
        let Some(targets) = self.thread_targets(sub) else {
            return;
        };
        let scope = if sub { "subthread" } else { "thread" };
        let (what, verb) = match op {
            ThreadOp::Delete => (format!("delete {scope}"), "deleted"),
            ThreadOp::Undelete => (format!("undelete {scope}"), "undeleted"),
            // mutt's tag-thread follows the message under the cursor:
            // the whole thread takes the opposite of its tag.
            ThreadOp::Tag if self.msgs[mi].env.tagged => (format!("untag {scope}"), "untagged"),
            ThreadOp::Tag => (format!("tag {scope}"), "tagged"),
        };
        self.push_undo(&what, &targets);
        let want_tag = !self.msgs[mi].env.tagged;
        for &i in &targets {
            match op {
                ThreadOp::Delete => {
                    self.msgs[i].env.file.flags.deleted = true;
                    self.msgs[i].dirty = true;
                }
                ThreadOp::Undelete => {
                    self.msgs[i].env.file.flags.deleted = false;
                    self.msgs[i].dirty = true;
                }
                ThreadOp::Tag => self.msgs[i].env.tagged = want_tag,
            }
        }
        self.note(format!("{} {verb}", targets.len()));
        // mutt's $resolve, which is what makes deleting thread after
        // thread one repeated key; a rescue stays where it is.
        if op == ThreadOp::Delete
            && let Some(pos) = self.step_message(true, true)
        {
            self.sel = pos;
        }
    }

    /// mutt's next-thread / previous-thread: the first message of the
    /// thread either side of this one.
    pub fn jump_thread(&mut self, forward: bool) {
        if self.sort != SortKey::Threads {
            self.error("thread operations need thread sort (o t)");
            return;
        }
        for (vi, wrapped) in wrap_order(self.visible.len(), self.sel, forward) {
            let mi = self.visible[vi];
            if self.thread_depth.get(mi).copied().unwrap_or(0) == 0 {
                if wrapped {
                    self.note("wrapped around");
                }
                self.select(vi);
                return;
            }
        }
        self.error("no other thread");
    }

    pub fn jump_new(&mut self, forward: bool) {
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        for (vi, wrapped) in wrap_order(n, self.sel, forward) {
            let m = &self.msgs[self.visible[vi]];
            if m.env.file.is_new || !m.env.file.flags.seen {
                if wrapped {
                    self.note("search wrapped");
                }
                self.select(vi);
                return;
            }
        }
        self.error("no new or unread messages");
    }

    /// Apply `f` to every message matching `input`, within the active
    /// limit (members of folded threads included; folding is display
    /// only), and report the count.
    pub fn apply_pattern(&mut self, input: &str, verb: &'static str, f: impl Fn(&mut Msg)) {
        if input.is_empty() {
            return;
        }
        let patterns = match pattern::parse(input) {
            Ok(p) => p,
            Err(err) => {
                self.error(format!("bad pattern: {err}"));
                return;
            }
        };
        self.resolve_body_terms(&patterns);
        let positions = self.positions();
        let mut hits = Vec::new();
        for (i, pos) in positions.iter().enumerate() {
            let in_limit = match &self.limit {
                Some((_, l)) => self.env_matches_at(l, &self.msgs[i].env, *pos),
                None => true,
            };
            if in_limit && self.env_matches_at(&patterns, &self.msgs[i].env, *pos) {
                hits.push(i);
            }
        }
        self.push_undo(&format!("{verb} by pattern"), &hits);
        for &i in &hits {
            f(&mut self.msgs[i]);
        }
        self.note(format!("{} {verb}", hits.len()));
    }

    pub fn search_next(&mut self) {
        let Some(patterns) = self.last_search.clone() else {
            self.error("no search pattern (use /)");
            return;
        };
        if self.visible.is_empty() {
            return;
        }
        let positions = self.positions();
        for (vi, wrapped) in wrap_order(self.visible.len(), self.sel, !self.search_rev) {
            let mi = self.visible[vi];
            if self.env_matches_at(&patterns, &self.msgs[mi].env, positions[mi]) {
                if wrapped {
                    self.note("search wrapped");
                }
                self.sel = vi;
                return;
            }
        }
        self.error("not found");
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
    pub fn sync(&mut self, purge: bool) {
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
            self.error(format!("trash failed: {err:#}; nothing purged"));
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
                self.error(format!("sync failed: {err:#}"));
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
                self.error(format!("sync failed: {err:#}"));
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
        // The marks are on disk now, and the paths the stack keyed on
        // have been renamed away: there is nothing left to walk back.
        self.undo.clear();
        self.resort(keep);
        match errors.is_empty() {
            true => self.notify(Notice::Synced {
                deleted: removed,
                updated: saved,
            }),
            false => self.note(format!("sync errors: {}", errors.join("; "))),
        }
    }

    /// An error status: rendered in the error color with a bell,
    /// unlike informational notes (mutt's mutt_error vs mutt_message).
    /// Mailbox specs for the folder browser and for Tab completion at
    /// a mailbox prompt: the configured mailboxes, the open account's
    /// folders (IMAP LIST), and maildirs discovered next to the open
    /// one.
    pub fn folder_candidates(&mut self) -> Result<Vec<(String, usize)>> {
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

    /// mutt's $mark_old (on by default): when leaving the mailbox,
    /// unread new mail ages to old: moved out of new/ without the
    /// seen flag, shown as O and no longer counted as new.
    pub fn mark_old_unread(&mut self) {
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
    pub fn ready_to_leave(&mut self) -> bool {
        if self.deleted_count() > 0 {
            self.error("deleted messages pending; sync with $ or undelete first");
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

    /// Which account to submit outgoing mail through. Explicit sendmail
    /// configuration ($RMUT_SENDMAIL or mail.sendmail) wins; otherwise
    /// the open mailbox's account, or the first one with an smtp_host.
    pub fn smtp_account(&self) -> Option<Account> {
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

    /// Copy the message to a mailbox (local maildir path or a folder
    /// of the open IMAP account); with `delete` the original is marked
    /// deleted afterwards, mutt's s versus C.
    pub fn copy_message(&mut self, input: &str, delete: bool, tagged: bool) {
        if input.is_empty() {
            self.error("no mailbox given");
            return;
        }
        let targets = self.op_targets(tagged);
        if targets.is_empty() {
            return;
        }
        // What the undo of this step has to take back: the copies just
        // delivered, and (for a save) the originals' deleted marks.
        let mut created: Vec<PathBuf> = Vec::new();
        let mut marks = Vec::new();
        let mut copied = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut note = None;
        let mut target = String::new();
        for &i in &targets {
            match self.copy_one(i, input, &mut created) {
                Ok(shown) => {
                    if shown.starts_with("imap:") {
                        note = Some(format!("the copy in {shown} stays"));
                    }
                    target = shown;
                    marks.push(self.mark(i));
                    copied.push(i);
                }
                // One bad message does not undo the good ones: the
                // rest still go, and the trouble is reported after.
                Err(err) => errors.push(err),
            }
        }
        let verb = if delete { "save" } else { "copy" };
        if copied.is_empty() {
            self.error(format!("cannot {verb}: {}", errors.join("; ")));
            return;
        }
        self.push_undo_step(UndoStep {
            what: format!("{verb} to {target}"),
            marks,
            sel: self.selected_path(),
            created,
            note,
        });
        let n = copied.len();
        if delete {
            for &i in &copied {
                self.msgs[i].env.file.flags.deleted = true;
                self.msgs[i].dirty = true;
            }
        }
        let mut status = match (delete, n) {
            (true, 1) => format!("saved to {target} (original marked deleted)"),
            (true, _) => format!("saved {n} to {target} (originals marked deleted)"),
            (false, 1) => format!("copied to {target}"),
            (false, _) => format!("copied {n} to {target}"),
        };
        if !errors.is_empty() {
            status += &format!("; {} failed: {}", errors.len(), errors.join("; "));
            self.error(status);
        } else {
            self.note(status);
        }
    }

    /// One message into `spec`: a folder of the open IMAP account, or
    /// a local maildir path (created if missing). Returns where it
    /// went, and pushes the delivered file, which undo removes.
    fn copy_one(
        &mut self,
        i: usize,
        spec: &str,
        created: &mut Vec<PathBuf>,
    ) -> Result<String, String> {
        let flags = self.msgs[i].env.file.flags;
        let bytes = self.message_bytes(i).ok_or("cannot read the message")?;
        match remote::parse_spec(spec) {
            Some((account, folder)) => match &mut self.remote {
                Some(remote) if remote.account.name == account => remote
                    .append_to(folder, flags, &bytes)
                    .map(|folder| format!("imap:{account}/{folder}"))
                    .map_err(|err| format!("{err:#}")),
                _ => Err("can only save to a folder of the open account".into()),
            },
            None => {
                let dir = expand_tilde(spec);
                maildir::create(&dir)
                    .and_then(|()| maildir::deliver(&dir, &bytes, flags))
                    .map(|path| {
                        created.push(path);
                        dir.display().to_string()
                    })
                    .map_err(|err| format!("{err:#}"))
            }
        }
    }

    /// mutt's create-alias: one line appended to the alias file.
    pub fn create_alias(&mut self, nick: &str, addr: &str) {
        if nick.is_empty() || nick.contains(char::is_whitespace) {
            self.error("the alias nick must be one word");
            return;
        }
        match alias::append(nick, addr) {
            Ok(_) => self.note(format!("added: alias {nick} {addr}")),
            Err(err) => self.error(format!("cannot save the alias: {err:#}")),
        }
    }

    /// Pipe the raw message to a shell command, like mutt's |. With
    /// `;` the tagged messages are concatenated into one run of the
    /// command, which is what mutt does with $pipe_split unset.
    pub fn pipe_message(&mut self, command: &str, tagged: bool) {
        if command.is_empty() {
            self.error("no command given");
            return;
        }
        let Some(bytes) = self.op_bytes(tagged) else {
            return;
        };
        let n = self.op_targets(tagged).len();
        match pipe_to(command, &bytes) {
            Ok(()) => self.note(match n {
                1 => format!("piped to {command}"),
                _ => format!("piped {n} messages to {command}"),
            }),
            Err(err) => self.error(format!("pipe failed: {err:#}")),
        }
    }

    /// Resend the message as-is to new recipients: Resent-* headers on
    /// top, the rest untouched.
    pub fn bounce_current(&mut self, to: &str, tagged: bool) {
        let rcpts = compose::addresses(to);
        if rcpts.is_empty() {
            self.error(format!("cannot parse the addresses in {to:?}"));
            return;
        }
        let targets = self.op_targets(tagged);
        let mut sent = 0usize;
        for i in targets {
            let Some(bytes) = self.message_bytes(i) else {
                return;
            };
            if let Err(err) = self.bounce_one(&bytes, to, &rcpts) {
                self.error(format!("bounce failed: {err:#}"));
                return;
            }
            sent += 1;
        }
        self.note(match sent {
            1 => format!("message bounced to {to}"),
            _ => format!("{sent} messages bounced to {to}"),
        });
    }

    /// One message resent as-is: Resent-* headers on top, the rest
    /// untouched.
    fn bounce_one(&mut self, bytes: &[u8], to: &str, rcpts: &[String]) -> Result<()> {
        let host = maildir::hostname();
        let from = self
            .current_identity(rcpts)
            .from_line()
            .unwrap_or_else(|| default_from(&host));
        let text = compose::bounce_text(
            bytes,
            &from,
            to,
            &compose::rfc2822_now(),
            &compose::make_message_id(&host),
        );
        let envelope_from = compose::bare_address(&from).unwrap_or_else(|| from.clone());
        match self.smtp_account() {
            Some(account) => account_password(&account).and_then(|password| {
                smtp::send(&account, &password, &envelope_from, rcpts, text.as_bytes())
            }),
            None => run_sendmail(
                text.as_bytes(),
                self.config.mail.sendmail.as_deref(),
                Some(rcpts),
            ),
        }
    }

    /// Fetch (IMAP), parse, and PGP-process a message the way the
    /// pager shows it.
    pub fn load_view(&mut self, path: &Path) -> Result<message::MessageView> {
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
        let mut view = message::load_with(path, &self.display)?;
        // PGP messages: decrypt/verify via gpg, prepend the verdict
        // line to whatever body ends up shown.
        if let Ok(raw) = std::fs::read(path)
            && let Some(p) = pgp::view(&self.config.pgp, &raw)
        {
            match p.body {
                // A decrypted PGP/MIME entity is a MIME tree of its
                // own: render it whole, so attachments inside
                // encrypted mail are announced like any others.
                Some(pgp::Body::Entity(raw)) => {
                    view.body = message::render_entity(&raw, &self.display);
                }
                Some(pgp::Body::Text(text)) => view.body = text,
                None => {}
            }
            view.body = format!("{}\n\n{}", p.note, view.body);
        }
        Ok(view)
    }

    /// Pipe the message as displayed (brief headers, decoded body) to
    /// the configured print command, lpr by default.
    pub fn print_current(&mut self, tagged: bool) {
        let targets = self.op_targets(tagged);
        if targets.is_empty() {
            return;
        }
        let mut text = String::new();
        for i in targets.iter().copied() {
            let path = self.msgs[i].env.file.path.clone();
            let view = match self.load_view(&path) {
                Ok(v) => v,
                Err(err) => {
                    self.error(format!("cannot print: {err:#}"));
                    return;
                }
            };
            if !text.is_empty() {
                text.push('\n');
            }
            for (name, value) in &view.brief {
                text += &format!("{name}: {value}\n");
            }
            text.push('\n');
            text += &view.body;
        }
        let n = targets.len();
        let command = self
            .config
            .mail
            .print
            .clone()
            .unwrap_or_else(|| "lpr".into());
        match pipe_to(&command, text.as_bytes()) {
            Ok(()) => self.note(match n {
                1 => format!("printed via {command}"),
                _ => format!("printed {n} messages via {command}"),
            }),
            Err(err) => self.error(format!("print failed: {err:#}")),
        }
    }

    /// The identity in effect for this mailbox (and, when known, the
    /// draft's recipients).
    pub fn current_identity(&self, rcpts: &[String]) -> rmut_core::config::Identity {
        self.config
            .identity_for(&self.title, rcpts, self.remote.as_ref().map(|r| &r.account))
    }

    /// Indices of the message-hooks the selected message matches.
    pub fn matching_message_hooks(&self) -> Vec<usize> {
        let Some(env) = self.visible.get(self.sel).map(|&mi| &self.msgs[mi].env) else {
            return Vec::new();
        };
        // `~m` and `~=` want the whole list; a hook asks about one
        // message, so only its own numbering is filled in.
        let pos = pattern::Position {
            number: self.sel + 1,
            current: self.sel + 1,
            last: self.visible.len(),
            duplicate: false,
        };
        self.message_hooks
            .iter()
            .enumerate()
            .filter(|(_, h)| pattern::matches_in(&h.patterns, env, self.scope(pos), None))
            .map(|(i, _)| i)
            .collect()
    }

    /// mutt's reply-hook: the command lines whose pattern matches the
    /// message being replied to. Running them is the front end's, and
    /// so is putting the config back afterwards.
    pub fn reply_hook_lines(&self, path: &Path) -> Vec<String> {
        if self.reply_hooks.is_empty() {
            return Vec::new();
        }
        let Some(env) = self.msgs.iter().find(|m| m.env.file.path == *path) else {
            return Vec::new();
        };
        let scope = self.scope(pattern::Position::default());
        self.reply_hooks
            .iter()
            .filter(|h| pattern::matches_in(&h.patterns, &env.env, scope, None))
            .map(|h| h.value.clone())
            .collect()
    }

    /// mutt's fcc-hook: the mailbox the first matching entry names for
    /// this outgoing draft, or None when nothing matches.
    pub fn fcc_hook_target(&self, draft: &str, path: &Path) -> Option<String> {
        if self.fcc_hooks.is_empty() {
            return None;
        }
        let env = compose::draft_envelope(draft, path);
        let scope = self.scope(pattern::Position::default());
        self.fcc_hooks
            .iter()
            .find(|h| pattern::matches_in(&h.patterns, &env, scope, None))
            .map(|h| h.value.clone())
    }

    /// mutt's crypt-hook: a recipient with a hook of its own is
    /// encrypted to that key id instead of to its address.
    pub fn crypt_key_for(&self, address: &str) -> Option<String> {
        self.crypt_hooks
            .iter()
            .find(|(m, _)| m.is_match(address))
            .map(|(_, key)| key.clone())
    }

    /// `X` submitted: `notmuch search --output=files` into a virtual
    /// read-only mailbox: the hits are symlinked into a cache
    /// maildir (the real copies stay where they are), so viewing,
    /// replying, copying, and piping work while flag changes and
    /// deletes stay refused.
    /// Mirror a notmuch query into a maildir of symlinks, ready to be
    /// opened. Returns where it is and how many messages it holds.
    pub fn notmuch_mirror(&mut self, query: &str) -> Option<(PathBuf, usize)> {
        if query.is_empty() {
            return None;
        }
        let out = Command::new("notmuch")
            .args(["search", "--output=files", "--limit=1000", "--", query])
            .output();
        let out = match out {
            Ok(out) if out.status.success() => out,
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                self.error(format!("notmuch: {}", err.trim()));
                return None;
            }
            Err(err) => {
                self.error(format!("notmuch: {err}"));
                return None;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let files: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        if files.is_empty() {
            self.error("notmuch: no matches");
            return None;
        }
        if !self.ready_to_leave() {
            return None;
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
            self.note(format!("notmuch mirror: {err:#}"));
            return None;
        }
        Some((dir, files.len()))
    }

    pub fn compose_base(&self) -> Option<ComposeBase> {
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

    /// The To prompt, prefilled for replies with Reply-To (the
    /// question's yes) or the plain From (its no).
    /// Where a list reply goes: the list's own List-Post address when
    /// it published one, else the first To/Cc address that matches a
    /// configured list.
    pub fn list_target(&self, base: &ComposeBase) -> Option<String> {
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
    pub fn followup_header(&self, to: &str, cc: Option<&str>, from: &str) -> Option<String> {
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

    /// mutt's edit_headers (default false, like mutt): whether the
    /// header block is part of the editor buffer.
    pub fn edit_headers(&self) -> bool {
        self.config.mail.edit_headers.unwrap_or(false)
    }

    /// Write a fresh draft file for the editor: the whole text (with
    /// any `my_hdr` merged in), or (with edit_headers = false) only
    /// the body, the header block withheld for draft_full to rejoin.
    pub fn stage_draft(&self, text: &str) -> Result<(PathBuf, Option<String>)> {
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
    pub fn compose_from(&self, base: Option<&ComposeBase>, to: &str) -> Option<String> {
        if self.config.identity.reverse_name
            && let Some(b) = base
            && let Some(from) = compose::reverse_from(&b.orig_to, &b.orig_cc, self.me())
        {
            return Some(from);
        }
        let rcpts = compose::addresses(to);
        self.current_identity(&rcpts).from_line()
    }

    pub fn forward_attaches(&self) -> bool {
        self.config.mail.forward.as_deref() == Some("attach")
    }

    /// The sent copy's default target, as the Fcc line shows it. An
    /// fcc-hook matching the draft on screen wins, so the menu shows
    /// where the copy is really going.
    /// Where the sent copy goes by default, as the Fcc line shows it.
    /// An fcc-hook matching `draft` wins, so the menu shows where the
    /// copy is really going; delivery passes None, having resolved
    /// the hook when the message was sent.
    pub fn default_fcc(&self, draft: Option<&Compose>) -> String {
        if let Some(compose) = draft
            && let Ok(full) = draft_full(compose)
            && let Some(mailbox) = self.fcc_hook_target(&full, &compose.path)
        {
            return mailbox;
        }
        match &self.remote {
            Some(remote) => format!(
                "imap:{}/{}",
                remote.account.name, remote.account.sent_folder
            ),
            None => self.config.mail.sent.clone().unwrap_or_default(),
        }
    }

    /// Initial security for a fresh draft, from the [pgp] config.
    pub fn default_security(&self) -> Security {
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

    /// Anything due in the outbox goes out; whatever still waits owns
    /// the status line, counting down.
    pub fn tick_outbox(&mut self) {
        while self.outbox.first().is_some_and(|h| h.due <= Instant::now()) {
            let held = self.outbox.remove(0);
            self.deliver(held);
        }
        if let Some(next) = self.outbox.first()
            && !self.notice().is_some_and(|n| n.is_error())
        {
            let left = next.due.saturating_duration_since(Instant::now()).as_secs() + 1;
            self.note(format!("sending {} in {left}s (z cancels)", next.label));
        }
    }

    /// Send everything still waiting, on the way out: the messages
    /// were confirmed, only `z` takes one back.
    /// Send everything still waiting, on the way out: the messages
    /// were confirmed, only `z` takes one back. Trouble comes back as
    /// lines for the front end to print once the terminal is its own
    /// again, since nobody would see a message line by then.
    pub fn flush_outbox(&mut self) -> Vec<String> {
        let mut trouble = Vec::new();
        while !self.outbox.is_empty() {
            let held = self.outbox.remove(0);
            let label = held.label.clone();
            self.deliver(held);
            if let Some(err) = self.notice().filter(|n| n.is_error()).map(|n| n.text()) {
                trouble.push(format!("{label}: {err}"));
                self.clear_notice();
            }
        }
        trouble
    }

    /// Hold a sent message for its $undo_send window. It goes out on
    /// the next tick after it falls due, or when the session is
    /// flushed on the way out.
    pub fn hold_send(&mut self, held: Held) {
        self.outbox.push(held);
    }

    /// Take the newest held message back, draft and all. None when
    /// nothing was waiting.
    pub fn cancel_send(&mut self) -> bool {
        let Some(held) = self.outbox.pop() else {
            return false;
        };
        self.note(format!("send cancelled: {}", held.label));
        self.hand_back(held.state);
        true
    }

    /// Transmit a held message and keep the Fcc copy.
    /// Transmit a held message and keep the Fcc copy. A send that
    /// fails hands the draft back, for the front end to put on screen
    /// again.
    pub fn deliver(&mut self, held: Held) {
        let Held {
            state: compose_state,
            text: final_text,
            fcc: chosen,
            ..
        } = held;
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
                let skip_copy = chosen.as_deref() == Some("")
                    || (chosen.is_none() && self.config.mail.copy == Some(false));
                if skip_copy {
                    // Nothing kept, on request.
                } else if let Some(fcc) = chosen
                    .as_deref()
                    .filter(|f| Some(*f) != Some(self.default_fcc(None).as_str()))
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
                self.note(note);
            }
            Err(err) => {
                self.error(format!("send failed: {err:#}"));
                self.hand_back(compose_state);
            }
        }
    }

    /// neomutt's attachment reminder: does the body mention one when
    /// nothing is attached? Quoted lines and anything below a `-- `
    /// signature do not count, so a reply to "see attached" and a
    /// signature naming one are not false alarms.
    pub fn attachment_forgotten(&self, raw: &str, compose: &Compose) -> bool {
        if self.config.mail.abort_noattach.as_deref().unwrap_or("no") == "no" {
            return false;
        }
        if compose.attach.is_some() || !compose::extract_attachments(raw).1.is_empty() {
            return false;
        }
        let body = raw.split_once("\n\n").map_or("", |(_, body)| body);
        for line in body.lines() {
            if line.trim_end() == "--" || line == "-- " {
                break;
            }
            if self.quote_re.is_match(line) {
                continue;
            }
            if self.attach_re.is_match(line) {
                return true;
            }
        }
        false
    }

    /// Assemble the outgoing message from a finalized draft: `files`
    /// and the forwarded `original` first turn it into multipart/mixed,
    /// then the chosen PGP treatment wraps whatever entity resulted.
    pub fn secure_message(
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
            // mutt's crypt-hook: a recipient with a key of its own is
            // encrypted to that key id, not to its address.
            for r in &mut rcpts {
                if let Some(key) = self.crypt_key_for(r) {
                    *r = key;
                }
            }
            rcpts.sort();
            rcpts.dedup();
            Ok(rcpts)
        };
        let flowed = self.config.mail.text_flowed;
        if files.is_empty() && original.is_none() {
            return match security {
                // No MIME wrapper at all, so $text_flowed has to
                // declare the body itself.
                Security::None if flowed => Ok(compose::flow_plain(&text)),
                Security::None => Ok(text),
                Security::Sign => pgp::sign_message(cfg, &text, flowed),
                Security::Encrypt | Security::Both => pgp::encrypt_message(
                    cfg,
                    &recipients(&text)?,
                    security == Security::Both,
                    &text,
                    flowed,
                ),
            };
        }
        let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
        let entity = compose::mixed_entity(body, files, original, flowed)?;
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

    pub fn postponed_dir(&self) -> Option<PathBuf> {
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

    pub fn has_postponed(&self) -> bool {
        self.postponed_dir()
            .and_then(|d| maildir::scan(&d).ok())
            .is_some_and(|files| !files.is_empty())
    }

    /// mutt's postpone: the draft goes to the postponed maildir,
    /// headers and all, ready for a recall.
    pub fn postpone_draft(&mut self, compose_state: Compose) {
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
                self.note(format!("postponed to {}", path.display()));
            }
            Err(err) => {
                self.error(format!(
                    "postpone failed: {err:#}; draft at {}",
                    compose_state.path.display()
                ));
            }
        }
    }

    /// A postponed draft read back into a compose state, for the
    /// front end to hand to the editor. None when it cannot be read.
    pub fn recall_file(&mut self, source: PathBuf) -> Option<Compose> {
        let result = std::fs::read_to_string(&source)
            .map_err(anyhow::Error::from)
            .and_then(|content| self.stage_draft(&content));
        match result {
            Ok((path, hidden_head)) => Some(Compose {
                path,
                recall_source: Some(source),
                security: self.default_security(),
                attach: None,
                hidden_head,
                fcc: None,
            }),
            Err(err) => {
                self.error(format!("cannot recall: {err:#}"));
                None
            }
        }
    }

    /// The draft's header block, wherever it currently lives.
    pub fn draft_head(&self) -> String {
        let Some(c) = &self.draft else {
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
    pub fn edit_draft_head(&mut self, f: impl Fn(&str) -> String) {
        let Some(c) = &mut self.draft else {
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
    pub fn set_draft_header(&mut self, name: &str, value: &str) {
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

    pub fn draft_header(&self, name: &str) -> String {
        header_value(&self.draft_head(), name).unwrap_or_default()
    }

    /// Send the draft in hand. It may need a question answered
    /// first, and if it cannot go the draft stays in hand with a
    /// request to put it back on screen.
    pub fn send_draft(&mut self) -> Option<Ask> {
        let compose_state = self.draft.take()?;
        let raw = match draft_full(&compose_state) {
            Ok(r) => r,
            Err(err) => {
                self.error(format!("cannot read draft: {err}"));
                return None;
            }
        };
        // neomutt's $abort_noattach: the body says "attached" and
        // nothing is. Asked once per draft; an answered draft sends.
        if !mem::take(&mut self.attach_confirmed) && self.attachment_forgotten(&raw, &compose_state)
        {
            self.draft = Some(compose_state);
            match self.config.mail.abort_noattach.as_deref() {
                // neomutt's "yes" aborts outright: attach the file,
                // or take the word out of the body.
                Some("yes") => {
                    self.error("no attachment: not sent (abort_noattach); a attaches one");
                    self.requests.push(Request::ShowDraft);
                    return None;
                }
                _ => {
                    return Some(Ask::Key {
                        label:
                            "The body mentions an attachment and none is attached. Send? (y/n): "
                                .into(),
                        what: AskKind::NoAttach,
                    });
                }
            }
        }
        // mutt's fcc-hook, evaluated on the draft as it stands after
        // the editor; an Fcc picked in the menu still wins.
        let draft_path = self
            .draft
            .as_ref()
            .map(|c| c.path.clone())
            .unwrap_or_default();
        let hook_fcc = self.fcc_hook_target(&raw, &draft_path);
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
                self.error(format!("{err}; press e to edit"));
                self.hand_back(compose_state);
                return None;
            }
        };
        let original = match &compose_state.attach {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    self.error(format!("cannot attach the original: {err}"));
                    self.hand_back(compose_state);
                    return None;
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
                self.error(format!("{err:#}; e edits, s changes security"));
                self.hand_back(compose_state);
                return None;
            }
        };
        // The menu's Fcc wins, then any fcc-hook; empty means keep no
        // copy, and $copy = no makes that the default.
        let held = Held {
            text: final_text,
            fcc: compose_state.fcc.clone().or(hook_fcc),
            label: {
                let head = raw.split_once("\n\n").map_or(raw.as_str(), |(h, _)| h);
                header_value(head, "Subject")
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| header_value(head, "To"))
                    .unwrap_or_else(|| "message".into())
            },
            state: compose_state,
            due: Instant::now() + Duration::from_secs(self.config.mail.undo_send),
        };
        // $undo_send: the message waits, and z takes it back.
        if self.config.mail.undo_send > 0 {
            self.note(format!(
                "sending {} in {}s (z cancels)",
                held.label, self.config.mail.undo_send
            ));
            self.hold_send(held);
            return None;
        }
        self.deliver(held);
        None
    }

    /// The attachment reminder is answered for this draft: the next
    /// send goes through without asking again.
    fn confirm_attachment(&mut self) {
        self.attach_confirmed = true;
    }

    /// A draft that could not go: back in hand, and back on screen.
    fn hand_back(&mut self, draft: Compose) {
        self.draft = Some(draft);
        self.requests.push(Request::ShowDraft);
    }

    /// The draft in hand, for a front end showing it.
    pub fn draft(&self) -> Option<&Compose> {
        self.draft.as_ref()
    }

    pub fn draft_mut(&mut self) -> Option<&mut Compose> {
        self.draft.as_mut()
    }

    /// Take it out of the session's hands (the front end is about to
    /// edit it, postpone it, or throw it away).
    pub fn take_draft(&mut self) -> Option<Compose> {
        self.draft.take()
    }

    /// Put one back, after an editor has been over it.
    pub fn set_draft(&mut self, draft: Compose) {
        self.draft = Some(draft);
    }

    /// The submitted d / ctrl+t edit: rewrite the k-th Attach: line
    /// with the new description or content-type.
    /// The submitted description or content-type for the k-th
    /// `Attach:` line.
    pub fn set_attach_field(&mut self, k: usize, input: &str, is_type: bool) {
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

    /// D in the compose menu: drop the selected Attach: line (the
    /// body and a forwarded original cannot be detached).
    /// Drop the `Attach:` line the menu's `sel`-th row stands for.
    pub fn detach(&mut self, sel: usize) {
        let fixed = 1 + usize::from(self.draft().is_some_and(|c| c.attach.is_some()));
        if sel < fixed {
            self.error("only Attach: files can be detached");
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
    }

    /// Add an `Attach:` line to the draft's header block without a
    /// trip through the editor (send prompt `a`).
    /// A file added to the draft as an `Attach:` line.
    pub fn attach_file(&mut self, input: &str) {
        let input = input.trim();
        if !input.is_empty() {
            if !expand_tilde(input).is_file() {
                self.error(format!("{input} is not a file"));
            } else if let Some(c) = self.draft_mut() {
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
                    self.error(format!("cannot attach: {err}"));
                }
            }
        }
        self.requests.push(Request::ShowDraft);
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.notify(Notice::Error(msg.into()));
    }

    /// Something worth saying that is not a complaint.
    pub fn note(&mut self, msg: impl Into<String>) {
        self.notify(Notice::Info(msg.into()));
    }

    /// Emit, and ring the bell on an error if the user wants one.
    /// The bell belongs to the front end, not to the notice: $beep
    /// can be set from a command mid-session, so it is read here
    /// rather than remembered by the sink.
    fn notify(&mut self, notice: Notice) {
        if notice.is_error() && self.config.ui.beep {
            use std::io::Write as _;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
        self.notices.notice(notice);
    }

    /// The last thing said, for the message line and for the callers
    /// that only speak up when nothing else has.
    pub fn notice(&self) -> Option<&Notice> {
        self.notices.latest()
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
pub fn parse_sort(spec: &str) -> Option<(SortKey, bool)> {
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

fn dir_mtimes(dir: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let mtime = |p: PathBuf| p.metadata().and_then(|m| m.modified()).ok();
    (mtime(dir.join("new")), mtime(dir.join("cur")))
}

/// Run the account's password command once per session. OAuth tokens
/// expire, so those are fetched fresh for every connection instead.
pub fn account_password(account: &Account) -> Result<String> {
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

/// Visit order for a wrapping scan over `n` entries starting after
/// (before, when backwards) `sel`: each index paired with a flag set
/// once the walk passed the end (start). The starting index comes
/// last, so a lone match under the cursor still counts as a wrap.
pub fn wrap_order(n: usize, sel: usize, forward: bool) -> Vec<(usize, bool)> {
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

/// A path with a leading `~` expanded, as mutt does everywhere it
/// takes one.
pub fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(input)
}

pub fn send_via_smtp(account: &Account, text: &str) -> Result<()> {
    let from = compose::from_address(text).context("cannot parse the From address")?;
    let (rcpts, text) = compose::smtp_envelope(text)?;
    anyhow::ensure!(!rcpts.is_empty(), "no recipient addresses");
    let password = account_password(account)?;
    smtp::send(account, &password, &from, &rcpts, text.as_bytes())
}

/// Run a shell command with `bytes` on its stdin.
pub fn pipe_to(command: &str, bytes: &[u8]) -> Result<()> {
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
pub fn run_sendmail(
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

pub fn default_from(hostname: &str) -> String {
    if let Ok(email) = std::env::var("EMAIL") {
        return email;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    format!("{user}@{hostname}")
}

/// neomutt's $abort_noattach_regex default: the words that make a
/// draft look like it should have carried a file.
const DEFAULT_ATTACH_KEYWORD: &str = r"\b(attach|attached|attaching|attachment|attachments)\b";

/// mutt's $abort_noattach_regex, compiled; a bad one warns and the
/// default stands.
fn attach_re_from_config(config: &Config, warnings: &mut Vec<String>) -> regex_lite::Regex {
    let spec = config
        .mail
        .attach_keyword
        .clone()
        .unwrap_or_else(|| DEFAULT_ATTACH_KEYWORD.to_string());
    match regex_lite::Regex::new(&format!("(?i){spec}")) {
        Ok(re) => re,
        Err(err) => {
            warnings.push(format!("bad attach_keyword {spec:?}: {err}"));
            default_attach_re()
        }
    }
}

fn default_attach_re() -> regex_lite::Regex {
    regex_lite::Regex::new(&format!("(?i){DEFAULT_ATTACH_KEYWORD}")).expect("the default compiles")
}

/// mutt's $quote_regexp, compiled; a bad one warns and the default
/// stands.
fn quote_re_from_config(config: &Config, warnings: &mut Vec<String>) -> regex_lite::Regex {
    match &config.pager.quote_regexp {
        Some(spec) => match regex_lite::Regex::new(spec) {
            Ok(re) => re,
            Err(err) => {
                warnings.push(format!("bad quote_regexp {spec:?}: {err}"));
                default_quote_re()
            }
        },
        None => default_quote_re(),
    }
}

/// What the pager needs from the config: the header rules and the
/// auto_view filter table.
fn display_from_config(config: &Config) -> message::Display {
    // [pager] ignore/unignore/hdr_order override the classic
    // five-header view field by field; entries are lowercased and
    // hdr_order accepts mutt's trailing colons.
    let mut rules = message::HeaderRules::default();
    let clean = |list: &Vec<String>| {
        list.iter()
            .map(|n| n.trim_end_matches(':').to_lowercase())
            .collect::<Vec<_>>()
    };
    if let Some(list) = &config.pager.ignore {
        rules.ignore = clean(list);
    }
    if let Some(list) = &config.pager.unignore {
        rules.unignore = clean(list);
    }
    if let Some(list) = &config.pager.hdr_order {
        rules.order = clean(list);
    }
    // auto_view types with no command of their own take one from
    // mailcap, where mutt looks too; a type with no copiousoutput
    // entry there simply does not autoview, and shows as an
    // attachment stub.
    let mut filters: HashMap<String, String> = HashMap::new();
    let mut mailcap: Option<Vec<rmut_core::mailcap::Entry>> = None;
    for (mimetype, command) in &config.filters {
        let mimetype = mimetype.to_lowercase();
        if !command.trim().is_empty() {
            filters.insert(mimetype, command.clone());
            continue;
        }
        let entries = mailcap.get_or_insert_with(rmut_core::mailcap::load);
        if let Some(command) = rmut_core::mailcap::command_for(entries, &mimetype) {
            filters.insert(mimetype, command);
        }
    }
    message::Display {
        filters,
        rules,
        reflow: config.pager.reflow_text.unwrap_or(true),
        alternative_order: config
            .pager
            .alternative_order
            .iter()
            .map(|t| t.to_lowercase())
            .collect(),
    }
}

/// Compile a hook table, dropping (with a warning) any entry whose
/// pattern does not parse or whose value is empty.
fn compile_hooks<'a>(
    what: &str,
    entries: impl Iterator<Item = (&'a String, &'a String)>,
    warnings: &mut Vec<String>,
) -> Vec<Hook> {
    let mut out = Vec::new();
    for (spec, value) in entries {
        if value.trim().is_empty() {
            warnings.push(format!("{what} {spec:?} has nothing to do"));
            continue;
        }
        match pattern::parse(spec) {
            Ok(patterns) => out.push(Hook {
                patterns,
                value: value.clone(),
            }),
            Err(err) => warnings.push(format!("bad {what} pattern {spec:?}: {err}")),
        }
    }
    out
}

/// First value of a (single-line) header in a draft head block.
pub fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_string())
    })
}

/// The draft as a full message: the file as edited, with any withheld
/// header block put back in front.
pub fn draft_full(c: &Compose) -> std::io::Result<String> {
    let text = std::fs::read_to_string(&c.path)?;
    Ok(match &c.hidden_head {
        Some(head) => format!("{}\n\n{}", head.trim_end(), text),
        None => text,
    })
}

/// mutt's $quote_regexp default.
pub fn default_quote_re() -> regex_lite::Regex {
    regex_lite::Regex::new(r"^([ \t]*[|>:}#])+").expect("default quote_regexp compiles")
}

pub fn write_draft(text: &str) -> Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub fn subject_key_strips_reply_prefixes() {
        assert_eq!(subject_key("Re: Re: Lunch"), "lunch");
        assert_eq!(subject_key("FWD: re: x"), "x");
        assert_eq!(subject_key("Redo"), "redo");
    }

    #[test]
    pub fn wrap_order_visits_everything_once() {
        assert_eq!(
            wrap_order(4, 1, true),
            vec![(2, false), (3, false), (0, true), (1, true)]
        );
        assert_eq!(
            wrap_order(4, 1, false),
            vec![(0, false), (3, true), (2, true), (1, true)]
        );
        assert_eq!(wrap_order(0, 0, true), vec![]);
        assert_eq!(wrap_order(1, 0, true), vec![(0, true)]);
    }
}
