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
use rmut_core::remote;
use rmut_core::{alias, command, compose, maildir, message, pgp};
use rmut_session::{
    Session, SortKey, ThreadOp, account_password, default_from, expand_tilde, parse_sort, pipe_to,
    run_sendmail, send_via_smtp, wrap_order,
};

use crate::keymap::{IndexAction, Keymap, PagerAction, parse_key, parse_sequence};
use crate::theme::Theme;

/// neomutt's $abort_noattach_regex default: the words that make a
/// draft look like it should have carried a file.
const DEFAULT_ATTACH_KEYWORD: &str = r"\b(attach|attached|attaching|attachment|attachments)\b";

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

#[derive(Clone, Copy)]
pub enum LineKind {
    Limit,
    Search,
    /// The same, the other way round (mutt's search-reverse): `n`
    /// keeps going backwards afterwards.
    SearchBack,
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
    /// The same, opened read-only (mutt's Esc c).
    ChangeDirReadOnly,
    /// A shell command to run with the TUI stood down (mutt's !).
    Shell,
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
    /// The kinds whose answer names a mailbox, so `=x` / `+x` expand
    /// before anything reads it. Same list as the "mailbox" history
    /// bucket below.
    fn takes_mailbox(self) -> bool {
        matches!(
            self,
            LineKind::ChangeDir
                | LineKind::ChangeDirReadOnly
                | LineKind::SaveMsg
                | LineKind::CopyMsg
                | LineKind::BrowseDir
                | LineKind::CreateDir
                | LineKind::EditFcc
        )
    }

    fn history_bucket(self) -> &'static str {
        match self {
            LineKind::Limit
            | LineKind::Search
            | LineKind::PagerSearch
            | LineKind::SearchBack
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
            | LineKind::ChangeDirReadOnly
            | LineKind::SaveMsg
            | LineKind::CopyMsg
            | LineKind::BrowseDir
            | LineKind::CreateDir
            | LineKind::EditFcc => "mailbox",
            LineKind::SavePart | LineKind::AttachFile => "file",
            LineKind::Pipe | LineKind::PipePart | LineKind::Shell => "command",
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
    /// $abort_noattach = ask: the body mentions an attachment and
    /// none is attached. Send it anyway?
    NoAttach,
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

/// A message that has been sent but is waiting out $undo_send before
/// it goes anywhere. The finalized text is ready to transmit; the
/// draft it came from is kept whole, so cancelling puts the compose
/// menu back exactly as it was.
struct Held {
    state: Compose,
    text: String,
    /// The Fcc target as it was decided at send time (menu, then
    /// fcc-hook); None means the default sent copy.
    fcc: Option<String>,
    /// What the status line calls it: the subject, or the recipients.
    label: String,
    due: Instant,
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
    /// Compiled $quote_regexp classifying quoted body lines.
    pub(crate) quote_re: regex_lite::Regex,
    /// Compiled $abort_noattach_regex, for the attachment reminder.
    attach_re: regex_lite::Regex,
    /// Compiled [[color_body]] rules: regex + style, in config order.
    pub(crate) body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
    /// Set while a `;`-prefixed operation is being asked about (the
    /// mailbox, the command, the y/n): the answer applies to the
    /// tagged set, not to the message under the cursor.
    tag_op: bool,
    /// Messages sent but still inside their $undo_send window, oldest
    /// first. They go out when the timer runs out or rmut exits.
    outbox: Vec<Held>,
    /// Trouble from the send at exit, printed once the terminal is
    /// back (nobody would see a status line by then).
    pub exit_notes: Vec<String>,
    /// Width and content rows from the last key dispatch, for actions
    /// (prompt submissions) that arrive without a size at hand.
    view_size: (usize, usize),
    pub theme: Theme,
    pub keymap: Keymap,
    /// Compiled hook tables (patterns plus the line or mailbox each
    /// carries).
    message_hooks: Vec<Hook>,
    reply_hooks: Vec<Hook>,
    fcc_hooks: Vec<Hook>,
    crypt_hooks: Vec<(pattern::Matcher, String)>,
    /// The config as it was before the active message-hooks changed
    /// it, so leaving the message puts every setting back.
    hook_base: Option<Box<Config>>,
    /// Which message-hooks are in force right now, by index; a change
    /// here is what triggers restore-and-reapply.
    active_message_hooks: Vec<usize>,
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
    /// The attachment reminder has been answered for this draft: the
    /// next send goes through without asking again.
    attach_confirmed: bool,
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

/// mutt's $quote_regexp default.
pub(crate) fn default_quote_re() -> regex_lite::Regex {
    regex_lite::Regex::new(r"^([ \t]*[|>:}#])+").expect("default quote_regexp compiles")
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

/// A compiled hook: the pattern its message must match, and what the
/// hook carries (an enter-command line, or an Fcc mailbox).
struct Hook {
    patterns: Vec<Pattern>,
    value: String,
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

/// Everything the running app derives from [`Config`]: compiled rules,
/// the theme, and the key tables. Kept in one place so `:` commands can
/// rebuild it after changing the config, exactly as startup built it.
struct Derived {
    theme: Theme,
    keymap: Keymap,
    message_hooks: Vec<Hook>,
    reply_hooks: Vec<Hook>,
    fcc_hooks: Vec<Hook>,
    crypt_hooks: Vec<(pattern::Matcher, String)>,
    index_rules: Vec<(Vec<Pattern>, ratatui::style::Style)>,
    body_rules: Vec<(regex_lite::Regex, ratatui::style::Style)>,
    quote_re: regex_lite::Regex,
    /// $abort_noattach_regex: what counts as mentioning an attachment.
    attach_re: regex_lite::Regex,
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
        let attach_re = {
            let spec = config
                .mail
                .attach_keyword
                .clone()
                .unwrap_or_else(|| DEFAULT_ATTACH_KEYWORD.to_string());
            match regex_lite::Regex::new(&format!("(?i){spec}")) {
                Ok(re) => re,
                Err(err) => {
                    warnings.push(format!("bad attach_keyword {spec:?}: {err}"));
                    regex_lite::Regex::new(&format!("(?i){DEFAULT_ATTACH_KEYWORD}"))
                        .expect("the default compiles")
                }
            }
        };
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
        (
            Derived {
                theme,
                keymap,
                message_hooks: compile_hooks(
                    "message-hook",
                    config
                        .message_hooks
                        .iter()
                        .map(|h| (&h.pattern, &h.command)),
                    &mut warnings,
                ),
                reply_hooks: compile_hooks(
                    "reply-hook",
                    config.reply_hooks.iter().map(|h| (&h.pattern, &h.command)),
                    &mut warnings,
                ),
                fcc_hooks: compile_hooks(
                    "fcc-hook",
                    config.fcc_hooks.iter().map(|h| (&h.pattern, &h.mailbox)),
                    &mut warnings,
                ),
                crypt_hooks: config
                    .crypt_hooks
                    .iter()
                    .map(|h| (pattern::Matcher::new(&h.address), h.key.clone()))
                    .collect(),
                index_rules,
                body_rules,
                quote_re,
                attach_re,
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
            message_hooks,
            reply_hooks,
            fcc_hooks,
            crypt_hooks,
            index_rules,
            body_rules,
            quote_re,
            attach_re,
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
            quote_re,
            attach_re,
            body_rules,
            tag_op: false,
            outbox: Vec::new(),
            exit_notes: Vec::new(),
            view_size: (80, 24),
            theme,
            keymap,
            message_hooks,
            reply_hooks,
            fcc_hooks,
            crypt_hooks,
            hook_base: None,
            active_message_hooks: Vec::new(),
            compose_setup: None,
            compose: None,
            pending_editor: None,
            pending_raw_edit: None,
            bounce_to: None,
            alias_addr: None,
            attach_edit: None,
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
            attach_confirmed: false,
            tag_next: false,
            quit: false,
        };
        app.session.install_notices(Box::new(notices));
        if !all.is_empty() {
            app.note(all.join("; "));
        }
        app
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

    pub fn open(dir: &Path, config: Config) -> Result<Self> {
        let (session, warnings) = Session::open(dir, config)?;
        Ok(App::new(session, warnings))
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
            self.sync_message_hooks();
            self.tick_outbox();
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
        self.flush_outbox();
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
            let count = match remote::parse_spec(&spec) {
                Some((account, folder)) => match &mut self.session.remote {
                    Some(remote) if remote.account.name == account => remote.unseen(folder),
                    _ => 0, // other accounts: no connection just for a count
                },
                None => maildir::new_count(&expand_tilde(&spec)),
            };
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
                self.session.sort = sort;
                self.session.sort_rev = rev;
                self.session.apply_sort();
                self.note(format!(
                    "sorted by {}{}",
                    sort.name(),
                    if rev { " (reverse)" } else { "" }
                ));
            }
            KeyKind::Purge { quit } => match code {
                // ask-yes, like mutt's $delete: Enter takes the yes.
                // n writes flag changes but keeps the messages marked
                // deleted; anything else calls the whole thing off,
                // including the quit that asked.
                KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Enter => {
                    self.session.sync(code != KeyCode::Char('n'));
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
                        self.note("message discarded");
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
                    self.note("reply cancelled");
                }
            },
            KeyKind::NoSubject => match code {
                // ask-yes: Enter aborts, like mutt.
                KeyCode::Char('n') => self.subject_ready(String::new()),
                _ => {
                    self.compose_setup = None;
                    self.error("aborted (no subject)");
                }
            },
            KeyKind::NoAttach => match code {
                // ask-no: Enter goes back to the menu, where `a`
                // attaches the file that was forgotten.
                KeyCode::Char('y') => {
                    self.attach_confirmed = true;
                    self.send_draft();
                }
                _ => {
                    self.note("not sent; a attaches a file");
                    self.open_compose_menu();
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
                        self.note("reply cancelled");
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
                        self.note("forward cancelled");
                    }
                }
            }
            KeyKind::Print => {
                if code == KeyCode::Char('y') {
                    self.session.print_current(self.tag_op);
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
                    self.session.bounce_current(&to, self.tag_op);
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
            LineKind::Limit => {
                let keep = self.session.selected_path();
                if input.is_empty() || input == "all" {
                    self.session.limit = None;
                } else {
                    match pattern::parse(input) {
                        Ok(patterns) => {
                            self.session.resolve_body_terms(&patterns);
                            self.session.limit = Some((input.to_string(), patterns));
                        }
                        Err(err) => {
                            self.error(format!("bad pattern: {err}"));
                            return;
                        }
                    }
                }
                self.session.rebuild_visible(keep);
                if self.session.visible.is_empty() {
                    self.note("no messages match the limit");
                }
            }
            LineKind::Search | LineKind::SearchBack => {
                self.session.search_rev = matches!(kind, LineKind::SearchBack);
                if !input.is_empty() {
                    match pattern::parse(input) {
                        Ok(patterns) => {
                            self.session.resolve_body_terms(&patterns);
                            self.session.last_search = Some(patterns);
                        }
                        Err(err) => {
                            self.error(format!("bad pattern: {err}"));
                            return;
                        }
                    }
                }
                self.session.search_next();
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
            LineKind::DeletePattern => self.session.apply_pattern(input, "deleted", |m| {
                if !m.env.file.flags.deleted {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
            }),
            LineKind::UndeletePattern => self.session.apply_pattern(input, "undeleted", |m| {
                if m.env.file.flags.deleted {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                }
            }),
            LineKind::TagPattern => self
                .session
                .apply_pattern(input, "tagged", |m| m.env.tagged = true),
            LineKind::UntagPattern => self
                .session
                .apply_pattern(input, "untagged", |m| m.env.tagged = false),
            LineKind::ChangeDir => self.open_mailbox_spec(input),
            LineKind::ChangeDirReadOnly => self.open_mailbox_read_only(input),
            LineKind::Shell => {
                if !input.is_empty() {
                    self.pending_shell = Some(input.to_string());
                }
            }
            LineKind::BrowseDir => self.browse_dir(input),
            LineKind::CreateDir => self.create_maildir(input),
            LineKind::SavePart => self.save_part(input),
            LineKind::SaveMsg => self.session.copy_message(input, true, self.tag_op),
            LineKind::CopyMsg => self.session.copy_message(input, false, self.tag_op),
            LineKind::Pipe => self.session.pipe_message(input, self.tag_op),
            LineKind::PipePart => self.pipe_part(input),
            LineKind::Query => self.run_query(input),
            LineKind::Notmuch => self.notmuch_search(input),
            LineKind::EnterCommand => self.run_command_line(input),
            LineKind::BounceTo => self.bounce_to_submitted(input),
            LineKind::AttachFile => self.attach_file_submitted(input),
            LineKind::AliasNick => {
                if let Some(addr) = self.alias_addr.take() {
                    self.session.create_alias(input, &addr);
                }
            }
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
        // Any new action ends a previous `;` operation, including one
        // whose prompt was abandoned with Esc.
        self.tag_op = false;
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
                self.tag_op = apply_tagged;
                self.prompt_copy(action == IndexAction::Save);
            }
            IndexAction::Pipe => {
                self.tag_op = apply_tagged;
                self.prompt_pipe();
            }
            IndexAction::Bounce => {
                self.tag_op = apply_tagged;
                self.prompt_bounce();
            }
            IndexAction::Resend => self.resend_current(),
            IndexAction::Edit => self.start_raw_edit(),
            IndexAction::CreateAlias => self.prompt_create_alias(),
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
                    self.prompt_purge(true);
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
                    self.prompt_purge(false);
                } else {
                    self.session.sync(true);
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
                    .session
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
            IndexAction::SearchReverse => {
                self.prompt = Some(Prompt::line(
                    "Reverse search: ",
                    String::new(),
                    LineKind::SearchBack,
                ));
            }
            IndexAction::SearchNext => self.session.search_next(),
            IndexAction::NextNew => self.session.jump_new(true),
            IndexAction::PrevNew => self.session.jump_new(false),
            IndexAction::DeletePattern => {
                if !self.session.deny_readonly() {
                    self.prompt = Some(Prompt::line(
                        "Delete messages matching: ",
                        String::new(),
                        LineKind::DeletePattern,
                    ));
                }
            }
            IndexAction::UndeletePattern => {
                if !self.session.deny_readonly() {
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
                self.prompt = Some(Prompt::line(
                    "Shell command: ",
                    String::new(),
                    LineKind::Shell,
                ));
            }
            IndexAction::Redraw => self.redraw = true,
            IndexAction::Suspend => self.pending_suspend = true,
            IndexAction::Folders => self.open_folder_browser(),
            IndexAction::Print => {
                self.tag_op = apply_tagged;
                self.confirm_print();
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
                self.pending_suspend = true;
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
                self.error(format!("notmuch: {}", err.trim()));
                return;
            }
            Err(err) => {
                self.error(format!("notmuch: {err}"));
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let files: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        if files.is_empty() {
            self.error("notmuch: no matches");
            return;
        }
        if !self.session.ready_to_leave() {
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
            self.note(format!("notmuch mirror: {err:#}"));
            return;
        }
        let count = files.len();
        self.open_mailbox_spec(&dir.display().to_string());
        if self.session.dir == dir {
            // The virtual mailbox: never write through the symlinks.
            self.session.read_only = true;
            self.session.title = format!("notmuch: {query}");
            self.note(format!("{count} matching message(s)"));
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
    fn open_mailbox_read_only(&mut self, spec: &str) {
        self.open_mailbox_spec(spec);
        self.session.read_only = true;
        self.note(format!("{} (read-only)", self.session.title));
    }

    fn open_mailbox_spec(&mut self, spec: &str) {
        self.session.mark_old_unread();
        // Message-hook settings belong to the message being left, not
        // to the config the new mailbox inherits.
        self.clear_message_hooks();
        // Another folder of the open account reuses the live session
        // (a SELECT) instead of a fresh connect+login round; a dead
        // session falls through to the full open below.
        if let Some((account_name, mailbox)) = remote::parse_spec(spec)
            && self
                .session
                .remote
                .as_ref()
                .is_some_and(|r| r.account.name == account_name)
            && self
                .session
                .remote
                .as_mut()
                .unwrap()
                .switch(mailbox)
                .is_ok()
        {
            let remote = self.session.remote.take().unwrap();
            match App::open(&remote.cache.clone(), self.session.config.clone()) {
                Ok(mut app) => {
                    app.session.title = remote.spec.clone();
                    if let Ok(password) = account_password(&remote.account) {
                        app.session.idle = Some(remote::idle_watch(
                            &remote.account,
                            &remote.mailbox,
                            &password,
                        ));
                    }
                    app.session.remote = Some(remote);
                    app.session.read_only_session = self.session.read_only_session;
                    app.session.read_only = self.session.read_only_session;
                    app.sidebar_visible = self.sidebar_visible;
                    app.refresh_sidebar();
                    *self = app;
                    self.run_folder_hooks();
                }
                Err(err) => self.error(format!("cannot open {spec}: {err:#}")),
            }
            return;
        }
        match App::open_spec(spec, self.session.config.clone()) {
            Ok(mut app) => {
                // The runtime sidebar toggle survives a mailbox switch,
                // and so does a -R session: it is not a property of the
                // mailbox that was open.
                app.session.read_only_session = self.session.read_only_session;
                app.session.read_only = self.session.read_only_session;
                app.sidebar_visible = self.sidebar_visible;
                app.refresh_sidebar();
                *self = app;
                self.run_folder_hooks();
            }
            Err(err) => self.error(format!("cannot open {spec}: {err:#}")),
        }
    }

    // ---- compose ----

    fn compose_base(&self) -> Option<ComposeBase> {
        let &mi = self.session.visible.get(self.session.sel)?;
        let env = &self.session.msgs[mi].env;
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
                    self.error("no message selected");
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
        if self.session.config.mail.autoedit && self.edit_headers() {
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
            .find(|a| self.session.lists.iter().any(|m| m.is_match(a)))
    }

    /// mutt's $followup_to: mail going to a known list carries a
    /// Mail-Followup-To, so replies land on the list. Being subscribed
    /// leaves my own address out, since the list copy is the one I get.
    fn followup_header(&self, to: &str, cc: Option<&str>, from: &str) -> Option<String> {
        if self.session.lists.is_empty() {
            return None;
        }
        let mut rcpts = compose::addresses(to);
        rcpts.extend(compose::addresses(cc.unwrap_or_default()));
        if !rcpts
            .iter()
            .any(|a| self.session.lists.iter().any(|m| m.is_match(a)))
        {
            return None;
        }
        let subscribed = rcpts
            .iter()
            .any(|a| self.session.subscribed.iter().any(|m| m.is_match(a)));
        let value = compose::followup_to(
            to,
            cc.unwrap_or_default(),
            self.session.me(),
            subscribed,
            from,
        );
        (!value.is_empty()).then_some(value)
    }

    /// `L`: reply to the mailing list. Refuses when the message names
    /// no list rmut knows of, rather than quietly replying to the
    /// author, which is the mistake list-reply exists to prevent.
    fn start_list_reply(&mut self) {
        let Some(base) = self.compose_base() else {
            self.error("no message selected");
            return;
        };
        if self.list_target(&base).is_none() {
            self.error(if self.session.lists.is_empty() {
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
        if self.session.config.mail.fast_reply
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
        if self.session.config.mail.fast_reply && !subject_prefill.is_empty() {
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
        let ask_fwd = self.session.config.mail.forward.as_deref() == Some("ask")
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
        // mutt's reply-hook: in force while this reply's draft is
        // built, so `set from`, edit_headers and my_hdr all see it.
        let reply_hooks = self.apply_reply_hooks(setup.base.as_ref(), setup.kind);
        self.finish_compose_draft(setup, subject, include);
        self.restore_after_reply_hooks(reply_hooks);
    }

    fn finish_compose_draft(&mut self, setup: ComposeSetup, subject: &str, include: bool) {
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
                                self.session.me(),
                                self.session.config.mail.metoo,
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
            Err(err) => self.error(format!("cannot write draft: {err:#}")),
        }
    }

    /// mutt's edit_headers (default false, like mutt): whether the
    /// header block is part of the editor buffer.
    fn edit_headers(&self) -> bool {
        self.session.config.mail.edit_headers.unwrap_or(false)
    }

    /// Write a fresh draft file for the editor: the whole text (with
    /// any `my_hdr` merged in), or (with edit_headers = false) only
    /// the body, the header block withheld for draft_full to rejoin.
    fn stage_draft(&self, text: &str) -> Result<(PathBuf, Option<String>)> {
        // mutt's my_hdr lands here, so every draft the TUI opens
        // carries it: with edit_headers the editor shows the lines,
        // without it they ride along in the withheld head.
        let text = compose::apply_my_hdr(text, &self.session.config.mail.my_hdr);
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
        if self.session.config.identity.reverse_name
            && let Some(b) = base
            && let Some(from) = compose::reverse_from(&b.orig_to, &b.orig_cc, self.session.me())
        {
            return Some(from);
        }
        let rcpts = compose::addresses(to);
        self.session.current_identity(&rcpts).from_line()
    }

    fn forward_attaches(&self) -> bool {
        self.session.config.mail.forward.as_deref() == Some("attach")
    }

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
                self.compose = Some(compose);
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
                .session
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
            None if self.session.config.mail.copy == Some(false) => "(no copy)".into(),
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

    /// The sent copy's default target, as the Fcc line shows it. An
    /// fcc-hook matching the draft on screen wins, so the menu shows
    /// where the copy is really going.
    fn default_fcc(&self) -> String {
        if let Some(compose) = &self.compose
            && let Ok(full) = draft_full(compose)
            && let Some(mailbox) = self.fcc_hook_target(&full)
        {
            return mailbox;
        }
        match &self.session.remote {
            Some(remote) => format!(
                "imap:{}/{}",
                remote.account.name, remote.account.sent_folder
            ),
            None => self.session.config.mail.sent.clone().unwrap_or_default(),
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
            self.error("only Attach: files can be edited");
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
                self.error(format!("{input} is not a file"));
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
                    self.error(format!("cannot attach: {err}"));
                }
            }
        }
        self.open_compose_menu();
    }

    /// Initial security for a fresh draft, from the [pgp] config.
    fn default_security(&self) -> Security {
        match (
            self.session.config.pgp.sign_by_default,
            self.session.config.pgp.encrypt_by_default,
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
                self.error(format!("cannot read draft: {err}"));
                return;
            }
        };
        // neomutt's $abort_noattach: the body says "attached" and
        // nothing is. Asked once per draft; an answered draft sends.
        if !mem::take(&mut self.attach_confirmed) && self.attachment_forgotten(&raw, &compose_state)
        {
            self.compose = Some(compose_state);
            match self.session.config.mail.abort_noattach.as_deref() {
                // neomutt's "yes" aborts outright: attach the file,
                // or take the word out of the body.
                Some("yes") => {
                    self.error("no attachment: not sent (abort_noattach); a attaches one");
                    self.open_compose_menu();
                }
                _ => {
                    self.prompt = Some(Prompt::Key {
                        label:
                            "The body mentions an attachment and none is attached. Send? (y/n): "
                                .into(),
                        kind: KeyKind::NoAttach,
                    });
                }
            }
            return;
        }
        // mutt's fcc-hook, evaluated on the draft as it stands after
        // the editor; an Fcc picked in the menu still wins.
        let hook_fcc = self.fcc_hook_target(&raw);
        let (raw, files) = compose::extract_attachments(&raw);
        let host = maildir::hostname();
        let from = self
            .session
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
                self.compose = Some(compose_state);
                self.open_compose_menu();
                return;
            }
        };
        let original = match &compose_state.attach {
            Some(path) => match std::fs::read(path) {
                Ok(bytes) => Some(bytes),
                Err(err) => {
                    self.error(format!("cannot attach the original: {err}"));
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
                self.error(format!("{err:#}; e edits, s changes security"));
                self.compose = Some(compose_state);
                self.open_compose_menu();
                return;
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
            due: Instant::now() + Duration::from_secs(self.session.config.mail.undo_send),
        };
        // $undo_send: the message waits, and z takes it back.
        if self.session.config.mail.undo_send > 0 {
            self.note(format!(
                "sending {} in {}s (z cancels)",
                held.label, self.session.config.mail.undo_send
            ));
            self.outbox.push(held);
            return;
        }
        self.deliver(held);
    }

    /// Anything due in the outbox goes out; whatever still waits owns
    /// the status line, counting down.
    fn tick_outbox(&mut self) {
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
    pub fn flush_outbox(&mut self) {
        while !self.outbox.is_empty() {
            let held = self.outbox.remove(0);
            let label = held.label.clone();
            self.deliver(held);
            if let Some(err) = self.notice().filter(|n| n.is_error()).map(|n| n.text()) {
                self.exit_notes.push(format!("{label}: {err}"));
                self.clear_notice();
            }
        }
    }

    /// Take the newest held message back to its compose menu, draft
    /// and all. Returns false when nothing was waiting.
    /// mutt has no undo; rmut's walks back the last step. A message
    /// still inside its $undo_send window is the most recent thing
    /// done, so it is what undo takes back first.
    fn undo_last(&mut self) {
        if self.cancel_send() {
            return;
        }
        self.session.undo_last();
    }

    fn cancel_send(&mut self) -> bool {
        let Some(held) = self.outbox.pop() else {
            return false;
        };
        self.note(format!("send cancelled: {}", held.label));
        self.compose = Some(held.state);
        self.open_compose_menu();
        true
    }

    /// Transmit a held message and keep the Fcc copy.
    fn deliver(&mut self, held: Held) {
        let Held {
            state: compose_state,
            text: final_text,
            fcc: chosen,
            ..
        } = held;
        let send_result = match self.session.smtp_account() {
            Some(account) => send_via_smtp(&account, &final_text),
            None => run_sendmail(
                final_text.as_bytes(),
                self.session.config.mail.sendmail.as_deref(),
                None,
            ),
        };
        match send_result {
            Ok(()) => {
                let mut note = String::from("message sent");
                let skip_copy = chosen.as_deref() == Some("")
                    || (chosen.is_none() && self.session.config.mail.copy == Some(false));
                if skip_copy {
                    // Nothing kept, on request.
                } else if let Some(fcc) = chosen
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
                    match &mut self.session.remote {
                        // Fcc goes to the account's Sent folder on the
                        // server.
                        Some(remote) => match remote.append_sent(final_text.as_bytes()) {
                            Ok(folder) => note += &format!(", copy in {folder}"),
                            Err(_) => note += ", Fcc to Sent failed",
                        },
                        None => {
                            let sent_dir = self
                                .session
                                .config
                                .mail
                                .sent
                                .as_deref()
                                .map(expand_tilde)
                                .filter(|p| p.join("cur").is_dir())
                                .or_else(|| {
                                    maildir::find_special(&self.session.dir, &["sent", "sent-mail"])
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
                self.compose = Some(compose_state);
                self.open_compose_menu();
            }
        }
    }

    /// neomutt's attachment reminder: does the body mention one when
    /// nothing is attached? Quoted lines and anything below a `-- `
    /// signature do not count, so a reply to "see attached" and a
    /// signature naming one are not false alarms.
    fn attachment_forgotten(&self, raw: &str, compose: &Compose) -> bool {
        if self
            .session
            .config
            .mail
            .abort_noattach
            .as_deref()
            .unwrap_or("no")
            == "no"
        {
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
    fn secure_message(
        &self,
        security: Security,
        text: String,
        files: &[compose::Attachment],
        original: Option<&[u8]>,
    ) -> Result<String> {
        let cfg = &self.session.config.pgp;
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
        let flowed = self.session.config.mail.text_flowed;
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

    fn postponed_dir(&self) -> Option<PathBuf> {
        self.session
            .config
            .mail
            .postponed
            .as_deref()
            .map(expand_tilde)
            .filter(|p| p.join("cur").is_dir())
            .or_else(|| {
                maildir::find_special(
                    &self.session.dir,
                    &["drafts", "postponed", "rmut-postponed"],
                )
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
                let d = self.session.dir.join(".rmut-postponed");
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

    /// Recall a postponed draft: straight into the editor when there
    /// is one, a picker when there are several.
    fn recall_postponed(&mut self) {
        let files = self
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
            Err(err) => self.error(format!("cannot recall: {err:#}")),
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
            Err(err) => return self.error(err),
        };
        let sort_before = (
            self.session.config.index.sort.clone(),
            self.session.config.index.sort_aux.clone(),
        );
        let sidebar_before = self.session.config.sidebar.visible;
        let mut reports = Vec::new();
        for cmd in &commands {
            let outcome = match cmd {
                command::Command::Bind { .. } | command::Command::Macro { .. } => {
                    self.bind_command(cmd)
                }
                command::Command::Alias { nick, expansion } => self.alias_command(nick, expansion),
                command::Command::Push(seq) => self.push_command(seq),
                command::Command::Exec(function) => self.exec_command(function),
                config_command => command::apply(&mut self.session.config, config_command),
            };
            match outcome {
                Ok(Some(text)) => reports.push(text),
                Ok(None) => {}
                Err(err) => return self.error(err),
            }
        }
        if commands.is_empty() {
            return;
        }
        // A `:set trash="=Trash"` names a mailbox too.
        self.session.config.expand_folders();
        reports.extend(self.recompile());
        if sort_before
            != (
                self.session.config.index.sort.clone(),
                self.session.config.index.sort_aux.clone(),
            )
        {
            if let Some(spec) = self.session.config.index.sort.clone()
                && let Some((sort, rev)) = parse_sort(&spec)
            {
                self.session.sort = sort;
                self.session.sort_rev = rev;
            }
            self.session.apply_sort();
        }
        if self.session.config.sidebar.visible != sidebar_before {
            self.sidebar_visible = self.session.config.sidebar.visible;
        }
        if !reports.is_empty() {
            self.note(reports.join("; "));
        }
    }

    // ---- hooks (folder-hook, message-hook, reply-hook, fcc-hook) ----

    /// mutt's folder-hook: every entry whose glob matches the mailbox
    /// just opened runs its command line, in config order. Like mutt,
    /// nothing is undone on the way out, so a catch-all entry is how
    /// you put a setting back.
    pub fn run_folder_hooks(&mut self) {
        if self.session.config.folder_hooks.is_empty() {
            return;
        }
        let title = self.session.title.clone();
        let lines: Vec<String> = self
            .session
            .config
            .folder_hooks
            .iter()
            .filter(|h| rmut_core::config::glob_match(&h.folder, &title))
            .map(|h| h.command.clone())
            .collect();
        for line in lines {
            self.run_hook("folder-hook", &line);
        }
    }

    /// mutt's message-hook: the lines matching the selected message
    /// are in force while it is selected, and the config goes back to
    /// what it was as soon as the match set changes. Cheap when
    /// nothing matches, so the draw loop can call it every frame.
    fn sync_message_hooks(&mut self) {
        if self.message_hooks.is_empty() && self.active_message_hooks.is_empty() {
            return;
        }
        let matching = self.matching_message_hooks();
        if matching == self.active_message_hooks {
            return;
        }
        self.active_message_hooks = matching.clone();
        // Back to the pre-hook config first: a hook that no longer
        // matches must leave no trace.
        if let Some(base) = self.hook_base.take() {
            self.session.config = *base;
            let warnings = self.recompile();
            if !warnings.is_empty() {
                self.error(warnings.join("; "));
            }
        }
        if matching.is_empty() {
            return;
        }
        self.hook_base = Some(Box::new(self.session.config.clone()));
        for i in matching {
            let Some(line) = self.message_hooks.get(i).map(|h| h.value.clone()) else {
                continue;
            };
            self.run_hook("message-hook", &line);
        }
    }

    /// Put back whatever the active message-hooks changed, and forget
    /// them: for leaving the mailbox, where the config carries over.
    fn clear_message_hooks(&mut self) {
        self.active_message_hooks.clear();
        if let Some(base) = self.hook_base.take() {
            self.session.config = *base;
            let warnings = self.recompile();
            if !warnings.is_empty() {
                self.error(warnings.join("; "));
            }
        }
    }

    /// Indices of the message-hooks the selected message matches.
    fn matching_message_hooks(&self) -> Vec<usize> {
        let Some(env) = self
            .session
            .visible
            .get(self.session.sel)
            .map(|&mi| &self.session.msgs[mi].env)
        else {
            return Vec::new();
        };
        // `~m` and `~=` want the whole list; a hook asks about one
        // message, so only its own numbering is filled in.
        let pos = pattern::Position {
            number: self.session.sel + 1,
            current: self.session.sel + 1,
            last: self.session.visible.len(),
            duplicate: false,
        };
        self.message_hooks
            .iter()
            .enumerate()
            .filter(|(_, h)| pattern::matches_in(&h.patterns, env, self.session.scope(pos), None))
            .map(|(i, _)| i)
            .collect()
    }

    /// mutt's reply-hook: the lines matching the message being replied
    /// to, applied for as long as the reply's draft is being built.
    /// Returns the config to put back, or None when nothing matched.
    fn apply_reply_hooks(
        &mut self,
        base: Option<&ComposeBase>,
        kind: ComposeKind,
    ) -> Option<Box<Config>> {
        if self.reply_hooks.is_empty()
            || !matches!(
                kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            )
        {
            return None;
        }
        let path = &base?.path;
        let env = self
            .session
            .msgs
            .iter()
            .find(|m| m.env.file.path == *path)
            .map(|m| &m.env)?;
        let scope = self.session.scope(pattern::Position::default());
        let lines: Vec<String> = self
            .reply_hooks
            .iter()
            .filter(|h| pattern::matches_in(&h.patterns, env, scope, None))
            .map(|h| h.value.clone())
            .collect();
        if lines.is_empty() {
            return None;
        }
        let saved = Box::new(self.session.config.clone());
        for line in lines {
            self.run_hook("reply-hook", &line);
        }
        Some(saved)
    }

    /// Undo `apply_reply_hooks`.
    fn restore_after_reply_hooks(&mut self, saved: Option<Box<Config>>) {
        let Some(saved) = saved else { return };
        self.session.config = *saved;
        let warnings = self.recompile();
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
    }

    /// mutt's fcc-hook: the mailbox the first matching entry names for
    /// this outgoing draft, or None when nothing matches.
    fn fcc_hook_target(&self, draft: &str) -> Option<String> {
        if self.fcc_hooks.is_empty() {
            return None;
        }
        let path = self
            .compose
            .as_ref()
            .map(|c| c.path.clone())
            .unwrap_or_default();
        let env = compose::draft_envelope(draft, &path);
        let scope = self.session.scope(pattern::Position::default());
        self.fcc_hooks
            .iter()
            .find(|h| pattern::matches_in(&h.patterns, &env, scope, None))
            .map(|h| h.value.clone())
    }

    /// mutt's crypt-hook: a recipient with a hook of its own is
    /// encrypted to that key id instead of to its address.
    fn crypt_key_for(&self, address: &str) -> Option<String> {
        self.crypt_hooks
            .iter()
            .find(|(m, _)| m.is_match(address))
            .map(|(_, key)| key.clone())
    }

    /// Run one hook's command line, naming the hook when it fails so
    /// it is clear where a bad line came from.
    fn run_hook(&mut self, what: &str, line: &str) {
        self.clear_notice();
        self.run_command_line(line);
        if let Some(err) = self.notice().filter(|n| n.is_error()).map(|n| n.text()) {
            self.error(format!("{what}: {err}"));
        }
    }

    /// Recompile the config-derived state (theme, key tables, color
    /// rules, quote regexp, header rules) and hand back any warnings.
    fn recompile(&mut self) -> Vec<String> {
        let (derived, warnings) = Derived::from_config(&self.session.config);
        self.theme = derived.theme;
        self.keymap = derived.keymap;
        self.session.recompile();
        self.message_hooks = derived.message_hooks;
        self.reply_hooks = derived.reply_hooks;
        self.fcc_hooks = derived.fcc_hooks;
        self.crypt_hooks = derived.crypt_hooks;
        self.index_rules = derived.index_rules;
        self.body_rules = derived.body_rules;
        self.quote_re = derived.quote_re;
        self.attach_re = derived.attach_re;
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

    fn prompt_copy(&mut self, delete: bool) {
        if self.session.visible.get(self.session.sel).is_none() {
            return;
        }
        // Save marks the original deleted; a plain copy is fine.
        if delete && self.session.deny_readonly() {
            return;
        }
        let buf = self.session.config.mail.save.clone().unwrap_or_default();
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

    /// Offer the selected message's sender for the alias file, with
    /// the address's local part as the suggested nick.
    fn prompt_create_alias(&mut self) {
        let Some(&i) = self.session.visible.get(self.session.sel) else {
            return;
        };
        let path = self.session.msgs[i].env.file.path.clone();
        let Some(from) = message::first_header(&path, "From") else {
            self.error("the message has no From header");
            return;
        };
        let nick = compose::bare_address(&from)
            .and_then(|a| a.split('@').next().map(|l| l.to_lowercase()))
            .unwrap_or_default();
        self.alias_addr = Some(from.trim().to_string());
        self.prompt = Some(Prompt::line("Alias as (nick): ", nick, LineKind::AliasNick));
    }

    fn prompt_pipe(&mut self) {
        if self.session.visible.get(self.session.sel).is_none() {
            return;
        }
        self.prompt = Some(Prompt::line(
            "Pipe to command: ",
            String::new(),
            LineKind::Pipe,
        ));
    }

    fn prompt_bounce(&mut self) {
        if self.session.visible.get(self.session.sel).is_none() {
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
            self.note("no recipients, bounce cancelled");
            return;
        }
        self.bounce_to = Some(to.clone());
        let n = self.session.op_targets(self.tag_op).len();
        self.prompt = Some(Prompt::Key {
            label: match n {
                1 => format!("Bounce message to {to}? (y/n): "),
                _ => format!("Bounce {n} messages to {to}? (y/n): "),
            },
            kind: KeyKind::Bounce,
        });
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
        match &mut self.session.remote {
            Some(remote) => {
                // Like mutt on IMAP: the edited copy is appended and
                // the original marked deleted, purged on the next $.
                let flags = self.session.msgs[self.session.visible[self.session.sel]]
                    .env
                    .file
                    .flags;
                let mailbox = remote.mailbox.clone();
                if let Err(err) = remote.append_to(&mailbox, flags, &edited) {
                    self.error(format!("cannot store the edited copy: {err:#}"));
                    return;
                }
                if let Some(m) = self.session.cur_mut() {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
                self.check_new_mail();
                self.note("edited copy appended; original marked deleted ($ purges)");
            }
            None => {
                if let Err(err) = std::fs::write(&path, &edited) {
                    self.error(format!("cannot write message: {err}"));
                    return;
                }
                self.session.rescan();
                self.note("message edited");
            }
        }
    }

    /// Open a copy of the message as a new draft (mutt's resend): its
    /// To/Cc/Subject and body prefill the editor, then the normal send
    /// prompt takes over.
    fn resend_current(&mut self) {
        // Completes a partial IMAP file so the body is really there.
        if self.session.full_message_bytes().is_none() {
            return;
        }
        let Some(base) = self.compose_base() else {
            self.error("no message selected");
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

    fn confirm_print(&mut self) {
        if self.session.visible.get(self.session.sel).is_none() {
            return;
        }
        let n = self.session.op_targets(self.tag_op).len();
        self.prompt = Some(Prompt::Key {
            label: match n {
                1 => "Print message? (y/n): ".to_string(),
                _ => format!("Print {n} messages? (y/n): "),
            },
            kind: KeyKind::Print,
        });
    }

    /// Write all pending changes to the maildir: T-flagged messages are
    /// removed, other dirty messages are renamed with their new flags.
    /// For an IMAP mailbox the changes go to the server first (UID
    /// STORE / EXPUNGE); the local pass then updates the cache to match.
    fn prompt_purge(&mut self, quit: bool) {
        // mutt's $delete: yes purges without asking, no keeps the
        // marks, and the ask default is the question below.
        match self.session.config.mail.delete.as_deref() {
            Some("yes") | Some("no") => {
                let purge = self.session.config.mail.delete.as_deref() == Some("yes");
                self.session.sync(purge);
                if quit {
                    self.quit = true;
                }
            }
            _ => {
                self.prompt = Some(Prompt::Key {
                    label: format!(
                        "Purge {} deleted message(s)? (y/n): ",
                        self.session.deleted_count()
                    ),
                    kind: KeyKind::Purge { quit },
                });
            }
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
