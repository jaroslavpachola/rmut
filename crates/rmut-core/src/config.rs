//! TOML configuration from $RMUT_CONFIG or ~/.config/rmut/config.toml.
//!
//! ```toml
//! [identity]
//! name = "Jane Doe"
//! email = "jane@example.com"
//! reverse_name = false  # reply From = the address the mail came to
//!
//! [[identities]]        # conditional identity (folder-/send-hook)
//! folder = "*work*"     # glob on the open mailbox, and/or:
//! recipient = "*@work.example.com"   # glob on a draft recipient
//! name = "Jane Work"
//! email = "jane@work.example.com"
//!
//! [mail]
//! alternates = ["jane@old\\.example\\.com"]  # my other addresses
//! my_hdr = ["Organization: Acme"]        # on every draft
//! mailboxes = ["~/Maildir", "~/Maildir/.Sent"]
//! sent = "~/Maildir/.Sent"
//! postponed = "~/Maildir/.Drafts"
//! sendmail = "/usr/sbin/sendmail"
//! editor = "vim"
//! poll_seconds = 5
//! print = "lpr"
//!
//! [index]
//! format = "%4C %Z %-6d %-15.15L (%?l?%4l&%4c?) %s"
//!
//! [ui]
//! theme = "default"   # or "mono"
//!
//! [colors]            # overrides: status_fg status_bg deleted flagged header
//! deleted = "red"
//!
//! [keys.index]        # action = key, e.g. sync = "w", delete = "ctrl+d"
//! [keys.pager]
//!
//! [[accounts]]        # remote account, opened as imap:name/FOLDER
//! name = "work"
//! user = "jane@example.com"
//! password_command = "pass show mail/work"   # or: password = "..."
//! imap_host = "imap.example.com"   # imap_port = 993, imap_tls = true
//! smtp_host = "smtp.example.com"   # smtp_port = 587, smtp_tls = true
//! sent_folder = "Sent"             # Fcc target via IMAP APPEND
//!
//! [[folder_hooks]]      # mutt's folder-hook, any `:` command line
//! folder = "*work*"
//! command = "set index_format=\"%4C %Z %-6d %-20.20F %s\""
//!
//! [[message_hooks]]     # applied while the message is selected
//! pattern = "~f boss@example.com"
//! command = "set pager_context=5"
//!
//! [[reply_hooks]]       # applied while a reply to it is built
//! pattern = "~t @work.example.com"
//! command = "set from=jane@work.example.com"
//!
//! [[fcc_hooks]]         # where the sent copy goes
//! pattern = "~t @work.example.com"
//! mailbox = "~/Maildir/.WorkSent"
//!
//! [[crypt_hooks]]       # encrypt to this key for this recipient
//! address = "boss@example.com"
//! key = "0xDEADBEEF"
//!
//! [pgp]
//! command = "gpg"              # runs via $PATH; passphrases come from
//! sign_key = "jane@example.com"  # the gpg agent, never from rmut
//! sign_by_default = false
//! encrypt_by_default = false
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub identity: Identity,
    pub mail: Mail,
    pub index: Index,
    pub pager: Pager,
    pub ui: Ui,
    pub net: Net,
    pub sidebar: Sidebar,
    pub colors: HashMap<String, String>,
    /// Pattern → color rules for index lines, evaluated in order.
    pub color_index: Vec<ColorRule>,
    /// Regex → color rules for pager body spans (mutt's `color body`),
    /// applied in order; `pattern` here is a plain regex, not a
    /// message pattern.
    pub color_body: Vec<ColorRule>,
    /// MIME type → shell command that renders the part (stdin → stdout),
    /// e.g. "text/html" = "w3m -dump -T text/html", mutt's auto_view.
    /// Applied to matching parts wherever they sit in the message,
    /// preferred in multipart/alternative, and used in the attachment
    /// viewer. An empty command means mutt's arrangement: the command
    /// comes from mailcap, from the first `copiousoutput` entry for
    /// the type; a type with no such entry simply does not autoview.
    pub filters: HashMap<String, String>,
    pub keys: Keys,
    /// Macros: a key that replays a sequence of keys, per menu:
    /// [macros.index] / [macros.pager], `key = "sequence"`. The
    /// sequence is literal characters plus `<enter>`/`<esc>`/
    /// `<ctrl+x>`/... names in angle brackets; it feeds the input
    /// queue, so it can drive prompts.
    pub macros: Keys,
    pub accounts: Vec<Account>,
    /// Conditional identities, applied in order over `identity` when
    /// their globs match: the minimal folder-hook / send-hook.
    pub identities: Vec<IdentityRule>,
    /// mutt's folder-hook with an arbitrary command: enter-command
    /// lines run when a matching mailbox is opened.
    pub folder_hooks: Vec<FolderHook>,
    /// mutt's message-hook: lines applied while a matching message is
    /// the selected one, and taken back off when it stops matching.
    pub message_hooks: Vec<MessageHook>,
    /// mutt's reply-hook: lines applied while a reply to a matching
    /// message is built.
    pub reply_hooks: Vec<MessageHook>,
    /// mutt's fcc-hook: where a matching outgoing message's copy goes.
    pub fcc_hooks: Vec<FccHook>,
    /// mutt's crypt-hook: the PGP key to encrypt to for a recipient.
    pub crypt_hooks: Vec<CryptHook>,
    pub pgp: Pgp,
}

/// One `[[folder_hooks]]` entry: mutt's folder-hook. Like mutt, a
/// folder-hook is not undone when you leave the mailbox, so a
/// catch-all entry (`folder = "*"`) is the way to put a setting back.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FolderHook {
    /// Glob (`*`) on the opened mailbox: a path or an `imap:` spec.
    pub folder: String,
    /// One enter-command line, e.g. `set index_format="%s"`.
    pub command: String,
}

/// One `[[message_hooks]]` or `[[reply_hooks]]` entry: a message
/// pattern and the enter-command line it runs.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageHook {
    pub pattern: String,
    pub command: String,
}

/// One `[[fcc_hooks]]` entry: the mailbox a matching outgoing
/// message's copy goes to (a local maildir path).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FccHook {
    /// Message pattern matched against the draft being sent.
    pub pattern: String,
    pub mailbox: String,
}

/// One `[[crypt_hooks]]` entry: encrypt to `key` for any recipient
/// address matching `address` (a case-insensitive regex).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CryptHook {
    pub address: String,
    pub key: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
    /// mutt's reverse_name: a reply's From becomes whichever of your
    /// addresses the original was sent to.
    pub reverse_name: bool,
    /// mutt's $reverse_realname: the display name comes over with the
    /// address reverse_name found. True unless set otherwise, as in
    /// mutt; false keeps the configured name and takes the address
    /// alone.
    pub reverse_realname: Option<bool>,
}

/// One `[[identities]]` entry. With both globs set, both must match;
/// with neither, it always applies. Unset name/email keep the value
/// from the layer below.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct IdentityRule {
    /// Glob (`*`) on the open mailbox: a path or an `imap:` spec.
    pub folder: Option<String>,
    /// Glob (`*`) on any recipient address of the draft.
    pub recipient: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Mail {
    /// mutt's $folder: where mailboxes live, so `=x` and `+x` name a
    /// mailbox under it, at a prompt or in a macro. An IMAP account
    /// spec works too ("imap:work"), making `=Archive` mean
    /// imap:work/Archive.
    pub folder: Option<String>,
    pub mailboxes: Vec<String>,
    pub sent: Option<String>,
    pub postponed: Option<String>,
    pub sendmail: Option<String>,
    pub editor: Option<String>,
    pub poll_seconds: Option<u64>,
    /// Shell command the printed message is piped to (default lpr).
    pub print: Option<String>,
    /// Default target offered by `s` (save message to a mailbox).
    pub save: Option<String>,
    /// "inline" (quoted text, the default) or "attach" (the original
    /// goes along as a message/rfc822 part, mutt's mime_forward).
    pub forward: Option<String>,
    /// mutt's query_command: external address lookup for Tab
    /// completion at the To prompt (`%s` = the word, or appended),
    /// e.g. "khard email --parsable %s".
    pub query_command: Option<String>,
    /// mutt's $trash: purged messages move here (a maildir path, or
    /// `imap:account/folder` of the open account) instead of being
    /// erased; purging inside the trash itself deletes for real.
    pub trash: Option<String>,
    /// mutt's edit_headers (default false, like mutt): true puts the
    /// draft's header block (From/To/Cc/Subject, Attach: lines) into
    /// the editor buffer. Off, the prompts set To/Subject and
    /// attachments go through the compose menu's a.
    pub edit_headers: Option<bool>,
    /// notmuch(1) search (`X`): false disables the key; unset/true
    /// leaves it on (notmuch itself must be installed; keep the
    /// database fresh with `notmuch new` in a hook or cron).
    pub notmuch: Option<bool>,
    /// mutt's $fast_reply: replies skip the To and Subject prompts,
    /// forwards skip Subject (the ask-yes questions still run).
    pub fast_reply: bool,
    /// mutt's $quit: "yes" (the default) leaves at once, "no"
    /// refuses, "ask-yes" and "ask-no" ask first.
    pub quit: Option<String>,
    /// mutt's $confirmappend: ask before adding messages to a mailbox
    /// that already exists. Off by default, where mutt asks: rmut has
    /// never asked, and a save is one keystroke either way.
    pub confirmappend: bool,
    /// mutt's $save_name: the default save target is the sender's
    /// local part under $folder, when such a mailbox exists.
    pub save_name: bool,
    /// mutt's $force_name: the same, whether or not it exists.
    pub force_name: bool,
    /// mutt's $mark_old: unread mail left behind in the mailbox ages
    /// to old (O in the index, out of the new count) when you leave.
    /// True unless set otherwise, as in mutt.
    pub mark_old: Option<bool>,
    /// mutt's $print: what `p` does. "ask-no" (the default) asks with
    /// Enter declining, "ask-yes" asks with Enter printing, "yes"
    /// prints without asking and "no" refuses to print at all.
    pub print_confirm: Option<String>,
    /// mutt's $alias_file: where aliases are read from and where
    /// create-alias appends. $RMUT_ALIASES, then
    /// ~/.config/rmut/aliases, when unset.
    pub alias_file: Option<String>,
    /// mutt's $attribution: the line a quoted reply opens with, over
    /// the message being replied to (%a address, %n name, %f the From
    /// header, %s subject, %i message-id, %d date, %{...} strftime).
    pub attribution: Option<String>,
    /// mutt's $indent_string: what each quoted line is prefixed with,
    /// `"> "` by default.
    pub indent_string: Option<String>,
    /// mutt's $forward_format: the subject a forward carries, the
    /// same format string over the message being forwarded.
    pub forward_format: Option<String>,
    /// mutt's $reply_regexp: what a reply's subject may already start
    /// with ("Re:" with an optional [n], by default). Replying takes
    /// it off and puts "Re: " on, so prefixes never pile up; a
    /// locale's own prefixes go in as `^(re|aw|sv):[ \t]*`.
    /// Case-insensitive unless the regex has an uppercase letter, as
    /// mutt compiles it.
    pub reply_regexp: Option<String>,
    /// mutt's $include: quote the original in a reply? "ask-yes" (the
    /// default) and "ask-no" ask, "yes" and "no" decide it.
    pub include: Option<String>,
    /// mutt's $forward_quote: the forwarded text inside the
    /// "----- Forwarded message" markers is quoted with
    /// $indent_string, the way a reply is.
    pub forward_quote: bool,
    /// mutt's $signature: a file whose contents end every new draft,
    /// or, when the name ends in `|`, a command whose output does.
    /// `~` is expanded. Unset (the default) appends nothing.
    pub signature: Option<String>,
    /// mutt's $sig_dashes: the signature is introduced by a line
    /// holding "-- ". True unless set otherwise, as in mutt.
    pub sig_dashes: Option<bool>,
    /// mutt's $abort_nosubject: a draft with an empty subject.
    /// "ask-yes" (the default) asks with Enter aborting, "ask-no"
    /// asks with Enter sending it on, "yes" aborts without asking,
    /// "no" never asks.
    pub abort_nosubject: Option<String>,
    /// mutt's $abort_unmodified: the first editor pass came back with
    /// the body untouched, so the draft is dropped. True unless set
    /// otherwise, as in mutt; only the first edit is checked.
    pub abort_unmodified: Option<bool>,
    /// mutt's $askcc / $askbcc: ask for those recipients when a draft
    /// is started, prefilled with what a group reply worked out.
    pub ask_cc: bool,
    pub ask_bcc: bool,
    /// mutt's $autoedit (needs edit_headers): skip every initial
    /// prompt and question, straight into the editor; the compose
    /// menu follows as usual.
    pub autoedit: bool,
    /// mutt's $copy: false skips the sent copy (Fcc) entirely; an
    /// Fcc set in the compose menu still wins.
    pub copy: Option<bool>,
    /// mutt's `lists`: address patterns naming mailing lists you know
    /// of. They drive `~l`, the `L` list-reply target, and the
    /// Mail-Followup-To rmut sets on mail to a list.
    pub lists: Vec<String>,
    /// mutt's `subscribe`: lists you are on. Subscribed lists count as
    /// known lists too, and a reply to one leaves your own address out
    /// of Mail-Followup-To, so the list copy is the only one you get.
    pub subscribed: Vec<String>,
    /// mutt's `alternates`: regexes matching your other addresses
    /// (aliases, an old domain, a role address). They join the
    /// identity addresses everywhere rmut asks "is this me?": `~p`
    /// and `~P`, the `+`/`T`/`C`/`F` index marks, reverse_name, the
    /// group-reply dedup, and Mail-Followup-To.
    pub alternates: Vec<String>,
    /// mutt's `my_hdr`: header lines added to every draft, e.g.
    /// "Organization: Acme" or "Bcc: me@example.com". One naming a
    /// header rmut already wrote replaces it (so `From:` and
    /// `Reply-To:` win); To/Cc/Bcc gain the address instead.
    pub my_hdr: Vec<String>,
    /// mutt's $metoo: keep your own address among a group reply's
    /// recipients instead of dropping it.
    pub metoo: bool,
    /// mutt's $text_flowed: outgoing text/plain is declared
    /// `format=flowed` and space-stuffed (RFC 3676), so a reader can
    /// rewrap it. The paragraphs themselves come from your editor,
    /// which has to leave a trailing space on a line that continues.
    pub text_flowed: bool,
    /// Shell command run when new mail arrives (neomutt's
    /// new_mail_command): `%f` = the mailbox, `%n` = how many, e.g.
    /// "notify-send 'rmut: %n new in %f'". Fire-and-forget.
    pub new_mail_command: Option<String>,
    /// mutt's $delete (a quadoption): what `$` and quitting do with
    /// messages marked for deletion. "ask" (the default) asks, with
    /// Enter taking the yes; "yes" purges them without asking; "no"
    /// never purges, so the marks stay for a later change of mind.
    pub delete: Option<String>,
    /// neomutt's $abort_noattach: what to do when the body mentions
    /// an attachment and none is attached. "no" (the default) never
    /// checks, "ask" asks before sending, "yes" refuses the send.
    /// neomutt's ask-yes / ask-no both import as "ask".
    pub abort_noattach: Option<String>,
    /// neomutt's $abort_noattach_regex: what counts as mentioning one.
    /// Case-insensitive; the default is
    /// `\b(attach|attached|attaching|attachment|attachments)\b`.
    pub attach_keyword: Option<String>,
    /// Seconds a sent message waits before it actually goes out, so
    /// `z` can take it back (rmut's own; mutt sends at once). 0 is
    /// off. A held message is sent when the timer runs out or when
    /// rmut exits; batch sends (-s and friends) never hold.
    pub undo_send: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Index {
    pub format: Option<String>,
    /// Initial sort: date/from/subject/size/threads, "reverse-" prefix
    /// allowed (the `o` menu can still change it at runtime).
    pub sort: Option<String>,
    /// "last-date-sent" orders threads by their newest message instead
    /// of the default oldest-first.
    pub sort_aux: Option<String>,
    /// chrono strftime string for the index date column (mutt's
    /// date_format), e.g. "%d.%m.%Y"; default "%b %e", like mutt's
    /// index date.
    pub date_format: Option<String>,
    /// mutt's $collapse_unread (default true, as in mutt): a thread
    /// holding unread mail folds like any other. False leaves those
    /// threads open when everything else folds.
    pub collapse_unread: Option<bool>,
    /// mutt's $uncollapse_jump: unfolding a thread puts the cursor on
    /// its first unread message.
    pub uncollapse_jump: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Pager {
    /// Lines of the message index kept visible above the pager.
    pub index_lines: u16,
    /// Lines of overlap when paging (mutt's pager_context).
    pub context: usize,
    /// mutt's $quote_regexp: classifies quoted body lines (depth =
    /// quote characters in the match). Default `^([ \t]*[|>:}#])+`.
    pub quote_regexp: Option<String>,
    /// mutt's ignore list: header-name prefixes hidden from the brief
    /// view (`*` = all). Unset keeps the classic view (everything
    /// hidden except Date/From/To/Cc/Subject).
    pub ignore: Option<Vec<String>>,
    /// mutt's unignore list: prefixes shown even when ignored.
    pub unignore: Option<Vec<String>>,
    /// mutt's hdr_order: name prefixes sorting the brief view;
    /// unlisted headers follow in message order.
    pub hdr_order: Option<Vec<String>>,
    /// mutt's $pager_format for the pager's bottom line; the default
    /// reproduces the classic "---Message n/m: subject -- NN%".
    pub format: Option<String>,
    /// mutt's $wrap: wrap body text at N columns (negative = a right
    /// margin of |N|); unset wraps at the window width.
    pub wrap: Option<i64>,
    /// mutt's $tilde: pad the rows below end-of-message with ~.
    pub tilde: bool,
    /// mutt's $pager_stop: paging past the end of a message stays
    /// put instead of opening the next one.
    pub pager_stop: bool,
    /// mutt's $markers (default true, as in mutt): the `+` at the
    /// start of a wrapped continuation line.
    pub markers: Option<bool>,
    /// mutt's $smart_wrap (default true, as in mutt): wrapped lines
    /// break at a word boundary rather than at the column.
    pub smart_wrap: Option<bool>,
    /// mutt's $reflow_text (default true): a `format=flowed` part is
    /// put back into paragraphs and wrapped at the display width
    /// instead of keeping the sender's line breaks.
    pub reflow_text: Option<bool>,
    /// mutt's alternative_order: MIME types, most wanted first, that
    /// decide which part of a multipart/alternative shows. `text/*`
    /// wildcards allowed; consulted before the auto_view filters and
    /// the built-in text ranking.
    pub alternative_order: Vec<String>,
}

/// How long to wait on the network before saying so. A mail client
/// that blocks has nothing to draw and no keys to read, so these are
/// short by default: an unreachable server should cost seconds, not
/// the OS default of about two minutes.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Net {
    /// Seconds to wait for a connection (mutt's $connect_timeout).
    /// 0 waits as long as the OS does.
    pub connect_timeout: u64,
    /// Seconds to wait for data on a live connection. Never off:
    /// IMAP IDLE uses it as its heartbeat, and anything under five
    /// seconds is treated as five.
    pub timeout: u64,
}

impl Default for Net {
    fn default() -> Self {
        Net {
            connect_timeout: 10,
            timeout: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub theme: Option<String>,
    /// mutt's status_format for the bottom line (see
    /// format::DEFAULT_STATUS_FORMAT for the specifiers).
    pub status_format: Option<String>,
    /// Ring the terminal bell on error statuses (mutt's $beep).
    pub beep: bool,
    /// mutt's $beep_new: ring it when mail arrives, too. Off by
    /// default, as in mutt.
    pub beep_new: bool,
    /// mutt's $wait_key: a shell escape ends with "Press Enter to
    /// continue", so whatever it printed can be read before the
    /// index paints over it. True unless set otherwise, as in mutt.
    pub wait_key: Option<bool>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            theme: None,
            status_format: None,
            beep: true,
            beep_new: false,
            wait_key: None,
        }
    }
}

/// One `[[color_index]]` rule (mutt's `color index FG BG PATTERN`):
/// index lines whose message matches `pattern` take these colors.
/// First matching rule wins; rules are checked in config order.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ColorRule {
    pub pattern: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
}

/// The optional left pane listing `mail.mailboxes` with new-mail
/// counts (toggle with B at runtime).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Sidebar {
    pub visible: bool,
    pub width: u16,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar {
            visible: false,
            width: 24,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub index: HashMap<String, String>,
    pub pager: HashMap<String, String>,
}

/// One remote account: IMAP for reading, SMTP for sending. The
/// password comes from `password_command` (preferred) or, when you
/// accept a secret sitting in the config file, a literal `password`.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub name: String,
    pub user: String,
    /// Shell command whose first stdout line is the password
    /// (pass(1)-style). Wins over `password` when both are set.
    pub password_command: Option<String>,
    /// Plaintext password. Convenient, but anyone who can read the
    /// config can read your mail, so keep it at mode 600.
    pub password: Option<String>,
    pub imap_host: Option<String>,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// Encrypt IMAP (default): TLS from the first byte on port 993,
    /// STARTTLS on any other port. Disabling is for tests only.
    #[serde(default = "default_true")]
    pub imap_tls: bool,
    pub smtp_host: Option<String>,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Encrypt SMTP (default): implicit TLS on port 465, STARTTLS
    /// otherwise. Disabling is for tests only.
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    /// "password" (default), "xoauth2", or "oauthbearer". The OAuth
    /// mechanisms authenticate with an access token from
    /// `token_command` instead of a password.
    pub auth: Option<String>,
    /// Shell command whose first stdout line is a *fresh* OAuth access
    /// token (refresh is its business: oauth2ms, mutt_oauth2.py, ...).
    /// Run for every connection; tokens expire, so it is never cached.
    pub token_command: Option<String>,
    /// IMAP folder that receives the Fcc copy of sent mail.
    #[serde(default = "default_sent_folder")]
    pub sent_folder: String,
    /// From identity when composing from this account's mailboxes,
    /// e.g. identity = { name = "Jane Work", email = "jane@work.example.com" }.
    pub identity: Option<Identity>,
}

/// PGP via gpg(1). Decrypt/verify happens automatically when a viewed
/// message is PGP; signing and encrypting are chosen at the send
/// prompt. Passphrases are gpg-agent's business; rmut never sees them.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Pgp {
    /// The gpg executable (a name looked up in $PATH or a full path).
    pub command: String,
    /// Signing key for --local-user; gpg's default key when unset.
    pub sign_key: Option<String>,
    /// Preselect signing / encrypting for new drafts (the compose menu's
    /// security menu can still change it per message).
    pub sign_by_default: bool,
    pub encrypt_by_default: bool,
}

impl Default for Pgp {
    fn default() -> Self {
        Pgp {
            command: "gpg".into(),
            sign_key: None,
            sign_by_default: false,
            encrypt_by_default: false,
        }
    }
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

fn default_true() -> bool {
    true
}

fn default_sent_folder() -> String {
    "Sent".into()
}

/// How an account authenticates, from its `auth` key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthKind {
    Password,
    XOAuth2,
    OAuthBearer,
}

impl AuthKind {
    pub fn sasl_name(self) -> &'static str {
        match self {
            AuthKind::Password => "PLAIN",
            AuthKind::XOAuth2 => "XOAUTH2",
            AuthKind::OAuthBearer => "OAUTHBEARER",
        }
    }

    /// The SASL initial response (before base64): RFC 7628 for
    /// OAUTHBEARER, the Google shape for XOAUTH2.
    pub fn initial_response(self, user: &str, token: &str, host: &str, port: u16) -> String {
        match self {
            AuthKind::XOAuth2 => format!("user={user}\x01auth=Bearer {token}\x01\x01"),
            AuthKind::OAuthBearer => {
                format!("n,a={user},\x01host={host}\x01port={port}\x01auth=Bearer {token}\x01\x01")
            }
            AuthKind::Password => String::new(),
        }
    }
}

/// First stdout line of a credential command.
fn first_line_of(command: &str, what: &str, name: &str) -> Result<String> {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("running {what} for account {name}"))?;
    ensure!(
        out.status.success(),
        "{what} for account {name} exited with {}",
        out.status
    );
    let secret = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    ensure!(
        !secret.is_empty(),
        "{what} for account {name} printed nothing"
    );
    Ok(secret)
}

impl Account {
    /// First stdout line of `password_command`, or the stored
    /// `password` when no command is configured.
    pub fn password(&self) -> Result<String> {
        let Some(command) = &self.password_command else {
            return self
                .password
                .clone()
                .filter(|p| !p.is_empty())
                .with_context(|| {
                    format!(
                        "account {} has neither password_command nor password",
                        self.name
                    )
                });
        };
        first_line_of(command, "password command", &self.name)
    }

    pub fn auth_kind(&self) -> Result<AuthKind> {
        match self.auth.as_deref() {
            None | Some("password") => Ok(AuthKind::Password),
            Some("xoauth2") => Ok(AuthKind::XOAuth2),
            Some("oauthbearer") => Ok(AuthKind::OAuthBearer),
            Some(other) => anyhow::bail!("unknown auth {other:?} for account {}", self.name),
        }
    }

    /// The credential matching `auth_kind`: the password, or a fresh
    /// access token from `token_command`.
    pub fn secret(&self) -> Result<String> {
        match self.auth_kind()? {
            AuthKind::Password => self.password(),
            _ => {
                let command = self.token_command.as_deref().with_context(|| {
                    format!(
                        "account {} has auth = oauth but no token_command",
                        self.name
                    )
                })?;
                first_line_of(command, "token command", &self.name)
            }
        }
    }
}

impl Config {
    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }

    /// The identity for a draft, layered like mutt hooks: `[identity]`,
    /// then the account's, then every matching `[[identities]]` rule in
    /// order (a later rule overrides an earlier one; unset fields keep
    /// the value below). `rcpts` are the draft's bare recipient
    /// addresses, empty when they are not known yet, which makes
    /// recipient rules not match.
    /// Every known mailing-list pattern (`lists` plus `subscribed`),
    /// compiled for matching against addresses.
    pub fn list_matchers(&self) -> Vec<crate::pattern::Matcher> {
        self.mail
            .lists
            .iter()
            .chain(&self.mail.subscribed)
            .map(|spec| crate::pattern::Matcher::new(spec))
            .collect()
    }

    /// The `subscribed` half on its own, for the Mail-Followup-To rule.
    pub fn subscribed_matchers(&self) -> Vec<crate::pattern::Matcher> {
        self.mail
            .subscribed
            .iter()
            .map(|spec| crate::pattern::Matcher::new(spec))
            .collect()
    }

    /// mutt's `alternates`, compiled for matching against a bare
    /// address.
    pub fn alternate_matchers(&self) -> Vec<crate::pattern::Matcher> {
        self.mail
            .alternates
            .iter()
            .map(|spec| crate::pattern::Matcher::new(spec))
            .collect()
    }

    pub fn identity_for(
        &self,
        folder: &str,
        rcpts: &[String],
        account: Option<&Account>,
    ) -> Identity {
        let mut id = self.identity.clone();
        let mut overlay = |name: &Option<String>, email: &Option<String>| {
            if name.is_some() {
                id.name = name.clone();
            }
            if email.is_some() {
                id.email = email.clone();
            }
        };
        if let Some(acct) = account.and_then(|a| a.identity.as_ref()) {
            overlay(&acct.name, &acct.email);
        }
        for rule in &self.identities {
            let folder_ok = rule.folder.as_deref().is_none_or(|g| glob_match(g, folder));
            let recipient_ok = rule
                .recipient
                .as_deref()
                .is_none_or(|g| rcpts.iter().any(|r| glob_match(g, r)));
            if folder_ok && recipient_ok {
                overlay(&rule.name, &rule.email);
            }
        }
        id
    }
}

/// Glob match: `*` spans anything, everything else is literal;
/// case-insensitive, anchored at both ends.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            // Backtrack: let the last * swallow one more character.
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// mutt's `+x` / `=x`: a mailbox named under $folder. `=` or `+`
/// alone is $folder itself; anything else, and any name at all when
/// no folder is configured, comes back untouched. This runs on every
/// mailbox rmut is handed, typed or configured, before anything
/// tries to read it as a path or an imap: spec.
pub fn expand_folder(spec: &str, folder: Option<&str>) -> String {
    let Some(rest) = spec.strip_prefix(['=', '+']) else {
        return spec.to_string();
    };
    let Some(folder) = folder
        .map(|f| f.trim_end_matches('/'))
        .filter(|f| !f.is_empty())
    else {
        return spec.to_string();
    };
    match rest.is_empty() {
        true => folder.to_string(),
        false => format!("{folder}/{rest}"),
    }
}

impl Config {
    /// Expand `=x` / `+x` in every mailbox the config names, so the
    /// rest of the program only ever sees real paths and imap: specs.
    /// Idempotent: an expanded name no longer starts with = or +.
    pub fn expand_folders(&mut self) {
        let folder = self.mail.folder.clone();
        let folder = folder.as_deref();
        let one = |slot: &mut Option<String>| {
            if let Some(v) = slot {
                *v = expand_folder(v, folder);
            }
        };
        one(&mut self.mail.sent);
        one(&mut self.mail.postponed);
        one(&mut self.mail.trash);
        one(&mut self.mail.save);
        for m in &mut self.mail.mailboxes {
            *m = expand_folder(m, folder);
        }
        for hook in &mut self.fcc_hooks {
            hook.mailbox = expand_folder(&hook.mailbox, folder);
        }
    }
}

pub fn path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RMUT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/rmut/config.toml"))
}

/// Load the config; a missing file is fine (defaults), a broken file
/// returns defaults plus a warning to show the user.
pub fn load_default() -> (Config, Option<String>) {
    let Some(p) = path() else {
        return (Config::default(), None);
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return (Config::default(), None);
    };
    match toml::from_str::<Config>(&text) {
        Ok(cfg) => {
            let warning = secret_exposed(&cfg, &p);
            (cfg, warning)
        }
        Err(err) => {
            let first = err
                .to_string()
                .lines()
                .next()
                .unwrap_or("parse error")
                .to_string();
            (
                Config::default(),
                Some(format!("config ignored ({}): {first}", p.display())),
            )
        }
    }
}

/// A plaintext `password` in a config anyone can read is the one
/// mistake worth interrupting for: the file holds the keys to the
/// mail. Says so once at startup, and only when the bits are
/// actually open, so a 600 config stays quiet.
fn secret_exposed(cfg: &Config, path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let holds_password = cfg
        .accounts
        .iter()
        .any(|a| a.password.as_ref().is_some_and(|p| !p.is_empty()));
    if !holds_password {
        return None;
    }
    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    if mode & 0o077 == 0 {
        return None;
    }
    // The imperative first: the message line clips at the window
    // edge, and the path is usually the long part.
    Some(format!(
        "chmod 600 {} (it holds a password and others can read it)",
        path.display()
    ))
}

impl Identity {
    /// "Name <email>" / "email" for the From header, if configured.
    pub fn from_line(&self) -> Option<String> {
        match (&self.name, &self.email) {
            (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
            (None, Some(e)) => Some(e.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_readable_config_holding_a_password_warns() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let with_password: Config = toml::from_str(
            r#"
            [[accounts]]
            name = "work"
            user = "jane"
            password = "hunter2"
            "#,
        )
        .unwrap();
        let with_command: Config = toml::from_str(
            r#"
            [[accounts]]
            name = "work"
            user = "jane"
            password_command = "gpg -q -d ~/.config/rmut/imap.gpg"
            "#,
        )
        .unwrap();

        let mode =
            |m: u32| std::fs::set_permissions(&path, std::fs::Permissions::from_mode(m)).unwrap();
        mode(0o644);
        let warning = secret_exposed(&with_password, &path).expect("a warning");
        assert!(warning.starts_with("chmod 600 "), "{warning}");
        // Shut when the bits are shut, and when there is no secret to
        // expose in the first place.
        mode(0o600);
        assert!(secret_exposed(&with_password, &path).is_none());
        mode(0o644);
        assert!(secret_exposed(&with_command, &path).is_none());
        assert!(secret_exposed(&Config::default(), &path).is_none());
    }

    #[test]
    fn folder_shorthand_expands_everywhere_a_mailbox_is_named() {
        assert_eq!(expand_folder("=archive", Some("~/Mail")), "~/Mail/archive");
        assert_eq!(expand_folder("+archive", Some("~/Mail/")), "~/Mail/archive");
        // = or + alone is $folder itself.
        assert_eq!(expand_folder("=", Some("~/Mail")), "~/Mail");
        // An IMAP account works as $folder, so =x is one of its folders.
        assert_eq!(
            expand_folder("=Archive", Some("imap:work")),
            "imap:work/Archive"
        );
        // Nothing to expand, or nowhere to expand to: untouched.
        assert_eq!(expand_folder("~/other", Some("~/Mail")), "~/other");
        assert_eq!(expand_folder("=archive", None), "=archive");
        assert_eq!(expand_folder("=archive", Some("")), "=archive");

        let mut cfg: Config = toml::from_str(
            r#"
            [mail]
            folder = "~/Mail"
            mailboxes = ["=inbox", "~/elsewhere"]
            sent = "+sent"
            trash = "=Trash"
            [[fcc_hooks]]
            pattern = "~A"
            mailbox = "=work"
            "#,
        )
        .unwrap();
        cfg.expand_folders();
        assert_eq!(cfg.mail.mailboxes, ["~/Mail/inbox", "~/elsewhere"]);
        assert_eq!(cfg.mail.sent.as_deref(), Some("~/Mail/sent"));
        assert_eq!(cfg.mail.trash.as_deref(), Some("~/Mail/Trash"));
        assert_eq!(cfg.fcc_hooks[0].mailbox, "~/Mail/work");
        // Idempotent: an expanded name no longer starts with = or +.
        cfg.expand_folders();
        assert_eq!(cfg.mail.trash.as_deref(), Some("~/Mail/Trash"));
    }

    #[test]
    fn parses_partial_config() {
        let cfg: Config = toml::from_str(
            r#"
            [identity]
            name = "Jane"
            email = "jane@x"
            [mail]
            mailboxes = ["~/Maildir"]
            sendmail = "/bin/true"
            [keys.index]
            sync = "w"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.identity.from_line().as_deref(), Some("Jane <jane@x>"));
        assert_eq!(cfg.mail.mailboxes, vec!["~/Maildir"]);
        assert_eq!(cfg.mail.sendmail.as_deref(), Some("/bin/true"));
        assert_eq!(cfg.keys.index.get("sync").map(String::as_str), Some("w"));
        assert!(cfg.ui.theme.is_none());
    }

    #[test]
    fn empty_and_unknown_keys_are_fine() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.identity.from_line().is_none());
        let cfg: Config = toml::from_str("[future]\nx = 1\n").unwrap();
        assert!(cfg.mail.mailboxes.is_empty());
        assert!(cfg.accounts.is_empty());
    }

    #[test]
    fn parses_accounts_with_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [[accounts]]
            name = "work"
            user = "jane@example.com"
            password_command = "pass show mail/work"
            imap_host = "imap.example.com"
            smtp_host = "smtp.example.com"

            [[accounts]]
            name = "test"
            user = "u"
            password_command = "true"
            imap_host = "localhost"
            imap_port = 10143
            imap_tls = false
            smtp_port = 465
            sent_folder = "INBOX/Sent"
            "#,
        )
        .unwrap();
        let work = cfg.account("work").unwrap();
        assert_eq!(work.imap_port, 993);
        assert_eq!(work.smtp_port, 587);
        assert!(work.imap_tls && work.smtp_tls);
        assert_eq!(work.sent_folder, "Sent");
        let test = cfg.account("test").unwrap();
        assert_eq!(test.imap_port, 10143);
        assert!(!test.imap_tls);
        assert!(test.smtp_host.is_none());
        assert_eq!(test.sent_folder, "INBOX/Sent");
        assert!(cfg.account("nope").is_none());
    }

    #[test]
    fn pgp_section_defaults_and_overrides() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.pgp.command, "gpg");
        assert!(cfg.pgp.sign_key.is_none());
        assert!(!cfg.pgp.sign_by_default && !cfg.pgp.encrypt_by_default);
        let cfg: Config = toml::from_str(
            "[pgp]\ncommand = \"gpg2\"\nsign_key = \"jane@x\"\nsign_by_default = true\n",
        )
        .unwrap();
        assert_eq!(cfg.pgp.command, "gpg2");
        assert_eq!(cfg.pgp.sign_key.as_deref(), Some("jane@x"));
        assert!(cfg.pgp.sign_by_default && !cfg.pgp.encrypt_by_default);
    }

    #[test]
    fn account_missing_required_field_fails_parse() {
        assert!(toml::from_str::<Config>("[[accounts]]\nname = \"x\"\n").is_err());
    }

    fn test_account() -> Account {
        Account {
            name: "t".into(),
            user: "u".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: None,
            smtp_port: 587,
            smtp_tls: true,
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
        }
    }

    #[test]
    fn glob_match_star_and_case() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*work*", "/home/jane/Maildir/work-stuff"));
        assert!(glob_match("*@work.example.com", "Jane@Work.Example.Com"));
        assert!(glob_match("imap:work/*", "imap:work/INBOX"));
        assert!(!glob_match("*@work.example.com", "jane@example.com"));
        assert!(!glob_match("work", "workplace")); // anchored
        assert!(glob_match("a*b*c", "aXbYc"));
        assert!(!glob_match("a*b*c", "aXcYb"));
    }

    #[test]
    fn identity_layers_like_hooks() {
        let cfg: Config = toml::from_str(
            r#"
            [identity]
            name = "Jane"
            email = "jane@example.com"
            reverse_name = true

            [[identities]]
            folder = "*work*"
            email = "jane@work.example.com"

            [[identities]]
            recipient = "*@club.example.com"
            name = "Jenny"

            [[accounts]]
            name = "acct"
            user = "u"
            imap_host = "h"
            identity = { name = "Jane Acct", email = "acct@example.com" }
            "#,
        )
        .unwrap();
        assert!(cfg.identity.reverse_name);
        // No match: the global identity as-is.
        let id = cfg.identity_for("~/Maildir", &[], None);
        assert_eq!(id.from_line().as_deref(), Some("Jane <jane@example.com>"));
        // Folder rule overrides the email, keeps the name.
        let id = cfg.identity_for("~/Maildir/work", &[], None);
        assert_eq!(
            id.from_line().as_deref(),
            Some("Jane <jane@work.example.com>")
        );
        // Recipient rule overlays the name; needs a matching recipient.
        let rcpts = vec!["bob@club.example.com".to_string()];
        let id = cfg.identity_for("~/Maildir", &rcpts, None);
        assert_eq!(id.from_line().as_deref(), Some("Jenny <jane@example.com>"));
        let id = cfg.identity_for("~/Maildir", &[], None);
        assert_eq!(id.name.as_deref(), Some("Jane"));
        // The account identity sits between global and the rules.
        let account = cfg.account("acct").unwrap();
        let id = cfg.identity_for("imap:acct/INBOX", &[], Some(account));
        assert_eq!(
            id.from_line().as_deref(),
            Some("Jane Acct <acct@example.com>")
        );
        let id = cfg.identity_for("imap:acct/work", &[], Some(account));
        assert_eq!(
            id.from_line().as_deref(),
            Some("Jane Acct <jane@work.example.com>")
        );
    }

    #[test]
    fn password_command_takes_first_line() {
        let account = |cmd: &str| Account {
            password_command: Some(cmd.into()),
            ..test_account()
        };
        assert_eq!(
            account("printf 'secret\\nrest\\n'").password().unwrap(),
            "secret"
        );
        assert!(account("false").password().is_err());
        assert!(account("true").password().is_err()); // empty output
    }

    #[test]
    fn auth_kinds_and_token_command() {
        let acct = test_account();
        assert_eq!(acct.auth_kind().unwrap(), AuthKind::Password);
        let oauth = Account {
            auth: Some("oauthbearer".into()),
            token_command: Some("printf 'tok123\\nrest\\n'".into()),
            ..test_account()
        };
        assert_eq!(oauth.auth_kind().unwrap(), AuthKind::OAuthBearer);
        assert_eq!(oauth.secret().unwrap(), "tok123");
        let no_command = Account {
            auth: Some("xoauth2".into()),
            ..test_account()
        };
        assert!(
            no_command
                .secret()
                .unwrap_err()
                .to_string()
                .contains("no token_command")
        );
        let bad = Account {
            auth: Some("kerberos".into()),
            ..test_account()
        };
        assert!(bad.auth_kind().is_err());
        // "password" is an explicit spelling of the default.
        let explicit = Account {
            auth: Some("password".into()),
            password: Some("pw".into()),
            ..test_account()
        };
        assert_eq!(explicit.secret().unwrap(), "pw");
    }

    #[test]
    fn oauth_initial_responses() {
        assert_eq!(
            AuthKind::XOAuth2.initial_response("jane", "tok", "imap.example.com", 993),
            "user=jane\x01auth=Bearer tok\x01\x01"
        );
        assert_eq!(
            AuthKind::OAuthBearer.initial_response("jane", "tok", "imap.example.com", 993),
            "n,a=jane,\x01host=imap.example.com\x01port=993\x01auth=Bearer tok\x01\x01"
        );
    }

    #[test]
    fn stored_password_and_precedence() {
        let stored = Account {
            password: Some("hunter2".into()),
            ..test_account()
        };
        assert_eq!(stored.password().unwrap(), "hunter2");
        // A configured command wins over the stored password.
        let both = Account {
            password_command: Some("echo from-command".into()),
            password: Some("hunter2".into()),
            ..test_account()
        };
        assert_eq!(both.password().unwrap(), "from-command");
        let neither = test_account();
        assert!(neither.password().is_err());
        let cfg: Config = toml::from_str(
            "[[accounts]]\nname = \"x\"\nuser = \"u\"\npassword = \"pw\"\nimap_host = \"h\"\n",
        )
        .unwrap();
        assert_eq!(cfg.account("x").unwrap().password().unwrap(), "pw");
    }
}
