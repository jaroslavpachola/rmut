//! Muttrc importer: translate the common muttrc directives into rmut's
//! TOML config. Meant for a one-time `rmut --import-muttrc` run whose
//! output the user reviews and saves; everything that does not map is
//! kept visible as `# not imported:` comments, never dropped silently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Parsed import: the TOML text plus the alias lines found, in
/// muttrc form. rmut reads mutt-format alias files as they are, so
/// these belong in one of those rather than in the TOML; the caller
/// either writes them to the alias file or shows them with
/// `alias_block`.
pub struct Import {
    pub toml: String,
    pub aliases: Vec<String>,
    /// mutt's $alias_file, when the muttrc named one: rmut reads and
    /// appends to it, so an import writes no alias file of its own.
    pub alias_file: Option<String>,
}

pub fn import_file(path: &Path) -> Result<Import> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    Ok(import(&text, dir))
}

/// `dir` anchors relative `source` includes.
pub fn import(text: &str, dir: &Path) -> Import {
    let mut st = State::default();
    parse_into(text, dir, 0, &mut st);
    Import {
        toml: st.to_toml(),
        aliases: st.aliases.clone(),
        alias_file: st.alias_file.clone(),
    }
}

/// An `[[identities]]` rule from a folder-hook / send-hook.
struct IdRule {
    folder: Option<String>,
    recipient: Option<String>,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Default)]
struct State {
    name: Option<String>,
    email: Option<String>,
    reverse_name: bool,
    /// `set noreverse_realname`: keep the configured name.
    no_reverse_realname: bool,
    identity_rules: Vec<IdRule>,
    /// folder-hooks whose command is not a from/realname set: they
    /// become [[folder_hooks]] with the command line kept whole.
    folder_hook_lines: Vec<(String, String)>,
    /// message-hook / reply-hook: (mutt pattern, command line).
    message_hooks: Vec<(String, String)>,
    reply_hooks: Vec<(String, String)>,
    /// fcc-hook / fcc-save-hook: (mutt pattern, mailbox).
    fcc_hooks: Vec<(String, String)>,
    /// crypt-hook: (address pattern, key id).
    crypt_hooks: Vec<(String, String)>,
    /// `set folder`, for expanding the +/= mailbox shortcuts.
    folder: Option<String>,
    spoolfile: Option<String>,
    mailboxes: Vec<String>,
    sent: Option<String>,
    postponed: Option<String>,
    sendmail: Option<String>,
    editor: Option<String>,
    poll_seconds: Option<u64>,
    index_format: Option<String>,
    colors: BTreeMap<&'static str, String>,
    /// `color quoted`/`quotedN` depth palette, keyed by N.
    quoted_colors: BTreeMap<usize, String>,
    /// `color index FG BG PATTERN` rules, in muttrc order.
    color_index_rules: Vec<(String, String, String)>,
    /// `color body FG BG REGEX` rules, in muttrc order.
    color_body_rules: Vec<(String, String, String)>,
    quote_regexp: Option<String>,
    /// ignore/unignore/hdr_order lists for the brief header view.
    hdr_ignore: Vec<String>,
    /// mutt's `lists` and `subscribe`, kept apart: a subscribed list
    /// is also a known list, but only it drops my address from
    /// Mail-Followup-To.
    lists: Vec<String>,
    subscribed: Vec<String>,
    /// mutt's `alternates`: regexes for my other addresses.
    alternates: Vec<String>,
    /// mutt's `my_hdr`, as whole "Name: value" lines, in file order.
    my_hdr: Vec<String>,
    /// `set metoo`: keep my address in a group reply.
    metoo: bool,
    /// `set forward_quote`: the forwarded text comes in quoted.
    forward_quote: bool,
    /// `set signature` and `set nosig_dashes`.
    signature: Option<String>,
    no_sig_dashes: bool,
    sig_on_top: bool,
    hostname: Option<String>,
    user_agent: bool,
    /// mutt's $abort_nosubject and $abort_unmodified, when they are
    /// not what rmut does anyway.
    abort_nosubject: Option<String>,
    no_abort_unmodified: bool,
    /// `set text_flowed`: send text/plain; format=flowed.
    text_flowed: bool,
    /// mutt's $delete quadoption, when it is not the ask default.
    delete: Option<String>,
    /// neomutt's attachment reminder: $abort_noattach (as
    /// no/ask/yes) and $abort_noattach_regex.
    abort_noattach: Option<String>,
    attach_keyword: Option<String>,
    /// `set noreflow_text`: leave a flowed part's line breaks alone.
    no_reflow_text: bool,
    /// `alternative_order`: preferred types in a multipart/alternative.
    alternative_order: Vec<String>,
    hdr_unignore: Vec<String>,
    hdr_order: Vec<String>,
    pager_format: Option<String>,
    wrap: Option<i64>,
    tilde: bool,
    status_format: Option<String>,
    title_format: Option<String>,
    set_title: bool,
    history_file: Option<String>,
    status_on_top: bool,
    arrow_cursor: bool,
    status_chars: Option<String>,
    no_beep: bool,
    /// `set beep_new`: ring for an arrival as well.
    beep_new: bool,
    /// `set nowait_key`: no pause after a shell escape.
    no_wait_key: bool,
    /// `set nomark_old`: unread mail stays new when you leave.
    no_mark_old: bool,
    /// mutt's $print quadoption, when it is not rmut's ask-no.
    print_confirm: Option<String>,
    /// mutt's leaving and filing habits.
    quit: Option<String>,
    postpone: Option<String>,
    recall: Option<String>,
    confirmappend: bool,
    save_name: bool,
    force_name: bool,
    /// mutt's reading habits: $pager_stop, and the three that are on
    /// by default, kept as Option so "yes" stays satisfied.
    pager_stop: bool,
    markers_off: bool,
    smart_wrap_off: bool,
    collapse_unread_off: bool,
    uncollapse_jump: bool,
    hide_thread_subject: bool,
    /// mutt's $attribution, $indent_string and $forward_format, which
    /// rmut takes as they are: the specifiers are the same.
    attribution: Option<String>,
    indent_string: Option<String>,
    reply_regexp: Option<String>,
    simple_search: Option<String>,
    no_wrap_search: bool,
    forward_format: Option<String>,
    /// mutt's $include, $askcc and $askbcc.
    include: Option<String>,
    ask_cc: bool,
    ask_bcc: bool,
    /// mutt's $connect_timeout, in seconds.
    connect_timeout: Option<u64>,
    /// mutt's $certificate_file, a PEM of extra roots; and
    /// $ssl_usesystemcerts = no, which turns the OS store off.
    certificate_file: Option<String>,
    no_system_cas: bool,
    keys_index: BTreeMap<&'static str, String>,
    keys_pager: BTreeMap<&'static str, String>,
    macros_index: BTreeMap<String, String>,
    macros_pager: BTreeMap<String, String>,
    sign_key: Option<String>,
    sign_by_default: bool,
    encrypt_by_default: bool,
    reply_sign: bool,
    reply_encrypt: bool,
    reply_sign_encrypted: bool,
    print: Option<String>,
    query_command: Option<String>,
    trash: Option<String>,
    /// `set edit_headers` (off is mutt's and rmut's default).
    edit_headers_on: bool,
    sort: Option<String>,
    sort_aux: Option<String>,
    date_format: Option<String>,
    pager_index_lines: Option<u64>,
    pager_context: Option<u64>,
    search_context: Option<u64>,
    forward_attach: bool,
    /// mime_forward = ask-yes/ask-no.
    forward_ask: bool,
    fast_reply: bool,
    autoedit: bool,
    /// `set nocopy`: skip the sent copy.
    no_copy: bool,
    new_mail_command: Option<String>,
    save_default: Option<String>,
    filters: BTreeMap<String, String>,
    imap_user: Option<String>,
    imap_pass: Option<String>,
    /// "xoauth2"/"oauthbearer" from *_authenticators.
    oauth: Option<String>,
    smtp_pass: Option<String>,
    smtp_url: Option<String>,
    aliases: Vec<String>,
    skipped: Vec<String>,
    /// Directives that match what rmut always does, acknowledged in
    /// the output so the user knows they were seen, not dropped.
    satisfied: Vec<String>,
    /// Directives rmut answers in its own way: nothing to set, but
    /// worth saying how, since "not imported" would read as a hole.
    differs: Vec<String>,
    sidebar_visible: bool,
    sidebar_width: Option<u16>,
    alias_file: Option<String>,
}

fn parse_into(text: &str, dir: &Path, depth: usize, st: &mut State) {
    for line in logical_lines(text) {
        let tokens = tokenize(&line);
        let Some(cmd) = tokens.first() else {
            continue;
        };
        match cmd.as_str() {
            "set" => {
                for (name, value) in assignments(&tokens[1..]) {
                    st.set(&name, &value, &line);
                }
            }
            "mailboxes" => {
                for t in &tokens[1..] {
                    let m = st.expand_mailbox(t);
                    if !st.mailboxes.contains(&m) {
                        st.mailboxes.push(m);
                    }
                }
            }
            "alias" => st.aliases.push(line.clone()),
            "lists" => {
                for t in &tokens[1..] {
                    if !st.lists.contains(t) {
                        st.lists.push(t.clone());
                    }
                }
            }
            "subscribe" => {
                for t in &tokens[1..] {
                    if !st.subscribed.contains(t) {
                        st.subscribed.push(t.clone());
                    }
                }
            }
            "unlists" | "unsubscribe" => {
                for t in &tokens[1..] {
                    st.lists.retain(|l| l != t);
                    st.subscribed.retain(|l| l != t);
                }
            }
            "alternates" => {
                for t in &tokens[1..] {
                    if !st.alternates.contains(t) {
                        st.alternates.push(t.clone());
                    }
                }
            }
            "unalternates" => {
                for t in &tokens[1..] {
                    if t == "*" {
                        st.alternates.clear();
                    } else {
                        st.alternates.retain(|a| a != t);
                    }
                }
            }
            // The value carries colons and spaces, so it is taken off
            // the raw line rather than from the tokens.
            "my_hdr" => {
                let rest = line["my_hdr".len()..].trim().trim_matches('"').to_string();
                if rest.contains(':') {
                    st.set_my_hdr(rest);
                } else {
                    st.skip(&line, "my_hdr needs a \"Name: value\" header line");
                }
            }
            "unmy_hdr" => {
                for t in &tokens[1..] {
                    if t == "*" {
                        st.my_hdr.clear();
                    } else {
                        let name = t.trim_end_matches(':');
                        st.my_hdr.retain(|h| !header_named(h, name));
                    }
                }
            }
            "ignore" => st.hdr_ignore.extend(tokens[1..].iter().cloned()),
            "unignore" => st.hdr_unignore.extend(tokens[1..].iter().cloned()),
            "hdr_order" => st.hdr_order.extend(
                tokens[1..]
                    .iter()
                    .map(|t| t.trim_end_matches(':').to_string()),
            ),
            "bind" => st.bind(&tokens[1..], &line),
            "macro" => st.mutt_macro(&tokens[1..], &line),
            "color" => st.color(&tokens[1..], &line),
            "auto_view" | "unauto_view" => {
                for mime in &tokens[1..] {
                    st.auto_view(cmd == "auto_view", mime, &line);
                }
            }
            "alternative_order" | "unalternative_order" => {
                for mime in &tokens[1..] {
                    let mime = mime.to_lowercase();
                    if cmd == "alternative_order" {
                        if !st.alternative_order.contains(&mime) {
                            st.alternative_order.push(mime);
                        }
                    } else if mime == "*" {
                        st.alternative_order.clear();
                    } else {
                        st.alternative_order.retain(|t| *t != mime);
                    }
                }
            }
            "folder-hook" | "send-hook" => {
                let folder_hook = cmd == "folder-hook";
                st.hook(folder_hook, &tokens[1..], &line);
            }
            "message-hook" | "reply-hook" => st.command_hook(cmd, &tokens[1..], &line),
            "fcc-hook" | "fcc-save-hook" => st.fcc_hook(cmd, &tokens[1..], &line),
            "crypt-hook" | "pgp-hook" => match &tokens[1..] {
                [address, key] => st.crypt_hooks.push((address.clone(), key.clone())),
                _ => st.skip(&line, "crypt-hook ADDRESS KEYID"),
            },
            "save-hook" => match (tokens.get(1).map(String::as_str), tokens.get(2)) {
                // Expanded at output time; $folder may come later.
                (Some("." | "~A"), Some(mailbox)) => st.save_default = Some(mailbox.clone()),
                _ => st.skip(&line, "only the catch-all pattern . maps to [mail] save"),
            },
            "source" if tokens.len() >= 2 => {
                if depth >= 10 {
                    st.skip(&line, "source nesting too deep");
                    continue;
                }
                let target = expand_path(&tokens[1], dir);
                match std::fs::read_to_string(&target) {
                    Ok(included) => {
                        let sub = target.parent().unwrap_or(dir).to_path_buf();
                        parse_into(&included, &sub, depth + 1, st);
                    }
                    Err(err) => st.skip(&line, &format!("cannot read: {err}")),
                }
            }
            _ => st.skip(&line, "no rmut equivalent"),
        }
    }
}

// ---- muttrc syntax ----

/// Comment-stripped, continuation-joined, non-empty lines.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending = String::new();
    for raw in text.lines() {
        pending.push_str(raw);
        if pending.ends_with('\\') {
            pending.pop();
            continue;
        }
        let line = strip_comment(&pending);
        if !line.trim().is_empty() {
            out.push(line.trim().to_string());
        }
        pending.clear();
    }
    if !pending.trim().is_empty() {
        out.push(strip_comment(&pending).trim().to_string());
    }
    out.retain(|l| !l.is_empty());
    out
}

/// Cut an unquoted `#` comment.
fn strip_comment(line: &str) -> String {
    let mut quote = None;
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (None, '#') => return line[..i].to_string(),
            (None, '\'' | '"') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            _ => {}
        }
    }
    line.to_string()
}

/// Whitespace-separated tokens; quotes group, backslash escapes inside
/// double quotes and bare text (mutt-ish, close enough for configs).
pub(crate) fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    cur.push('\\');
                    cur.push(next);
                    has = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `set` arguments as (name, value) pairs: `a=b`, `a = b`, `a =b`,
/// `a= b`, and bare booleans `a` / `noa`.
pub(crate) fn assignments(tokens: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        if let Some((name, value)) = t.split_once('=') {
            if !name.is_empty() && !value.is_empty() {
                out.push((name.to_string(), value.to_string()));
                i += 1;
            } else if !name.is_empty() {
                // "name=" with the value in the next token.
                out.push((
                    name.to_string(),
                    tokens.get(i + 1).cloned().unwrap_or_default(),
                ));
                i += 2;
            } else {
                i += 1;
            }
        } else if tokens.get(i + 1).map(String::as_str) == Some("=") {
            out.push((t.clone(), tokens.get(i + 2).cloned().unwrap_or_default()));
            i += 3;
        } else if let Some(v) = tokens.get(i + 1).and_then(|n| n.strip_prefix('=')) {
            out.push((t.clone(), v.to_string()));
            i += 2;
        } else if let Some(name) = t.strip_prefix("no") {
            out.push((name.to_string(), "no".into()));
            i += 1;
        } else {
            out.push((t.clone(), "yes".into()));
            i += 1;
        }
    }
    out
}

pub(crate) fn is_yes(value: &str) -> bool {
    matches!(value, "yes" | "ask-yes" | "true" | "1")
}

/// "Jane Doe <jane@x>" or a bare address → (display name, address).
pub(crate) fn split_from(v: &str) -> (Option<String>, String) {
    match v.split_once('<') {
        Some((n, rest)) => {
            let n = n.trim().trim_matches('"');
            (
                (!n.is_empty()).then(|| n.to_string()),
                rest.trim_end_matches('>').trim().to_string(),
            )
        }
        None => (None, v.trim().to_string()),
    }
}

/// A mutt hook regex as an rmut glob, for the easy shapes: literals,
/// `.*` runs, `^`/`$` anchors, `\`-escapes, a `~t`/`~C` recipient
/// prefix, `+`/`=` folder shortcuts. None means too regex-y.
fn hook_glob(pattern: &str) -> Option<String> {
    let p = pattern.trim();
    let p = p
        .strip_prefix("~t ")
        .or_else(|| p.strip_prefix("~C "))
        .unwrap_or(p)
        .trim()
        .trim_start_matches(['+', '=']);
    if p.starts_with('~') || p.starts_with('%') {
        return None;
    }
    if p == "." || p == ".*" {
        return Some("*".into());
    }
    let (p, anchored_start) = match p.strip_prefix('^') {
        Some(rest) => (rest, true),
        None => (p, false),
    };
    let (p, anchored_end) = match p.strip_suffix('$') {
        Some(rest) => (rest, true),
        None => (p, false),
    };
    let mut glob = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => glob.push(chars.next()?),
            '.' if chars.peek() == Some(&'*') => {
                chars.next();
                glob.push('*');
            }
            '(' | ')' | '[' | ']' | '{' | '}' | '|' | '+' | '?' | '$' | '^' | '*' => return None,
            c => glob.push(c),
        }
    }
    if !anchored_start && !glob.starts_with('*') {
        glob.insert(0, '*');
    }
    if !anchored_end && !glob.ends_with('*') {
        glob.push('*');
    }
    Some(glob)
}

/// mutt's $default_hook, "~f %s !~P | (~P ~C %s)": a hook pattern that
/// is a plain address means "from them, unless I sent it, in which
/// case addressed to them". Patterns that already name an operator
/// (or are the catch-all) are left alone.
/// mutt's regexes spell a word edge \\< and \\>; rmut's engine
/// spells it \\b. A quoted muttrc value keeps its doubled
/// backslashes, so both forms are translated.
fn word_boundaries(re: &str) -> String {
    re.replace(r"\\<", r"\b")
        .replace(r"\\>", r"\b")
        .replace(r"\<", r"\b")
        .replace(r"\>", r"\b")
}

/// The alias lines as a comment block, for the printed-for-review
/// form of an import, which writes nothing anywhere.
pub fn alias_block(aliases: &[String], target: &std::path::Path) -> String {
    if aliases.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n# aliases found: rmut reads mutt-format alias files; put these\n\
         # lines in {} (or point $RMUT_ALIASES at them):\n",
        target.display()
    );
    for a in aliases {
        out += &format!("#   {a}\n");
    }
    out
}

fn default_hook_pattern(pattern: &str) -> String {
    let p = pattern.trim();
    if p == "." || p == ".*" {
        return "~A".into();
    }
    if p.starts_with('~') || p.starts_with('!') || p.starts_with('(') || p.contains(" ~") {
        return p.to_string();
    }
    format!("(~f \"{p}\" !~P) | (~P ~C \"{p}\")")
}

fn expand_path(value: &str, dir: &Path) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    let p = PathBuf::from(value);
    if p.is_relative() { dir.join(p) } else { p }
}

// ---- directive handling ----

/// Account name used when the muttrc points at an IMAP server.
const ACCOUNT: &str = "mutt";

/// True when a stored `my_hdr` line carries this header name.
fn header_named(entry: &str, name: &str) -> bool {
    entry
        .split_once(':')
        .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(name))
}

impl State {
    fn skip(&mut self, line: &str, why: &str) {
        self.skipped.push(format!("{line}  ({why})"));
    }

    /// mutt keeps one my_hdr per header name: a later line for the
    /// same header replaces the earlier one.
    fn set_my_hdr(&mut self, entry: String) {
        let Some((name, _)) = entry.split_once(':') else {
            return;
        };
        let name = name.trim().to_string();
        self.my_hdr.retain(|h| !header_named(h, &name));
        self.my_hdr.push(entry);
    }

    fn satisfy(&mut self, line: &str, why: &str) {
        self.satisfied.push(format!("{line}  ({why})"));
    }

    /// Not a setting rmut has, and not a hole either: rmut does the
    /// same thing another way, and the report says which.
    fn differently(&mut self, line: &str, how: &str) {
        self.differs.push(format!("{line}  ({how})"));
    }

    fn set(&mut self, name: &str, value: &str, line: &str) {
        let v = value.to_string();
        match name {
            "realname" => self.name = Some(v),
            "from" => {
                let (n, e) = split_from(&v);
                if let Some(n) = n {
                    self.name.get_or_insert(n);
                }
                self.email = Some(e);
            }
            "metoo" => {
                self.metoo = is_yes(value);
            }
            "delete" => {
                // A quadoption: ask-yes and ask-no are rmut's "ask",
                // which is also its default, so only yes/no are worth
                // carrying over.
                match v.as_str() {
                    "yes" | "no" => self.delete = Some(v),
                    _ => {}
                }
            }
            "abort_noattach" => {
                // A quadoption: ask-yes and ask-no both become "ask".
                self.abort_noattach = Some(
                    match v.as_str() {
                        "yes" => "yes",
                        "no" => "no",
                        _ => "ask",
                    }
                    .to_string(),
                );
            }
            "abort_noattach_regex" => self.attach_keyword = Some(word_boundaries(&v)),
            "text_flowed" => {
                self.text_flowed = is_yes(value);
            }
            "reflow_text" => {
                self.no_reflow_text = !is_yes(value);
            }
            "reverse_name" => {
                if is_yes(&v) {
                    self.reverse_name = true;
                } else {
                    self.satisfy(line, "off is rmut's default");
                }
            }
            "folder" => self.folder = Some(v),
            "spoolfile" => self.spoolfile = Some(v),
            "record" => self.sent = Some(v),
            "postponed" => self.postponed = Some(v),
            "sendmail" => self.sendmail = Some(v),
            "editor" | "visual" => self.editor = Some(v),
            "mail_check" => match v.parse() {
                Ok(n) => self.poll_seconds = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "index_format" => self.index_format = Some(v),
            "pgp_sign_as" | "pgp_default_key" => self.sign_key = Some(v),
            "crypt_autosign" | "pgp_autosign" => self.sign_by_default = is_yes(&v),
            "crypt_autoencrypt" | "pgp_autoencrypt" => self.encrypt_by_default = is_yes(&v),
            "crypt_replysign" => self.reply_sign = is_yes(&v),
            "crypt_replyencrypt" => self.reply_encrypt = is_yes(&v),
            "crypt_replysignencrypted" => self.reply_sign_encrypted = is_yes(&v),
            "assumed_charset" => self.differently(
                line,
                "rmut lets mailparse decode declared charsets; undeclared 8-bit is read as UTF-8",
            ),
            "print_command" => self.print = Some(v),
            "query_command" => self.query_command = Some(v),
            "trash" => self.trash = Some(v),
            "edit_headers" => {
                if is_yes(&v) {
                    self.edit_headers_on = true;
                } else {
                    self.satisfy(line, "off is rmut's default too");
                }
            }
            "status_format" => self.status_format = Some(v),
            "status_chars" => self.status_chars = Some(v),
            "ts_status_format" | "ts_icon_format" => self.title_format = Some(v),
            "history_file" => self.history_file = Some(v),
            "status_on_top" => {
                if is_yes(value) {
                    self.status_on_top = true;
                } else {
                    self.satisfy(line, "the status bar sits at the bottom, as rmut has it");
                }
            }
            "arrow_cursor" => {
                if is_yes(value) {
                    self.arrow_cursor = true;
                } else {
                    self.satisfy(line, "rmut marks the selection with reverse video by default");
                }
            }
            "save_history" | "history" => self.differently(
                line,
                "rmut keeps 100 entries per prompt; set [ui] history_file to persist them",
            ),
            "ts_enabled" => {
                if is_yes(value) {
                    self.set_title = true;
                } else {
                    self.satisfy(line, "rmut leaves the terminal title alone by default");
                }
            }
            "imap_user" => self.imap_user = Some(v),
            "smtp_url" => self.smtp_url = Some(v),
            "imap_pass" => self.imap_pass = Some(v),
            "smtp_pass" => self.smtp_pass = Some(v),
            "certificate_file" | "ssl_ca_certificates_file" => self.certificate_file = Some(v),
            "ssl_usesystemcerts" => {
                if is_yes(&v) {
                    self.satisfy(line, "rmut trusts the OS store by default");
                } else {
                    self.no_system_cas = true;
                }
            }
            "ssl_verify_host" | "ssl_verify_dates" => {
                // rmut always verifies; it has no interactive
                // accept-once, so a `no` here cannot be honoured.
                if is_yes(&v) {
                    self.satisfy(line, "rmut always verifies the certificate");
                } else {
                    self.skip(line, "rmut cannot be told to skip verification");
                }
            }
            "tunnel" => self.skip(line, "rmut has no $tunnel transport yet"),
            "ssl_client_cert" => self.skip(line, "rmut has no client-certificate auth yet"),
            "ssl_starttls" | "ssl_force_tls" => {
                if is_yes(&v) {
                    self.satisfy(line, "rmut always negotiates TLS/STARTTLS");
                } else {
                    self.skip(line, "rmut cannot skip TLS (imap_tls = false is for tests)");
                }
            }
            "charset" | "send_charset" => {
                if v.to_lowercase().replace(['-', '_'], "").contains("utf8") {
                    self.satisfy(line, "rmut is UTF-8 native");
                } else {
                    self.skip(line, "rmut is UTF-8 only");
                }
            }
            "pgp_auto_decode" => {
                if is_yes(&v) {
                    self.satisfy(line, "rmut always decrypts/verifies PGP on view");
                } else {
                    self.skip(line, "rmut always checks PGP; there is no off switch");
                }
            }
            "sort" => {
                let (rev, name) = match v.strip_prefix("reverse-") {
                    Some(rest) => ("reverse-", rest),
                    None => ("", v.as_str()),
                };
                match name {
                    "date" | "date-sent" | "date-received" => {
                        self.sort = Some(format!("{rev}date"));
                    }
                    "threads" => self.sort = Some("threads".into()),
                    "subject" | "size" | "from" | "label" => {
                        self.sort = Some(format!("{rev}{name}"))
                    }
                    _ => self.skip(line, "no matching rmut sort order"),
                }
            }
            "sort_aux" => {
                // rmut takes mutt's spellings as they are: `last-`
                // orders a thread by its newest message, `reverse-`
                // turns the threads round.
                let spec = v.trim().to_lowercase();
                let bare = spec.strip_prefix("reverse-").unwrap_or(&spec);
                let bare = bare.strip_prefix("last-").unwrap_or(bare);
                match bare {
                    "date" | "date-sent" | "date-received" => {
                        if spec == "date" {
                            self.satisfy(line, "threads are ordered oldest-first by default");
                        } else {
                            self.sort_aux = Some(spec);
                        }
                    }
                    _ => self.skip(line, "rmut orders threads by date"),
                }
            }
            "date_format" => {
                // A leading ! toggles the locale in mutt; the format
                // string itself is what matters here.
                self.date_format = Some(v.trim_start_matches('!').to_string());
            }
            "pager_index_lines" => match v.parse() {
                Ok(n) => self.pager_index_lines = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "pager_context" => match v.parse() {
                Ok(n) => self.pager_context = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "search_context" => match v.parse() {
                Ok(n) => self.search_context = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "quote_regexp" => self.quote_regexp = Some(v.to_string()),
            "new_mail_command" => self.new_mail_command = Some(v.to_string()),
            "pager_format" => self.pager_format = Some(v.to_string()),
            "wrap" => match v.parse() {
                Ok(n) => self.wrap = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "tilde" => {
                if is_yes(&v) {
                    self.tilde = true;
                } else {
                    self.satisfy(line, "no tilde padding is rmut's default");
                }
            }
            "mime_forward" => {
                let m = v.to_lowercase();
                if m.starts_with("ask") {
                    self.forward_ask = true;
                } else if is_yes(&v) {
                    self.forward_attach = true;
                } else {
                    self.satisfy(line, "inline forwarding is rmut's default");
                }
            }
            "connect_timeout" => match v.parse::<i64>() {
                // mutt waits for the OS when it is zero or negative.
                Ok(secs) => self.connect_timeout = Some(secs.max(0) as u64),
                Err(_) => self.skip(line, "connect_timeout wants a number"),
            },
            "beep" => {
                if is_yes(&v) {
                    self.satisfy(line, "beeping on errors is rmut's default");
                } else {
                    self.no_beep = true;
                }
            }
            "beep_new" => {
                if is_yes(value) {
                    self.beep_new = true;
                } else {
                    self.satisfy(line, "an arrival rings no bell of its own");
                }
            }
            "wait_key" => {
                if is_yes(value) {
                    self.satisfy(line, "a shell escape waits for Enter already");
                } else {
                    self.no_wait_key = true;
                }
            }
            "mark_old" => {
                if is_yes(value) {
                    self.satisfy(line, "unread mail ages to old on the way out, as in mutt");
                } else {
                    self.no_mark_old = true;
                }
            }
            "print" => {
                // mutt's quadoption. rmut asks with Enter declining,
                // which is mutt's ask-no default.
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "ask-no" => self.satisfy(line, "p asks first, with Enter declining"),
                    "yes" | "no" | "ask-yes" => self.print_confirm = Some(want),
                    _ => self.skip(line, "print wants yes / no / ask-yes / ask-no"),
                }
            }
            "reverse_realname" => {
                if is_yes(value) {
                    self.satisfy(line, "the name comes over with the address");
                } else {
                    self.no_reverse_realname = true;
                }
            }
            "timeout" => self.differently(
                line,
                "rmut polls on a timer of its own, not on an idle keypress; [mail] poll_seconds is the interval",
            ),
            "strict_threads" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut threads by References only, never by subject");
                } else {
                    self.skip(line, "rmut cannot be told to thread by subject");
                }
            }
            "duplicate_threads" => {
                if is_yes(value) {
                    self.differently(
                        line,
                        "rmut gives each copy of a Message-ID its own line; ~= finds them",
                    );
                } else {
                    self.satisfy(line, "rmut keeps duplicate Message-IDs apart");
                }
            }
            "hide_missing" | "hide_top_missing" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut never draws a message it does not have");
                } else {
                    self.skip(line, "rmut cannot draw messages it does not have");
                }
            }
            "narrow_tree" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut's tree is two columns a level already");
                } else {
                    self.differently(line, "rmut's tree is two columns a level, wide or not");
                }
            }
            "quit" => {
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "yes" => self.satisfy(line, "q leaves at once, as in mutt"),
                    "no" | "ask-yes" | "ask-no" => self.quit = Some(want),
                    _ => self.skip(line, "quit wants yes / no / ask-yes / ask-no"),
                }
            }
            "postpone" => {
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "ask-yes" => self.satisfy(line, "leaving a draft asks, as in mutt"),
                    "yes" | "no" | "ask-no" => self.postpone = Some(want),
                    _ => self.skip(line, "postpone wants yes / no / ask-yes / ask-no"),
                }
            }
            "recall" => {
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "ask-yes" | "ask-no" => {
                        self.satisfy(line, "rmut offers new-or-recall when drafts wait")
                    }
                    "yes" | "no" => self.recall = Some(want),
                    _ => self.skip(line, "recall wants yes / no / ask-yes / ask-no"),
                }
            }
            "confirmappend" => {
                if is_yes(value) {
                    self.confirmappend = true;
                } else {
                    self.satisfy(line, "rmut adds to an existing mailbox without asking");
                }
            }
            "save_name" => {
                if is_yes(value) {
                    self.save_name = true;
                } else {
                    self.satisfy(line, "the save prompt offers [mail] save, as rmut always has");
                }
            }
            "force_name" => {
                if is_yes(value) {
                    self.force_name = true;
                } else {
                    self.satisfy(line, "rmut offers a named mailbox only when it exists");
                }
            }
            "move" => {
                if is_yes(value) {
                    self.differently(
                        line,
                        "rmut leaves read mail where it is; a folder-hook with a macro can move it",
                    );
                } else {
                    self.satisfy(line, "rmut leaves read mail where it is");
                }
            }
            "pager_stop" => {
                if is_yes(value) {
                    self.pager_stop = true;
                } else {
                    self.satisfy(line, "paging past the end opens the next message, as in mutt");
                }
            }
            "markers" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut marks wrapped lines with + already");
                } else {
                    self.markers_off = true;
                }
            }
            "smart_wrap" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut wraps at word boundaries already");
                } else {
                    self.smart_wrap_off = true;
                }
            }
            "collapse_unread" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut folds every thread, as mutt does by default");
                } else {
                    self.collapse_unread_off = true;
                }
            }
            "uncollapse_jump" => {
                if is_yes(value) {
                    self.uncollapse_jump = true;
                } else {
                    self.satisfy(line, "unfolding keeps the cursor where it was, as in mutt");
                }
            }
            "hide_thread_subject" => {
                if is_yes(value) {
                    self.hide_thread_subject = true;
                } else {
                    self.satisfy(line, "rmut shows every subject in a thread by default");
                }
            }
            "sidebar_visible" => {
                if is_yes(value) {
                    self.sidebar_visible = true;
                } else {
                    self.satisfy(line, "the sidebar is hidden until B shows it");
                }
            }
            "sidebar_width" => match v.trim().parse::<u16>() {
                Ok(n) => self.sidebar_width = Some(n),
                Err(_) => self.skip(line, "not a number"),
            },
            "sidebar_format" => self.differently(
                line,
                "rmut's sidebar draws the mailbox and its new count, with no format string",
            ),
            "sidebar_short_path" | "sidebar_delim_chars" | "sidebar_folder_indent" => {
                self.differently(line, "rmut's sidebar shows the mailbox as configured")
            }
            "alias_file" => self.alias_file = Some(v),
            "imap_idle" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut IDLEs whenever the server offers it");
                } else {
                    self.skip(line, "rmut cannot be told not to IDLE");
                }
            }
            "imap_keepalive" => self.differently(
                line,
                "rmut re-issues IDLE about every 25 minutes; [mail] poll_seconds is the fallback poll",
            ),
            "header_cache" | "message_cachedir" | "header_cache_backend" => self.differently(
                line,
                "rmut keeps its own header and body cache under ~/.cache/rmut",
            ),
            "crypt_use_gpgme" => {
                self.differently(line, "rmut runs gpg(1) directly, not through GPGME")
            }
            "mailcap_path" => self.differently(
                line,
                "rmut reads the files $MAILCAPS names, or the usual mailcap places",
            ),
            "implicit_autoview" => self.differently(
                line,
                "an empty [filters] command takes the mailcap one, per type",
            ),
            "attribution" => self.attribution = Some(v),
            "indent_string" => self.indent_string = Some(v),
            "reply_regexp" => self.reply_regexp = Some(v),
            "simple_search" => self.simple_search = Some(v),
            "wrap_search" => {
                if is_yes(value) {
                    self.satisfy(line, "search wraps around by default, as in mutt");
                } else {
                    self.no_wrap_search = true;
                }
            }
            "sort_re" => {
                if is_yes(value) {
                    self.satisfy(line, "rmut never threads by subject, so this is moot");
                } else {
                    self.skip(line, "rmut cannot be told to thread by subject");
                }
            }
            "forward_format" => self.forward_format = Some(v),
            "include" => {
                // mutt's quadoption: yes / no / ask-yes / ask-no.
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "yes" | "no" | "ask-yes" | "ask-no" => self.include = Some(want),
                    _ => self.skip(line, "include wants yes / no / ask-yes / ask-no"),
                }
            }
            "forward_quote" => {
                if is_yes(value) {
                    self.forward_quote = true;
                } else {
                    self.satisfy(line, "a forward comes in unquoted, as in mutt");
                }
            }
            "signature" => self.signature = Some(v),
            "sig_on_top" => {
                if is_yes(value) {
                    self.sig_on_top = true;
                } else {
                    self.satisfy(line, "the signature goes below the quote, as in mutt");
                }
            }
            "hostname" => self.hostname = Some(v),
            "use_domain" => self.differently(
                line,
                "rmut takes the Message-ID host from the system name or [mail] hostname",
            ),
            "user_agent" => {
                if is_yes(value) {
                    self.user_agent = true;
                } else {
                    self.satisfy(line, "rmut adds no User-Agent by default");
                }
            }
            "sig_dashes" => {
                if !is_yes(value) {
                    self.no_sig_dashes = true;
                } else {
                    self.satisfy(line, "the \"-- \" line is rmut's default too");
                }
            }
            "abort_nosubject" => {
                let want = v.trim().to_lowercase();
                match want.as_str() {
                    "ask-yes" => self.satisfy(line, "rmut asks, with Enter aborting"),
                    "yes" | "no" | "ask-no" => self.abort_nosubject = Some(want),
                    _ => self.skip(line, "abort_nosubject wants yes / no / ask-yes / ask-no"),
                }
            }
            "abort_unmodified" => {
                if is_yes(value) {
                    self.satisfy(line, "an untouched first edit drops the draft already");
                } else {
                    self.no_abort_unmodified = true;
                }
            }
            "reply_to" => {
                // rmut asks whenever a Reply-To differs from From,
                // which is mutt's ask-yes; the other three would be
                // rmut answering it for you.
                match v.trim().to_lowercase().as_str() {
                    "ask-yes" => self.satisfy(line, "rmut asks before taking a Reply-To"),
                    _ => self.skip(line, "rmut always asks about a Reply-To (mutt's ask-yes)"),
                }
            }
            "honor_followup_to" => {
                if is_yes(value) {
                    self.satisfy(line, "a group reply honors Mail-Followup-To already");
                } else {
                    self.skip(line, "rmut always honors a sender's Mail-Followup-To");
                }
            }
            "askcc" => self.ask_cc = is_yes(value),
            "askbcc" => self.ask_bcc = is_yes(value),
            "fast_reply" => {
                if is_yes(&v) {
                    self.fast_reply = true;
                } else {
                    self.satisfy(line, "prompting is rmut's default");
                }
            }
            "autoedit" => {
                if is_yes(&v) {
                    self.autoedit = true;
                } else {
                    self.satisfy(line, "prompting is rmut's default");
                }
            }
            "copy" => {
                if is_yes(&v) {
                    self.satisfy(line, "the sent copy is rmut's default");
                } else {
                    self.no_copy = true;
                }
            }
            "forward_decode" => {
                if is_yes(&v) {
                    self.satisfy(line, "inline forwards always quote the decoded text");
                } else {
                    self.skip(line, "rmut always decodes when quoting a forward");
                }
            }
            "mime_forward_rest" => {
                self.satisfy(line, "the entire original message is attached");
            }
            "imap_peek" => {
                if is_yes(&v) {
                    self.satisfy(line, "fetches use BODY.PEEK; \\Seen is set only on sync");
                } else {
                    self.skip(line, "rmut never marks messages read while fetching");
                }
            }
            "menu_scroll" => {
                self.satisfy(line, "menus always scroll line-wise");
            }
            "imap_authenticators" | "smtp_authenticators" => {
                let m = v.to_lowercase();
                if m.contains("oauthbearer") {
                    self.oauth = Some("oauthbearer".into());
                } else if m.contains("xoauth2") {
                    self.oauth = Some("xoauth2".into());
                } else if m.contains("plain") || m.contains("login") {
                    self.satisfy(line, "rmut negotiates AUTH PLAIN/LOGIN by itself");
                } else {
                    self.skip(
                        line,
                        "rmut supports PLAIN/LOGIN and OAuth (xoauth2/oauthbearer)",
                    );
                }
            }
            _ => self.skip(line, "no rmut equivalent"),
        }
    }

    /// A folder-hook / send-hook whose command only sets from/realname
    /// becomes an [[identities]] rule, where it layers with the rest
    /// of rmut's identity handling. Any other folder-hook command
    /// becomes a [[folder_hooks]] entry, run as an enter-command line
    /// when the mailbox opens; a send-hook doing something else has
    /// no rmut equivalent and is skipped.
    fn hook(&mut self, folder_hook: bool, args: &[String], line: &str) {
        let [pattern, command] = args else {
            self.skip(line, "unrecognized hook syntax");
            return;
        };
        let Some(glob) = hook_glob(pattern) else {
            self.skip(line, "the pattern does not translate to a glob");
            return;
        };
        let tokens = tokenize(command);
        let only_identity_sets = "only 'set from/realname' send-hooks translate";
        let identity_sets = || -> Option<(Option<String>, Option<String>)> {
            if tokens.first().map(String::as_str) != Some("set") {
                return None;
            }
            let (mut name, mut email) = (None, None);
            for (key, value) in assignments(&tokens[1..]) {
                match key.as_str() {
                    "realname" => name = Some(value),
                    "from" => {
                        let (n, e) = split_from(&value);
                        if name.is_none() {
                            name = n;
                        }
                        email = Some(e);
                    }
                    _ => return None,
                }
            }
            (name.is_some() || email.is_some()).then_some((name, email))
        };
        if let Some((name, email)) = identity_sets() {
            self.identity_rules.push(IdRule {
                folder: folder_hook.then(|| glob.clone()),
                recipient: (!folder_hook).then_some(glob),
                name,
                email,
            });
            return;
        }
        if !folder_hook {
            self.skip(line, only_identity_sets);
            return;
        }
        match crate::command::parse(command) {
            Ok(cmds) if !cmds.is_empty() => self.folder_hook_lines.push((glob, command.clone())),
            Ok(_) => self.skip(line, "the hook command does nothing"),
            Err(err) => self.skip(line, &err),
        }
    }

    /// message-hook / reply-hook: the pattern stays a rmut pattern and
    /// the command stays a command line, so both have to parse.
    fn command_hook(&mut self, cmd: &str, args: &[String], line: &str) {
        let [pattern, command] = args else {
            self.skip(line, &format!("{cmd} PATTERN COMMAND"));
            return;
        };
        if let Err(err) = crate::pattern::parse(pattern) {
            self.skip(line, &format!("pattern does not translate: {err}"));
            return;
        }
        match crate::command::parse(command) {
            Ok(cmds) if !cmds.is_empty() => {
                let table = if cmd == "message-hook" {
                    &mut self.message_hooks
                } else {
                    &mut self.reply_hooks
                };
                table.push((pattern.clone(), command.clone()));
            }
            Ok(_) => self.skip(line, "the hook command does nothing"),
            Err(err) => self.skip(line, &err),
        }
    }

    /// fcc-hook / fcc-save-hook. A bare pattern gets mutt's
    /// $default_hook expansion, so "boss@example.com" means what mutt
    /// means by it; fcc-save-hook also fills [mail] save when it is
    /// the catch-all, which is what save-hook already does.
    fn fcc_hook(&mut self, cmd: &str, args: &[String], line: &str) {
        let [pattern, mailbox] = args else {
            self.skip(line, &format!("{cmd} PATTERN MAILBOX"));
            return;
        };
        let expanded = default_hook_pattern(pattern);
        if let Err(err) = crate::pattern::parse(&expanded) {
            self.skip(line, &format!("pattern does not translate: {err}"));
            return;
        }
        self.fcc_hooks.push((expanded, mailbox.clone()));
        if cmd == "fcc-save-hook" && matches!(pattern.as_str(), "." | ".*" | "~A") {
            self.save_default = Some(mailbox.clone());
        }
    }

    /// mutt's +x / =x mean "under $folder"; for an IMAP folder that is
    /// an account folder spec, locally a joined path. A full
    /// imap[s]:// URL maps to the mailbox in its path.
    fn expand_mailbox(&self, value: &str) -> String {
        if is_imap_url(value) {
            return format!("imap:{ACCOUNT}/{}", url_mailbox(value));
        }
        let Some(rest) = value.strip_prefix(['+', '=']) else {
            return value.to_string();
        };
        let rest = rest.trim_matches('/');
        match &self.folder {
            Some(f) if is_imap_url(f) => format!("imap:{ACCOUNT}/{rest}"),
            Some(f) => format!("{}/{rest}", f.trim_end_matches('/')),
            None => rest.to_string(),
        }
    }

    fn bind(&mut self, args: &[String], line: &str) {
        let [menus, key, function] = args else {
            self.skip(line, "unrecognized bind syntax");
            return;
        };
        if function == "noop" {
            self.satisfy(line, "unbound keys already do nothing");
            return;
        }
        let Some(key) = convert_key(key) else {
            self.skip(line, "key has no rmut syntax");
            return;
        };
        let mut used = false;
        for menu in menus.split(',') {
            let table = match menu {
                "index" => &mut self.keys_index,
                "pager" => &mut self.keys_pager,
                _ => continue,
            };
            let actions = match menu {
                "index" => index_function(function),
                _ => pager_function(function),
            };
            match actions {
                Some(action) => {
                    table.insert(action, key.clone());
                    used = true;
                }
                None => self.skip(line, &format!("no rmut action for {function} in {menu}")),
            }
        }
        if !used && !menus.split(',').any(|m| m == "index" || m == "pager") {
            self.skip(line, "only index and pager menus exist in rmut");
        }
    }

    /// `macro MENU KEY SEQUENCE [description]`, translated when the
    /// sequence is plain keys and prompt input; mutt function names
    /// (`<collapse-all>` etc.) have no rmut equivalent.
    fn mutt_macro(&mut self, args: &[String], line: &str) {
        let (menus, key, seq) = match args {
            [m, k, s] | [m, k, s, _] => (m, k, s),
            _ => {
                self.skip(line, "unrecognized macro syntax");
                return;
            }
        };
        let Some(key) = convert_key(key) else {
            self.skip(line, "key has no rmut syntax");
            return;
        };
        let Some(sequence) = convert_sequence(seq) else {
            self.skip(
                line,
                "only plain keys translate; mutt function names do not",
            );
            return;
        };
        let mut used = false;
        for menu in menus.split(',') {
            let table = match menu {
                "index" => &mut self.macros_index,
                "pager" => &mut self.macros_pager,
                _ => continue,
            };
            table.insert(key.clone(), sequence.clone());
            used = true;
        }
        if !used {
            self.skip(line, "only index and pager menus exist in rmut");
        }
    }

    /// `auto_view` / `unauto_view`: a [filters] entry per type. The
    /// command is left empty, which is rmut's "look it up in mailcap",
    /// exactly where mutt looks for it.
    fn auto_view(&mut self, add: bool, mime: &str, line: &str) {
        let mime = mime.to_lowercase();
        if !add {
            if mime == "*" {
                self.filters.clear();
            } else {
                self.filters.remove(&mime);
            }
            return;
        }
        match mime.as_str() {
            "application/pgp" | "application/pgp-signature" | "application/pgp-encrypted" => {
                self.satisfy(line, "PGP is handled natively");
            }
            _ => {
                self.filters.entry(mime).or_default();
            }
        }
    }

    fn color(&mut self, args: &[String], line: &str) {
        let (Some(object), Some(fg), Some(bg)) = (args.first(), args.get(1), args.get(2)) else {
            self.skip(line, "unrecognized color syntax");
            return;
        };
        let fg = convert_color(fg);
        let bg = convert_color(bg);
        // rmut's index slots take one color; when the foreground says
        // nothing (black/white/default on a colored background), the
        // background is what the user actually sees.
        let vivid = if matches!(fg.as_str(), "black" | "white" | "default") && bg != "default" {
            bg.clone()
        } else {
            fg.clone()
        };
        // `color quoted` / `color quotedN`: the depth palette.
        if let Some(n) = object.strip_prefix("quoted")
            && let Ok(depth) = if n.is_empty() { Ok(0usize) } else { n.parse() }
        {
            self.quoted_colors.insert(depth, vivid);
            return;
        }
        match (object.as_str(), args.get(3).map(String::as_str)) {
            ("status", _) => {
                self.colors.insert("status_fg", fg);
                self.colors.insert("status_bg", bg);
            }
            ("search", _) => {
                if fg != "default" {
                    self.colors.insert("search_fg", fg.clone());
                }
                if bg != "default" {
                    self.colors.insert("search_bg", bg.clone());
                }
            }
            ("body", Some(regex)) => {
                self.color_body_rules.push((regex.to_string(), fg, bg));
            }
            ("header" | "hdrdefault", _) => {
                self.colors.insert("header", fg);
            }
            ("error", _) => {
                self.colors.insert("error", vivid);
            }
            // rmut has a slot of its own for these three, but a slot
            // carries one colour: a line that paints a background as
            // well keeps both, as a rule, which is what it looked
            // like in mutt.
            ("index", Some("~D")) if bg == "default" => {
                self.colors.insert("deleted", vivid);
            }
            ("index", Some("~F")) if bg == "default" => {
                self.colors.insert("flagged", vivid);
            }
            ("index", Some("~T")) if bg == "default" => {
                self.colors.insert("tagged", vivid);
            }
            // Any other pattern rmut's engine parses becomes a
            // [[color_index]] rule.
            ("index", Some(pattern)) if crate::pattern::parse(pattern).is_ok() => {
                self.color_index_rules.push((pattern.to_string(), fg, bg));
            }
            _ => self.skip(line, "no rmut color slot"),
        }
    }

    // ---- output ----

    fn to_toml(&self) -> String {
        let mut out = String::from("# generated by rmut --import-muttrc; review before use\n");
        if self.name.is_some()
            || self.email.is_some()
            || self.reverse_name
            || self.no_reverse_realname
        {
            out += "\n[identity]\n";
            if let Some(n) = &self.name {
                out += &format!("name = {}\n", quote(n));
            }
            if let Some(e) = &self.email {
                out += &format!("email = {}\n", quote(e));
            }
            if self.reverse_name {
                out += "reverse_name = true\n";
            }
            if self.no_reverse_realname {
                out += "reverse_realname = false\n";
            }
        }
        for rule in &self.identity_rules {
            out += "\n[[identities]]\n";
            if let Some(f) = &rule.folder {
                out += &format!("folder = {}\n", quote(f));
            }
            if let Some(r) = &rule.recipient {
                out += &format!("recipient = {}\n", quote(r));
            }
            if let Some(n) = &rule.name {
                out += &format!("name = {}\n", quote(n));
            }
            if let Some(e) = &rule.email {
                out += &format!("email = {}\n", quote(e));
            }
        }
        for (glob, command) in &self.folder_hook_lines {
            out += &format!(
                "\n[[folder_hooks]]\nfolder = {}\ncommand = {}\n",
                quote(glob),
                quote(command)
            );
        }
        for (table, hooks) in [
            ("message_hooks", &self.message_hooks),
            ("reply_hooks", &self.reply_hooks),
        ] {
            for (pattern, command) in hooks {
                out += &format!(
                    "\n[[{table}]]\npattern = {}\ncommand = {}\n",
                    quote(pattern),
                    quote(command)
                );
            }
        }
        for (pattern, mailbox) in &self.fcc_hooks {
            out += &format!(
                "\n[[fcc_hooks]]\npattern = {}\nmailbox = {}\n",
                quote(pattern),
                quote(&self.expand_mailbox(mailbox))
            );
        }
        for (address, key) in &self.crypt_hooks {
            out += &format!(
                "\n[[crypt_hooks]]\naddress = {}\nkey = {}\n",
                quote(address),
                quote(key)
            );
        }
        // A full URL in spoolfile also identifies the IMAP server when
        // $folder is local or unset.
        let imap = self
            .folder
            .as_deref()
            .filter(|f| is_imap_url(f))
            .or_else(|| self.spoolfile.as_deref().filter(|s| is_imap_url(s)));
        let mut mailboxes = Vec::new();
        if let Some(spool) = &self.spoolfile {
            mailboxes.push(match imap {
                Some(_) if is_imap_url(spool) => {
                    format!("imap:{ACCOUNT}/{}", url_mailbox(spool))
                }
                Some(_) => {
                    let inbox = spool.trim_start_matches(['+', '=']).trim_matches('/');
                    format!("imap:{ACCOUNT}/{inbox}")
                }
                None => self.expand_mailbox(spool),
            });
        }
        for m in &self.mailboxes {
            if !mailboxes.contains(m) {
                mailboxes.push(m.clone());
            }
        }
        let sent_local = self.sent.as_deref().filter(|_| imap.is_none());
        // $folder itself is worth carrying over: rmut expands +x / =x
        // at runtime too, which is what makes an imported macro like
        // "<save-message>=archive<enter>" land in the right mailbox.
        let folder_setting = self.folder.clone().filter(|f| !is_imap_url(f));
        if folder_setting.is_some()
            || !mailboxes.is_empty()
            || sent_local.is_some()
            || self.postponed.is_some()
            || self.sendmail.is_some()
            || self.editor.is_some()
            || self.poll_seconds.is_some()
            || self.print.is_some()
            || self.query_command.is_some()
            || self.trash.is_some()
            || self.save_default.is_some()
            || self.forward_attach
            || self.forward_ask
            || self.fast_reply
            || self.quit.is_some()
            || self.postpone.is_some()
            || self.recall.is_some()
            || self.confirmappend
            || self.save_name
            || self.force_name
            || self.alias_file.is_some()
            || self.attribution.is_some()
            || self.indent_string.is_some()
            || self.reply_regexp.is_some()
            || self.simple_search.is_some()
            || self.no_wrap_search
            || self.forward_format.is_some()
            || self.include.is_some()
            || self.ask_cc
            || self.ask_bcc
            || self.autoedit
            || self.no_copy
            || self.new_mail_command.is_some()
            || self.edit_headers_on
            || !self.lists.is_empty()
            || !self.subscribed.is_empty()
            || !self.alternates.is_empty()
            || !self.my_hdr.is_empty()
            || self.metoo
            || self.no_mark_old
            || self.print_confirm.is_some()
            || self.forward_quote
            || self.signature.is_some()
            || self.no_sig_dashes
            || self.sig_on_top
            || self.hostname.is_some()
            || self.user_agent
            || self.abort_nosubject.is_some()
            || self.no_abort_unmodified
            || self.text_flowed
            || self.delete.is_some()
            || self.abort_noattach.is_some()
            || self.attach_keyword.is_some()
        {
            out += "\n[mail]\n";
            if let Some(f) = &folder_setting {
                out += &format!("folder = {}\n", quote(f));
            }
            for (key, values) in [
                ("lists", &self.lists),
                ("subscribed", &self.subscribed),
                ("alternates", &self.alternates),
                ("my_hdr", &self.my_hdr),
            ] {
                if !values.is_empty() {
                    let list: Vec<String> = values.iter().map(|v| quote(v)).collect();
                    out += &format!("{key} = [{}]\n", list.join(", "));
                }
            }
            if !mailboxes.is_empty() {
                let list: Vec<String> = mailboxes.iter().map(|m| quote(m)).collect();
                out += &format!("mailboxes = [{}]\n", list.join(", "));
            }
            if let Some(s) = sent_local {
                out += &format!("sent = {}\n", quote(&self.expand_mailbox(s)));
            }
            if let Some(p) = &self.postponed {
                out += &format!("postponed = {}\n", quote(&self.expand_mailbox(p)));
            }
            if let Some(s) = &self.sendmail {
                out += &format!("sendmail = {}\n", quote(s));
            }
            if let Some(e) = &self.editor {
                out += &format!("editor = {}\n", quote(e));
            }
            if let Some(n) = self.poll_seconds {
                out += &format!("poll_seconds = {n}\n");
            }
            if let Some(p) = &self.print {
                out += &format!("print = {}\n", quote(p));
            }
            if let Some(q) = &self.query_command {
                out += &format!("query_command = {}\n", quote(q));
            }
            if let Some(t) = &self.trash {
                out += &format!("trash = {}\n", quote(&self.expand_mailbox(t)));
            }
            if let Some(save) = &self.save_default {
                out += &format!("save = {}\n", quote(&self.expand_mailbox(save)));
            }
            if self.forward_ask {
                out += "forward = \"ask\"\n";
            } else if self.forward_attach {
                out += "forward = \"attach\"\n";
            }
            if self.fast_reply {
                out += "fast_reply = true\n";
            }
            if let Some(v) = &self.postpone {
                out += &format!("postpone = {}\n", quote(v));
            }
            if let Some(v) = &self.recall {
                out += &format!("recall = {}\n", quote(v));
            }
            if let Some(v) = &self.quit {
                out += &format!("quit = {}\n", quote(v));
            }
            if self.confirmappend {
                out += "confirmappend = true\n";
            }
            if self.save_name {
                out += "save_name = true\n";
            }
            if self.force_name {
                out += "force_name = true\n";
            }
            if let Some(v) = &self.alias_file {
                out += &format!("alias_file = {}\n", quote(v));
            }
            if let Some(v) = &self.attribution {
                out += &format!("attribution = {}\n", quote(v));
            }
            if let Some(v) = &self.indent_string {
                out += &format!("indent_string = {}\n", quote(v));
            }
            if let Some(v) = &self.reply_regexp {
                out += &format!("reply_regexp = {}\n", quote(v));
            }
            if let Some(v) = &self.simple_search {
                out += &format!("simple_search = {}\n", quote(v));
            }
            if self.no_wrap_search {
                out += "wrap_search = false\n";
            }
            if let Some(v) = &self.forward_format {
                out += &format!("forward_format = {}\n", quote(v));
            }
            if let Some(v) = &self.include {
                out += &format!("include = {}\n", quote(v));
            }
            if self.ask_cc {
                out += "ask_cc = true\n";
            }
            if self.ask_bcc {
                out += "ask_bcc = true\n";
            }
            if self.autoedit {
                out += "autoedit = true\n";
            }
            if self.no_copy {
                out += "copy = false\n";
            }
            if self.metoo {
                out += "metoo = true\n";
            }
            if self.no_mark_old {
                out += "mark_old = false\n";
            }
            if let Some(v) = &self.print_confirm {
                out += &format!("print_confirm = {}\n", quote(v));
            }
            if self.forward_quote {
                out += "forward_quote = true\n";
            }
            if let Some(v) = &self.signature {
                out += &format!("signature = {}\n", quote(v));
            }
            if self.no_sig_dashes {
                out += "sig_dashes = false\n";
            }
            if self.sig_on_top {
                out += "sig_on_top = true\n";
            }
            if let Some(v) = &self.hostname {
                out += &format!("hostname = {}\n", quote(v));
            }
            if self.user_agent {
                out += "user_agent = true\n";
            }
            if let Some(v) = &self.abort_nosubject {
                out += &format!("abort_nosubject = {}\n", quote(v));
            }
            if self.no_abort_unmodified {
                out += "abort_unmodified = false\n";
            }
            if self.text_flowed {
                out += "text_flowed = true\n";
            }
            if let Some(v) = &self.delete {
                out += &format!("delete = {}\n", quote(v));
            }
            if let Some(v) = &self.abort_noattach {
                out += &format!("abort_noattach = {}\n", quote(v));
            }
            if let Some(v) = &self.attach_keyword {
                out += &format!("attach_keyword = {}\n", quote(v));
            }
            if let Some(c) = &self.new_mail_command {
                out += &format!("new_mail_command = {}\n", quote(c));
            }
            if self.edit_headers_on {
                out += "edit_headers = true\n";
            }
        }
        if self.index_format.is_some()
            || self.sort.is_some()
            || self.sort_aux.is_some()
            || self.date_format.is_some()
            || self.collapse_unread_off
            || self.uncollapse_jump
            || self.hide_thread_subject
        {
            out += "\n[index]\n";
            if let Some(f) = &self.index_format {
                out += "# rmut renders %C %Z %d %F %L %c %l %s and %?X?then&else? conditionals\n";
                out += &format!("format = {}\n", quote(f));
            }
            if let Some(sort) = &self.sort {
                out += &format!("sort = {}\n", quote(sort));
            }
            if let Some(aux) = &self.sort_aux {
                out += &format!("sort_aux = {}\n", quote(aux));
            }
            if let Some(df) = &self.date_format {
                out += &format!("date_format = {}\n", quote(df));
            }
            if self.collapse_unread_off {
                out += "collapse_unread = false\n";
            }
            if self.uncollapse_jump {
                out += "uncollapse_jump = true\n";
            }
            if self.hide_thread_subject {
                out += "hide_thread_subject = true\n";
            }
        }
        if self.pager_index_lines.is_some()
            || self.pager_context.is_some()
            || self.search_context.is_some()
            || self.quote_regexp.is_some()
            || self.pager_format.is_some()
            || self.wrap.is_some()
            || self.tilde
            || !self.hdr_ignore.is_empty()
            || !self.hdr_unignore.is_empty()
            || !self.hdr_order.is_empty()
            || self.no_reflow_text
            || self.pager_stop
            || self.markers_off
            || self.smart_wrap_off
            || !self.alternative_order.is_empty()
        {
            out += "\n[pager]\n";
            if let Some(n) = self.pager_index_lines {
                out += &format!("index_lines = {n}\n");
            }
            if let Some(n) = self.pager_context {
                out += &format!("context = {n}\n");
            }
            if let Some(n) = self.search_context {
                out += &format!("search_context = {n}\n");
            }
            if let Some(re) = &self.quote_regexp {
                out += &format!("quote_regexp = {}\n", quote(re));
            }
            for (key, list) in [
                ("ignore", &self.hdr_ignore),
                ("unignore", &self.hdr_unignore),
                ("hdr_order", &self.hdr_order),
            ] {
                if !list.is_empty() {
                    let items: Vec<String> = list.iter().map(|s| quote(s)).collect();
                    out += &format!("{key} = [{}]\n", items.join(", "));
                }
            }
            if let Some(f) = &self.pager_format {
                out += "# rmut renders %C %m %n %s %Z %P %f and %>X here\n";
                out += &format!("format = {}\n", quote(f));
            }
            if let Some(n) = self.wrap {
                out += &format!("wrap = {n}\n");
            }
            if self.tilde {
                out += "tilde = true\n";
            }
            if self.no_reflow_text {
                out += "reflow_text = false\n";
            }
            if self.pager_stop {
                out += "pager_stop = true\n";
            }
            if self.markers_off {
                out += "markers = false\n";
            }
            if self.smart_wrap_off {
                out += "smart_wrap = false\n";
            }
            if !self.alternative_order.is_empty() {
                let items: Vec<String> = self.alternative_order.iter().map(|s| quote(s)).collect();
                out += &format!("alternative_order = [{}]\n", items.join(", "));
            }
        }
        if !self.filters.is_empty() {
            out += "\n[filters]\n# auto_view: an empty command means rmut takes it from your\n# mailcap (the first copiousoutput entry), as mutt does; put a\n# command here to override it\n";
            for (mime, command) in &self.filters {
                out += &format!("{} = {}\n", quote(mime), quote(command));
            }
        }
        if !self.colors.is_empty() || !self.quoted_colors.is_empty() {
            out += "\n[colors]\n";
            for (k, v) in &self.colors {
                out += &format!("{k} = {}\n", quote(v));
            }
            for (n, v) in &self.quoted_colors {
                let key = if *n == 0 {
                    "quoted".to_string()
                } else {
                    format!("quoted{n}")
                };
                out += &format!("{key} = {}\n", quote(v));
            }
        }
        for (pattern, fg, bg) in &self.color_index_rules {
            out += "\n[[color_index]]\n";
            out += &format!("pattern = {}\n", quote(pattern));
            if fg != "default" {
                out += &format!("fg = {}\n", quote(fg));
            }
            if bg != "default" {
                out += &format!("bg = {}\n", quote(bg));
            }
        }
        for (pattern, fg, bg) in &self.color_body_rules {
            out += "\n[[color_body]]\n";
            out += &format!("pattern = {}\n", quote(pattern));
            if fg != "default" {
                out += &format!("fg = {}\n", quote(fg));
            }
            if bg != "default" {
                out += &format!("bg = {}\n", quote(bg));
            }
        }
        if self.sidebar_visible || self.sidebar_width.is_some() {
            out += "\n[sidebar]\n";
            if self.sidebar_visible {
                out += "visible = true\n";
            }
            if let Some(width) = self.sidebar_width {
                out += &format!("width = {width}\n");
            }
        }
        if self.connect_timeout.is_some() || self.certificate_file.is_some() || self.no_system_cas {
            out += "\n[net]\n";
            if let Some(secs) = self.connect_timeout {
                out += &format!("connect_timeout = {secs}\n");
            }
            if let Some(path) = &self.certificate_file {
                out += &format!("certificate_file = {}\n", quote(path));
            }
            if self.no_system_cas {
                out += "system_cas = false\n";
            }
        }
        if self.status_format.is_some()
            || self.title_format.is_some()
            || self.history_file.is_some()
            || self.status_on_top
            || self.arrow_cursor
            || self.status_chars.is_some()
            || self.set_title
            || self.no_beep
            || self.beep_new
            || self.no_wait_key
        {
            out += "\n[ui]\n";
            if let Some(sf) = &self.status_format {
                out += "# rmut renders %f %m %M %n %u %d %F %t %s %V %r %v and\n";
                out += "# %?X?then&else? conditionals; other specifiers show literally\n";
                out += &format!("status_format = {}\n", quote(sf));
            }
            if self.set_title {
                out += "set_title = true\n";
            }
            if let Some(v) = &self.history_file {
                out += &format!("history_file = {}\n", quote(v));
            }
            if self.status_on_top {
                out += "status_on_top = true\n";
            }
            if self.arrow_cursor {
                out += "arrow_cursor = true\n";
            }
            if let Some(v) = &self.status_chars {
                out += &format!("status_chars = {}\n", quote(v));
            }
            if let Some(tf) = &self.title_format {
                out += &format!("title_format = {}\n", quote(tf));
            }
            if self.no_beep {
                out += "beep = false\n";
            }
            if self.beep_new {
                out += "beep_new = true\n";
            }
            if self.no_wait_key {
                out += "wait_key = false\n";
            }
        }
        for (section, table) in [("index", &self.keys_index), ("pager", &self.keys_pager)] {
            if !table.is_empty() {
                out += &format!("\n[keys.{section}]\n");
                for (action, key) in table {
                    out += &format!("{action} = {}\n", quote(key));
                }
            }
        }
        for (section, table) in [("index", &self.macros_index), ("pager", &self.macros_pager)] {
            if !table.is_empty() {
                out += &format!("\n[macros.{section}]\n");
                for (key, sequence) in table {
                    out += &format!("{} = {}\n", quote(key), quote(sequence));
                }
            }
        }
        if self.sign_key.is_some()
            || self.sign_by_default
            || self.encrypt_by_default
            || self.reply_sign
            || self.reply_encrypt
            || self.reply_sign_encrypted
        {
            out += "\n[pgp]\n";
            if let Some(k) = &self.sign_key {
                out += &format!("sign_key = {}\n", quote(k));
            }
            if self.sign_by_default {
                out += "sign_by_default = true\n";
            }
            if self.encrypt_by_default {
                out += "encrypt_by_default = true\n";
            }
            if self.reply_sign {
                out += "reply_sign = true\n";
            }
            if self.reply_encrypt {
                out += "reply_encrypt = true\n";
            }
            if self.reply_sign_encrypted {
                out += "reply_sign_encrypted = true\n";
            }
        }
        let mut skipped = self.skipped.clone();
        if imap.is_some() || self.smtp_url.is_some() {
            out += &self.account_toml(imap, &mut skipped);
        } else if self.imap_pass.is_some() || self.smtp_pass.is_some() {
            skipped.push(
                "set imap_pass/smtp_pass = (redacted)  (no IMAP/SMTP server, nowhere to put it)"
                    .into(),
            );
        }
        if !self.satisfied.is_empty() {
            out += "\n# satisfied by rmut's defaults (nothing to configure):\n";
            for s in &self.satisfied {
                out += &format!("#   {s}\n");
            }
        }
        if !self.differs.is_empty() {
            out += "\n# rmut does these its own way:\n";
            for s in &self.differs {
                out += &format!("#   {s}\n");
            }
        }
        if !skipped.is_empty() {
            out += "\n# not imported:\n";
            for s in &skipped {
                out += &format!("#   {s}\n");
            }
        }
        out
    }

    /// The password lines of the account: imap_pass, or smtp_pass when
    /// it is the only one given, or a placeholder to fill in.
    fn password_toml(&self, out: &mut String, skipped: &mut Vec<String>) {
        match (&self.imap_pass, &self.smtp_pass) {
            (Some(a), Some(b)) if a != b => {
                *out += "# imported from imap_pass; consider password_command instead\n";
                *out += &format!("password = {}\n", quote(a));
                skipped.push(
                    "set smtp_pass = (redacted)  (differs from imap_pass; rmut uses one password per account)"
                        .into(),
                );
            }
            (Some(pass), _) | (None, Some(pass)) => {
                *out += "# imported from imap_pass/smtp_pass; consider password_command instead\n";
                *out += &format!("password = {}\n", quote(pass));
            }
            (None, None) => {
                *out += "# TODO: set a command that prints the password (or password = \"...\"):\n";
                *out += "password_command = \"pass show mail/TODO\"\n";
            }
        }
    }

    fn account_toml(&self, folder_url: Option<&str>, skipped: &mut Vec<String>) -> String {
        let mut out = format!("\n[[accounts]]\nname = {}\n", quote(ACCOUNT));
        // imap_user wins; then the user@ part of the folder/smtp URL.
        let url_user = folder_url
            .into_iter()
            .chain(self.smtp_url.as_deref())
            .find_map(|url| split_url(url).0.map(str::to_string));
        let user = self
            .imap_user
            .clone()
            .or(url_user)
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| "TODO".into());
        out += &format!("user = {}\n", quote(&user));
        if let Some(mech) = &self.oauth {
            out += &format!("auth = {}\n", quote(mech));
            out += "# TODO: set a command that prints a fresh access token\n";
            out += "# (oauth2ms, mutt_oauth2.py, ...):\n";
            out += "token_command = \"oauth2ms\"\n";
            if self.imap_pass.is_some() || self.smtp_pass.is_some() {
                skipped.push(
                    "set imap_pass/smtp_pass = (redacted)  (the account authenticates with OAuth)"
                        .into(),
                );
            }
        } else {
            // One credential per account: imap_pass, or smtp_pass when
            // it is the only one given.
            self.password_toml(&mut out, skipped);
        }
        if let Some(url) = folder_url {
            let (_, host, port, tls) = split_url(url);
            out += &format!("imap_host = {}\n", quote(host));
            // imap:// means the standard port with STARTTLS (rmut
            // upgrades any non-993 port), never a plaintext connection.
            match (port, tls) {
                (Some(p), _) => out += &format!("imap_port = {p}\n"),
                (None, false) => out += "imap_port = 143\n",
                (None, true) => {}
            }
        }
        if let Some(smtp) = &self.smtp_url {
            let (_, host, port, tls) = split_url(smtp);
            out += &format!("smtp_host = {}\n", quote(host));
            match (port, tls) {
                (Some(p), _) => out += &format!("smtp_port = {p}\n"),
                // smtps:// default is implicit TLS on 465.
                (None, true) => out += "smtp_port = 465\n",
                (None, false) => {}
            }
        }
        if folder_url.is_some()
            && let Some(sent) = &self.sent
        {
            let folder = if is_imap_url(sent) {
                url_mailbox(sent)
            } else {
                sent.trim_start_matches(['+', '=']).trim_matches('/').into()
            };
            out += &format!("sent_folder = {}\n", quote(&folder));
        }
        out
    }
}

fn is_imap_url(value: &str) -> bool {
    value.starts_with("imap://") || value.starts_with("imaps://")
}

/// userinfo, host, explicit port, and whether the scheme implies TLS.
/// Tolerates a trailing empty port ("host:"); any mailbox path in the
/// URL is ignored here (see `url_mailbox`).
fn split_url(url: &str) -> (Option<&str>, &str, Option<u16>, bool) {
    let (tls, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.ends_with('s'), rest),
        None => (true, url),
    };
    let rest = rest.split('/').next().unwrap_or(rest);
    let (user, rest) = match rest.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u), r),
        _ => (None, rest),
    };
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => (user, host, port.parse().ok(), tls),
        _ => (user, rest, None, tls),
    }
}

/// The mailbox named by an imap[s]:// URL's path; INBOX when absent.
fn url_mailbox(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest
        .split_once('/')
        .map_or("", |(_, p)| p)
        .trim_matches('/');
    if path.is_empty() {
        "INBOX".into()
    } else {
        path.to_string()
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// mutt key syntax → rmut key syntax (None: no equivalent).
pub fn convert_key(key: &str) -> Option<String> {
    if let Some(c) = key.strip_prefix("\\C").or_else(|| key.strip_prefix("\\c")) {
        let mut chars = c.chars();
        let c = chars.next()?;
        return chars
            .next()
            .is_none()
            .then(|| format!("ctrl+{}", c.to_ascii_lowercase()));
    }
    if let Some(c) = key
        .strip_prefix("\\e")
        .or_else(|| key.strip_prefix("<esc>").filter(|rest| !rest.is_empty()))
    {
        let mut chars = c.chars();
        let c = chars.next()?;
        return chars.next().is_none().then(|| format!("alt+{c}"));
    }
    let named = match key.to_lowercase().as_str() {
        "<return>" | "<enter>" => "enter",
        "<esc>" => "esc",
        "<space>" => "space",
        "<tab>" => "tab",
        "<backspace>" => "backspace",
        "<up>" => "up",
        "<down>" => "down",
        "<pageup>" => "pgup",
        "<pagedown>" => "pgdn",
        "<home>" => "home",
        "<end>" => "end",
        _ => {
            let mut chars = key.chars();
            let c = chars.next()?;
            return (chars.next().is_none() && c != '<').then(|| c.to_string());
        }
    };
    Some(named.to_string())
}

/// A mutt macro sequence in rmut's syntax: literal characters,
/// `<key-name>`s rmut knows, and `\`-escapes (`\n` `\t` `\e` `\Cx`).
/// None when it names mutt functions or exotic keys.
fn convert_sequence(seq: &str) -> Option<String> {
    const NAMES: [&str; 15] = [
        "enter",
        "return",
        "esc",
        "escape",
        "space",
        "tab",
        "backspace",
        "up",
        "down",
        "pgup",
        "pageup",
        "pgdn",
        "pagedown",
        "home",
        "end",
    ];
    let mut out = String::new();
    let mut chars = seq.chars();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('>') => break,
                        Some(c) => name.push(c),
                        None => return None,
                    }
                }
                let name = name.to_lowercase();
                if !NAMES.contains(&name.as_str()) {
                    return None; // a mutt function name
                }
                out += &format!("<{name}>");
            }
            '\\' => match chars.next()? {
                'n' | 'r' => out += "<enter>",
                't' => out += "<tab>",
                'e' => out += "<esc>",
                'c' | 'C' => out += &format!("<ctrl+{}>", chars.next()?.to_ascii_lowercase()),
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// mutt "bright" colors are close to ratatui's "light" family.
pub(crate) fn convert_color(name: &str) -> String {
    match name.strip_prefix("bright") {
        Some(base) => format!("light{base}"),
        None => name.to_string(),
    }
}

pub fn index_function(name: &str) -> Option<&'static str> {
    Some(match name {
        "quit" => "quit",
        "exit" => "abort",
        "next-entry" | "next-undeleted" => "down",
        "previous-entry" | "previous-undeleted" => "up",
        "next-page" => "page-down",
        "previous-page" => "page-up",
        "first-entry" => "first",
        "last-entry" => "last",
        "display-message" => "view",
        "delete-message" => "delete",
        "undelete-message" => "undelete",
        "flag-message" => "flag",
        "toggle-new" => "toggle-new",
        "sync-mailbox" => "sync",
        "mail" => "compose",
        "reply" => "reply",
        "group-reply" => "group-reply",
        "forward-message" => "forward",
        "sort-mailbox" => "sort",
        "limit" => "limit",
        "search" => "search",
        "search-next" => "search-next",
        "next-new" | "next-new-then-unread" | "next-unread" => "next-new",
        "previous-new" | "previous-new-then-unread" | "previous-unread" => "previous-new",
        "delete-pattern" => "delete-pattern",
        "undelete-pattern" => "undelete-pattern",
        "tag-pattern" => "tag-pattern",
        "untag-pattern" => "untag-pattern",
        "change-folder" => "change-mailbox",
        "view-attachments" => "attachments",
        "collapse-thread" => "fold-thread",
        "collapse-all" => "fold-all",
        // The thread functions, under mutt's own names.
        "delete-thread" => "delete-thread",
        "undelete-thread" => "undelete-thread",
        "tag-thread" => "tag-thread",
        "delete-subthread" => "delete-subthread",
        "undelete-subthread" => "undelete-subthread",
        "next-thread" => "next-thread",
        "previous-thread" => "previous-thread",
        "break-thread" => "break-thread",
        "link-threads" => "link-threads",
        "read-thread" => "read-thread",
        "read-subthread" => "read-subthread",
        "tag-subthread" => "tag-subthread",
        "parent-message" => "parent-message",
        "root-message" => "root-message",
        "imap-fetch-mail" | "fetch-mail" => "fetch-mail",
        "tag-entry" | "tag-message" => "tag",
        "tag-prefix" => "tag-prefix",
        "query" => "query",
        "save-message" => "save",
        "decode-save" => "decode-save",
        "decode-copy" => "decode-copy",
        "print-message" => "print",
        "edit" => "edit",
        "resend-message" => "resend",
        "edit-label" => "edit-label",
        "show-version" => "show-version",
        "show-limit" => "show-limit",
        "display-address" => "display-address",
        "toggle-write" => "toggle-write",
        "top-page" => "top-page",
        "middle-page" => "middle-page",
        "bottom-page" => "bottom-page",
        "help" => "help",
        _ => return None,
    })
}

pub fn pager_function(name: &str) -> Option<&'static str> {
    Some(match name {
        "exit" => "back",
        "next-line" => "down",
        "previous-line" => "up",
        "next-page" => "page-down",
        "previous-page" => "page-up",
        "half-down" => "half-down",
        "half-up" => "half-up",
        "top" => "top",
        "bottom" => "bottom",
        "toggle-quoted" => "toggle-quoted",
        "skip-quoted" => "skip-quoted",
        "next-entry" => "next",
        "previous-entry" => "previous",
        "next-undeleted" => "next-undeleted",
        "previous-undeleted" => "previous-undeleted",
        "delete-message" => "delete",
        "display-toggle-weed" => "headers",
        "search" => "search",
        "search-next" => "search-next",
        "search-opposite" => "search-prev",
        "search-toggle" => "search-toggle",
        "view-attachments" => "attachments",
        "mail" => "compose",
        "reply" => "reply",
        "group-reply" => "group-reply",
        "forward-message" => "forward",
        "save-message" => "save",
        "print-message" => "print",
        "edit" => "edit",
        "resend-message" => "resend",
        "help" => "help",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn to_config(muttrc: &str) -> (Config, String) {
        let import = import(muttrc, Path::new("/nonexistent"));
        let cfg: Config = toml::from_str(&import.toml)
            .unwrap_or_else(|e| panic!("bad TOML: {e}\n{}", import.toml));
        (cfg, import.toml)
    }

    #[test]
    fn format_flowed_options_import() {
        let (cfg, toml) = to_config("set text_flowed\nset noreflow_text\n");
        assert!(cfg.mail.text_flowed);
        assert_eq!(cfg.pager.reflow_text, Some(false));
        assert!(toml.contains("text_flowed = true"), "{toml}");
        assert!(toml.contains("reflow_text = false"), "{toml}");
        // Both defaults match rmut's, so an explicit default is quiet.
        let (cfg, _) = to_config("set notext_flowed\nset reflow_text\n");
        assert!(!cfg.mail.text_flowed);
        assert_eq!(cfg.pager.reflow_text, None);
    }

    #[test]
    fn index_colors_keep_a_background() {
        let (cfg, toml) = to_config(concat!(
            "color index black magenta \"~D\"\n",
            "color index brightred default \"~F\"\n",
        ));
        // A background means mutt painted a bar: both colours stay,
        // as a [[color_index]] rule.
        let deleted = cfg.color_index.iter().find(|r| r.pattern == "~D").unwrap();
        assert_eq!(deleted.fg.as_deref(), Some("black"));
        assert_eq!(deleted.bg.as_deref(), Some("magenta"));
        // No background: one visible colour, which is what the
        // built-in slot holds.
        assert_eq!(
            cfg.colors.get("flagged").map(String::as_str),
            Some("lightred")
        );
        assert!(toml.contains("[[color_index]]"), "{toml}");
    }

    #[test]
    fn attachment_reminder_imports() {
        let (cfg, toml) = to_config(concat!(
            "set abort_noattach = ask-yes\n",
            "set abort_noattach_regex = \"\\\\<(attach|pripojen)\"\n",
        ));
        // ask-yes and ask-no are the same question to rmut.
        assert_eq!(cfg.mail.abort_noattach.as_deref(), Some("ask"));
        assert_eq!(
            cfg.mail.attach_keyword.as_deref(),
            Some("\\b(attach|pripojen)")
        );
        assert!(toml.contains("abort_noattach"), "{toml}");
        let (cfg, _) = to_config("set abort_noattach = yes\n");
        assert_eq!(cfg.mail.abort_noattach.as_deref(), Some("yes"));
    }

    #[test]
    fn hooks_round_two_translate() {
        let (cfg, toml) = to_config(concat!(
            "folder-hook work 'set index_format=\"%s\"'\n",
            "folder-hook . 'set sort=threads'\n",
            "message-hook '~f boss@example\\.com' 'set pager_context=5'\n",
            "reply-hook '~t @work\\.example\\.com' 'set from=jane@work.example.com'\n",
            "fcc-hook '~t @work\\.example\\.com' +work-sent\n",
            "fcc-save-hook boss@example.com +boss\n",
            "crypt-hook boss@example.com 0xDEADBEEF\n",
            "message-hook '~X 3' 'set beep'\n", // unparseable pattern
            "reply-hook '~s x' 'frobnicate'\n", // unknown command
        ));
        assert_eq!(cfg.folder_hooks.len(), 2);
        assert_eq!(cfg.folder_hooks[0].folder, "*work*");
        assert_eq!(cfg.folder_hooks[0].command, "set index_format=\"%s\"");
        assert_eq!(cfg.folder_hooks[1].folder, "*");
        assert_eq!(cfg.message_hooks.len(), 1);
        assert_eq!(cfg.message_hooks[0].command, "set pager_context=5");
        assert_eq!(cfg.reply_hooks.len(), 1);
        assert_eq!(cfg.reply_hooks[0].pattern, "~t @work\\.example\\.com");
        assert_eq!(cfg.fcc_hooks.len(), 2);
        assert_eq!(cfg.fcc_hooks[0].mailbox, "work-sent");
        // A bare fcc-hook pattern gets mutt's $default_hook expansion.
        assert_eq!(
            cfg.fcc_hooks[1].pattern,
            "(~f \"boss@example.com\" !~P) | (~P ~C \"boss@example.com\")"
        );
        assert_eq!(cfg.crypt_hooks.len(), 1);
        assert_eq!(cfg.crypt_hooks[0].key, "0xDEADBEEF");
        // The two broken hooks stay visible as comments.
        assert!(toml.contains("pattern does not translate"), "{toml}");
        assert!(toml.contains("frobnicate"), "{toml}");
    }

    #[test]
    fn default_hook_expansion() {
        assert_eq!(default_hook_pattern("."), "~A");
        assert_eq!(default_hook_pattern("~t x"), "~t x");
        assert_eq!(default_hook_pattern("!~P"), "!~P");
        assert_eq!(
            default_hook_pattern("a@b"),
            "(~f \"a@b\" !~P) | (~P ~C \"a@b\")"
        );
    }

    #[test]
    fn alternates_and_my_hdr_import() {
        let (cfg, toml) = to_config(concat!(
            "alternates jane@old\\.example\\.com '@club\\.example\\.com$'\n",
            "alternates typo@example\\.com\n",
            "unalternates typo@example\\.com\n",
            "my_hdr Organization: Acme\n",
            "my_hdr X-Mailer: rmut\n",
            "my_hdr Organization: Acme Ltd\n",
            "unmy_hdr X-Mailer\n",
            "set metoo\n",
        ));
        assert_eq!(
            cfg.mail.alternates,
            ["jane@old\\.example\\.com", "@club\\.example\\.com$"]
        );
        // One entry per header name: the later Organization wins, and
        // unmy_hdr takes X-Mailer back out.
        assert_eq!(cfg.mail.my_hdr, ["Organization: Acme Ltd"]);
        assert!(cfg.mail.metoo);
        assert!(toml.contains("alternates = ["), "{toml}");
        assert!(toml.contains("metoo = true"), "{toml}");
    }

    #[test]
    fn hooks_become_identity_rules() {
        let (cfg, toml) = to_config(concat!(
            "set reverse_name = yes\n",
            "folder-hook work 'set from=\"Jane Work <jane@work.example.com>\"'\n",
            "send-hook '~t @club\\.example\\.com' 'set realname=\"Jenny\"'\n",
            "folder-hook . 'push <collapse-all>'\n",
            "send-hook '~l' 'set from=list@example.com'\n",
        ));
        assert!(cfg.identity.reverse_name);
        assert_eq!(cfg.identities.len(), 2);
        let work = &cfg.identities[0];
        assert_eq!(work.folder.as_deref(), Some("*work*"));
        assert!(work.recipient.is_none());
        assert_eq!(work.name.as_deref(), Some("Jane Work"));
        assert_eq!(work.email.as_deref(), Some("jane@work.example.com"));
        let club = &cfg.identities[1];
        assert_eq!(club.recipient.as_deref(), Some("*@club.example.com*"));
        assert!(club.folder.is_none());
        assert_eq!(club.name.as_deref(), Some("Jenny"));
        // A folder-hook that is not an identity set becomes a
        // [[folder_hooks]] line; the ~l send-hook stays a comment.
        assert_eq!(cfg.folder_hooks.len(), 1);
        assert_eq!(cfg.folder_hooks[0].folder, "*");
        assert_eq!(cfg.folder_hooks[0].command, "push <collapse-all>");
        assert!(toml.contains("does not translate to a glob"), "{toml}");
        // reverse_name off matches rmut's default.
        let (cfg, toml) = to_config("set reverse_name = no\n");
        assert!(!cfg.identity.reverse_name);
        assert!(toml.contains("# satisfied"), "{toml}");
    }

    #[test]
    fn oauth_authenticators_become_account_auth() {
        let (cfg, toml) = to_config(concat!(
            "set folder = \"imaps://outlook.example.com/\"\n",
            "set imap_user = \"jane@example.com\"\n",
            "set imap_pass = \"hunter2\"\n",
            "set imap_authenticators = \"oauthbearer\"\n",
            "set smtp_authenticators = \"xoauth2:plain\"\n",
        ));
        let account = cfg.account("mutt").unwrap();
        // The last-seen mechanism wins; both directives name OAuth.
        assert_eq!(account.auth.as_deref(), Some("xoauth2"));
        assert_eq!(account.token_command.as_deref(), Some("oauth2ms"));
        // The stored password is not emitted alongside OAuth.
        assert!(account.password.is_none());
        assert!(!toml.contains("hunter2"), "{toml}");
        assert!(toml.contains("the account authenticates with OAuth"));
        assert!(toml.contains("fresh access token"), "{toml}");
        // Plain-only stays satisfied, unknown mechanisms stay skipped.
        let (_, toml) = to_config("set smtp_authenticators = \"plain\"\n");
        assert!(toml.contains("# satisfied"), "{toml}");
        let (_, toml) = to_config("set smtp_authenticators = \"gssapi\"\n");
        assert!(toml.contains("xoauth2/oauthbearer"), "{toml}");
    }

    #[test]
    fn color_index_patterns_and_status_format_import() {
        let (cfg, toml) = to_config(concat!(
            "color index yellow default \"~f boss@example.com\"\n",
            "color index brightred blue \"~d <1w ~U\"\n",
            "color index red default ~D\n",         // still a slot
            "color index green default \"~X 3\"\n", // unparseable
            "set status_format = \"-%r- %f [%m msgs%?t?, %t tagged?]\"\n",
        ));
        assert_eq!(cfg.color_index.len(), 2);
        assert_eq!(cfg.color_index[0].pattern, "~f boss@example.com");
        assert_eq!(cfg.color_index[0].fg.as_deref(), Some("yellow"));
        assert!(cfg.color_index[0].bg.is_none()); // "default" dropped
        assert_eq!(cfg.color_index[1].fg.as_deref(), Some("lightred"));
        assert_eq!(cfg.color_index[1].bg.as_deref(), Some("blue"));
        assert_eq!(cfg.colors.get("deleted").map(String::as_str), Some("red"));
        assert_eq!(
            cfg.ui.status_format.as_deref(),
            Some("-%r- %f [%m msgs%?t?, %t tagged?]")
        );
        assert!(toml.contains("no rmut color slot"), "{toml}");
    }

    #[test]
    fn macros_translate_plain_sequences() {
        let (cfg, toml) = to_config(concat!(
            "macro index L \"l~f jane\\n\" \"limit to jane\"\n",
            "macro index,pager \\Cs \"~s urgent<Enter>\"\n",
            "macro index A \"<collapse-all>\"\n",
            "macro compose X \"y\"\n",
        ));
        assert_eq!(
            cfg.macros.index.get("L").map(String::as_str),
            Some("l~f jane<enter>")
        );
        assert_eq!(
            cfg.macros.index.get("ctrl+s").map(String::as_str),
            Some("~s urgent<enter>")
        );
        assert_eq!(
            cfg.macros.pager.get("ctrl+s").map(String::as_str),
            Some("~s urgent<enter>")
        );
        // Function names and foreign menus stay visible as comments.
        assert!(toml.contains("mutt function names do not"), "{toml}");
        assert!(toml.contains("only index and pager menus"), "{toml}");
    }

    #[test]
    fn hook_glob_shapes() {
        assert_eq!(hook_glob(".").as_deref(), Some("*"));
        assert_eq!(hook_glob("work").as_deref(), Some("*work*"));
        assert_eq!(hook_glob("^/mail/work$").as_deref(), Some("/mail/work"));
        assert_eq!(hook_glob("=lists").as_deref(), Some("*lists*"));
        assert_eq!(
            hook_glob("~t bob@example\\.com").as_deref(),
            Some("*bob@example.com*")
        );
        assert_eq!(hook_glob("work.*").as_deref(), Some("*work*"));
        assert!(hook_glob("(a|b)").is_none());
        assert!(hook_glob("~f jane").is_none());
    }

    #[test]
    fn set_forms_and_identity() {
        let (cfg, _) = to_config(concat!(
            "set realname = \"Jane Doe\"\n",
            "set from=jane@example.com\n",
            "set editor = vim  # trailing comment\n",
            "set mail_check=30\n",
        ));
        assert_eq!(cfg.identity.name.as_deref(), Some("Jane Doe"));
        assert_eq!(cfg.identity.email.as_deref(), Some("jane@example.com"));
        assert_eq!(cfg.mail.editor.as_deref(), Some("vim"));
        assert_eq!(cfg.mail.poll_seconds, Some(30));
    }

    #[test]
    fn from_with_display_name_fills_both() {
        let (cfg, _) = to_config("set from = \"Jane Doe <jane@x.org>\"\n");
        assert_eq!(cfg.identity.name.as_deref(), Some("Jane Doe"));
        assert_eq!(cfg.identity.email.as_deref(), Some("jane@x.org"));
        // explicit realname wins over the From display name
        let (cfg, _) = to_config("set realname=RN\nset from = \"DN <j@x>\"\n");
        assert_eq!(cfg.identity.name.as_deref(), Some("RN"));
    }

    #[test]
    fn mailboxes_expand_against_folder() {
        let (cfg, _) = to_config(concat!(
            "set folder = ~/Mail\n",
            "set spoolfile = +inbox\n",
            "set record = +sent\n",
            "set postponed = +drafts\n",
            "mailboxes +inbox +work ~/other\n",
        ));
        assert_eq!(
            cfg.mail.mailboxes,
            vec!["~/Mail/inbox", "~/Mail/work", "~/other"]
        );
        assert_eq!(cfg.mail.sent.as_deref(), Some("~/Mail/sent"));
        assert_eq!(cfg.mail.postponed.as_deref(), Some("~/Mail/drafts"));
        // $folder carries over, so rmut expands +x / =x at runtime as
        // well: an imported macro can still say "=archive".
        assert_eq!(cfg.mail.folder.as_deref(), Some("~/Mail"));
    }

    #[test]
    fn binds_translate_keys_and_functions() {
        let (cfg, toml) = to_config(concat!(
            "bind index \\Cd delete-message\n",
            "bind index <esc>s sync-mailbox\n",
            "bind pager <space> next-page\n",
            "bind index,pager R group-reply\n",
            "bind index gg first-entry\n", // multi-key: skipped
            "bind index Z frobnicate\n",   // unknown function: skipped
        ));
        assert_eq!(
            cfg.keys.index.get("delete").map(String::as_str),
            Some("ctrl+d")
        );
        assert_eq!(
            cfg.keys.index.get("sync").map(String::as_str),
            Some("alt+s")
        );
        assert_eq!(
            cfg.keys.pager.get("page-down").map(String::as_str),
            Some("space")
        );
        assert_eq!(
            cfg.keys.index.get("group-reply").map(String::as_str),
            Some("R")
        );
        assert_eq!(
            cfg.keys.pager.get("group-reply").map(String::as_str),
            Some("R")
        );
        assert!(toml.contains("# not imported:"));
        assert!(toml.contains("bind index gg first-entry"));
        assert!(toml.contains("bind index Z frobnicate"));
    }

    #[test]
    fn colors_map_to_rmut_slots() {
        let (cfg, toml) = to_config(concat!(
            "color status brightyellow blue\n",
            "color header cyan default\n",
            "color index red default ~D\n",
            "color index brightmagenta default ~F\n",
            "color indicator black white\n", // no slot: skipped
        ));
        assert_eq!(
            cfg.colors.get("status_fg").map(String::as_str),
            Some("lightyellow")
        );
        assert_eq!(
            cfg.colors.get("status_bg").map(String::as_str),
            Some("blue")
        );
        assert_eq!(cfg.colors.get("header").map(String::as_str), Some("cyan"));
        assert_eq!(cfg.colors.get("deleted").map(String::as_str), Some("red"));
        assert_eq!(
            cfg.colors.get("flagged").map(String::as_str),
            Some("lightmagenta")
        );
        assert!(toml.contains("color indicator"));
    }

    #[test]
    fn pager_colors_and_motion_translate() {
        let (cfg, toml) = to_config(concat!(
            "color quoted cyan default\n",
            "color quoted1 yellow default\n",
            "color body magenta default \"https?://[^ ]+\"\n",
            "color search black yellow\n",
            "set quote_regexp=\"^( *[>|])+\"\n",
            "bind pager \\Cd half-down\n",
            "bind pager T toggle-quoted\n",
            "bind pager S skip-quoted\n",
        ));
        assert_eq!(cfg.colors.get("quoted").map(String::as_str), Some("cyan"));
        assert_eq!(
            cfg.colors.get("quoted1").map(String::as_str),
            Some("yellow")
        );
        assert_eq!(
            cfg.colors.get("search_bg").map(String::as_str),
            Some("yellow")
        );
        assert_eq!(cfg.color_body.len(), 1);
        assert_eq!(cfg.color_body[0].pattern, "https?://[^ ]+");
        assert_eq!(cfg.color_body[0].fg.as_deref(), Some("magenta"));
        assert_eq!(cfg.pager.quote_regexp.as_deref(), Some("^( *[>|])+"));
        assert_eq!(
            cfg.keys.pager.get("half-down").map(String::as_str),
            Some("ctrl+d")
        );
        assert_eq!(
            cfg.keys.pager.get("toggle-quoted").map(String::as_str),
            Some("T")
        );
        assert!(toml.contains("[[color_body]]"));
    }

    #[test]
    fn header_weeding_and_pager_polish_translate() {
        let (cfg, toml) = to_config(concat!(
            "ignore *\n",
            "unignore from date subject\n",
            "hdr_order Date: From: Subject:\n",
            "set pager_format=\"-%Z- %C/%m: %s\"\n",
            "set wrap = 78\n",
            "set tilde\n",
        ));
        assert_eq!(cfg.pager.ignore.as_deref(), Some(&["*".to_string()][..]));
        assert_eq!(
            cfg.pager.unignore.as_deref(),
            Some(&["from".to_string(), "date".into(), "subject".into()][..])
        );
        assert_eq!(
            cfg.pager.hdr_order.as_deref(),
            Some(&["Date".to_string(), "From".into(), "Subject".into()][..])
        );
        assert_eq!(cfg.pager.format.as_deref(), Some("-%Z- %C/%m: %s"));
        assert_eq!(cfg.pager.wrap, Some(78));
        assert!(cfg.pager.tilde);
        assert!(toml.contains("hdr_order"));
    }

    #[test]
    fn compose_round_two_translates() {
        let (cfg, _) = to_config(concat!(
            "set fast_reply = yes\n",
            "set autoedit\n",
            "set nocopy\n",
            "set mime_forward = ask-yes\n",
            "set forward_decode\n", // satisfied: always decoded
            "set new_mail_command=\"notify-send 'rmut: %n new in %f'\"\n",
        ));
        assert!(cfg.mail.fast_reply);
        assert!(cfg.mail.autoedit);
        assert_eq!(cfg.mail.copy, Some(false));
        assert_eq!(cfg.mail.forward.as_deref(), Some("ask"));
        assert_eq!(
            cfg.mail.new_mail_command.as_deref(),
            Some("notify-send 'rmut: %n new in %f'")
        );
    }

    #[test]
    fn the_signature_and_the_send_questions_import() {
        let (cfg, toml) = to_config(concat!(
            "set signature = \"~/.signature\"\n",
            "set nosig_dashes\n",
            "set forward_quote\n",
            "set abort_nosubject = no\n",
            "set noabort_unmodified\n",
        ));
        assert_eq!(cfg.mail.signature.as_deref(), Some("~/.signature"));
        assert_eq!(cfg.mail.sig_dashes, Some(false));
        assert!(cfg.mail.forward_quote);
        assert_eq!(cfg.mail.abort_nosubject.as_deref(), Some("no"));
        assert_eq!(cfg.mail.abort_unmodified, Some(false));
        assert!(toml.contains("signature = \"~/.signature\""), "{toml}");
    }

    #[test]
    fn what_rmut_already_asks_is_satisfied_not_skipped() {
        // The mutt defaults for these four are rmut's behaviour, so
        // they belong in neither the config nor the skipped list.
        let (cfg, toml) = to_config(concat!(
            "set reply_to = ask-yes\n",
            "set honor_followup_to = yes\n",
            "set abort_nosubject = ask-yes\n",
            "set abort_unmodified = yes\n",
            "set sig_dashes = yes\n",
            "set noforward_quote\n",
        ));
        assert_eq!(cfg.mail.abort_nosubject, None);
        assert_eq!(cfg.mail.abort_unmodified, None);
        assert_eq!(cfg.mail.sig_dashes, None);
        assert!(!cfg.mail.forward_quote);
        let unclaimed = toml.split("# not imported:").nth(1).unwrap_or_default();
        assert!(unclaimed.trim().is_empty(), "{toml}");
        assert!(toml.contains("# satisfied by rmut's defaults"), "{toml}");
        // reply_to's other three are rmut answering it for you.
        let (_, toml) = to_config("set reply_to = yes\n");
        let unclaimed = toml.split("# not imported:").nth(1).unwrap_or_default();
        assert!(unclaimed.contains("set reply_to = yes"), "{toml}");
    }

    #[test]
    fn the_small_habits_import_or_are_already_rmut() {
        let (cfg, toml) = to_config(concat!(
            "set nomark_old\n",
            "set beep_new = yes\n",
            "set nowait_key\n",
            "set print = ask-yes\n",
            "set noreverse_realname\n",
        ));
        assert_eq!(cfg.mail.mark_old, Some(false));
        assert!(cfg.ui.beep_new);
        assert_eq!(cfg.ui.wait_key, Some(false));
        assert_eq!(cfg.mail.print_confirm.as_deref(), Some("ask-yes"));
        assert_eq!(cfg.identity.reverse_realname, Some(false));
        assert!(toml.contains("beep_new = true"), "{toml}");

        // The mutt defaults for all five are what rmut does anyway,
        // and $timeout is a poll rmut runs its own way.
        let (_, toml) = to_config(concat!(
            "set mark_old = yes\n",
            "set nobeep_new\n",
            "set wait_key = yes\n",
            "set print = ask-no\n",
            "set reverse_realname = yes\n",
            "set timeout = 15\n",
        ));
        let unclaimed = toml.split("# not imported:").nth(1).unwrap_or_default();
        assert!(unclaimed.trim().is_empty(), "{toml}");
        assert!(toml.contains("#   set timeout = 15  (rmut polls"), "{toml}");
    }

    #[test]
    fn pgp_settings_translate() {
        let (cfg, _) = to_config(concat!(
            "set pgp_sign_as = 0xDEADBEEF\n",
            "set crypt_autosign = yes\n",
            "set nocrypt_autoencrypt\n",
        ));
        assert_eq!(cfg.pgp.sign_key.as_deref(), Some("0xDEADBEEF"));
        assert!(cfg.pgp.sign_by_default);
        assert!(!cfg.pgp.encrypt_by_default);
    }

    #[test]
    fn what_rmut_has_is_imported_and_what_it_does_its_own_way_is_said() {
        let (cfg, toml) = to_config(concat!(
            "set sidebar_visible = yes\n",
            "set sidebar_width = 24\n",
            "set sidebar_format = \"%B%* %N\"\n",
            "set alias_file = ~/.mutt/aliases\n",
            "set imap_idle = yes\n",
            "set header_cache = ~/.cache/mutt\n",
            "set crypt_use_gpgme = yes\n",
            "set implicit_autoview = yes\n",
        ));
        // The settings rmut has come across.
        assert!(cfg.sidebar.visible, "{toml}");
        assert_eq!(cfg.sidebar.width, 24);
        assert_eq!(cfg.mail.alias_file.as_deref(), Some("~/.mutt/aliases"));
        // The ones it satisfies say so, rather than reading as holes.
        assert!(
            toml.contains("rmut IDLEs whenever the server offers it"),
            "{toml}"
        );
        assert!(toml.contains("# rmut does these its own way:"), "{toml}");
        assert!(toml.contains("under ~/.cache/rmut"), "{toml}");
        assert!(toml.contains("gpg(1) directly"), "{toml}");
        assert!(toml.contains("no format string"), "{toml}");
        // And none of them is claimed as not imported.
        let not_imported = toml.split("# not imported:").nth(1).unwrap_or("");
        for gone in [
            "sidebar_visible",
            "imap_idle",
            "header_cache",
            "crypt_use_gpgme",
        ] {
            assert!(!not_imported.contains(gone), "{gone} still skipped: {toml}");
        }
    }

    #[test]
    fn the_reply_and_forward_text_carries_over() {
        let (cfg, toml) = to_config(concat!(
            "set attribution = \"On %d, %n wrote:\"\n",
            "set indent_string = \"| \"\n",
            "set forward_format = \"Fwd: %s\"\n",
            "set include = no\n",
            "set askcc = yes\n",
            "set askbcc = yes\n",
        ));
        assert_eq!(
            cfg.mail.attribution.as_deref(),
            Some("On %d, %n wrote:"),
            "{toml}"
        );
        assert_eq!(cfg.mail.indent_string.as_deref(), Some("| "));
        assert_eq!(cfg.mail.forward_format.as_deref(), Some("Fwd: %s"));
        assert_eq!(cfg.mail.include.as_deref(), Some("no"));
        assert!(cfg.mail.ask_cc && cfg.mail.ask_bcc);
        // A quadoption rmut cannot make sense of is reported, not
        // guessed at.
        let import = import("set include = maybe\n", Path::new("/nonexistent"));
        assert!(
            import
                .toml
                .contains("include wants yes / no / ask-yes / ask-no"),
            "{}",
            import.toml
        );
    }

    #[test]
    fn connect_timeout_carries_over() {
        let (cfg, toml) = to_config("set connect_timeout=15\n");
        assert_eq!(cfg.net.connect_timeout, 15, "{toml}");
        // mutt waits for the OS when it is zero or less.
        let (cfg, _) = to_config("set connect_timeout=-1\n");
        assert_eq!(cfg.net.connect_timeout, 0);
    }

    #[test]
    fn wrap_search_off_carries_over() {
        let (cfg, toml) = to_config("set nowrap_search\n");
        assert_eq!(cfg.mail.wrap_search, Some(false), "{toml}");
    }

    #[test]
    fn simple_search_carries_over() {
        let (cfg, toml) = to_config("set simple_search = \"~f %s | ~s %s | ~b %s\"\n");
        assert_eq!(
            cfg.mail.simple_search.as_deref(),
            Some("~f %s | ~s %s | ~b %s"),
            "{toml}"
        );
    }

    #[test]
    fn search_context_carries_over() {
        let (cfg, toml) = to_config("set search_context = 3\n");
        assert_eq!(cfg.pager.search_context, 3, "{toml}");
    }

    #[test]
    fn hide_thread_subject_carries_over() {
        let (cfg, toml) = to_config("set hide_thread_subject = yes\n");
        assert_eq!(cfg.index.hide_thread_subject, Some(true), "{toml}");
    }

    #[test]
    fn status_chars_carries_over() {
        let (cfg, toml) = to_config("set status_chars = \"-*%A\"\n");
        assert_eq!(cfg.ui.status_chars.as_deref(), Some("-*%A"), "{toml}");
    }

    #[test]
    fn layout_settings_carry_over() {
        let (cfg, toml) = to_config("set status_on_top = yes\nset arrow_cursor = yes\n");
        assert_eq!(cfg.ui.status_on_top, Some(true), "{toml}");
        assert_eq!(cfg.ui.arrow_cursor, Some(true));
    }

    #[test]
    fn history_file_carries_over() {
        let (cfg, toml) = to_config("set history_file = ~/.rmut_history\n");
        assert_eq!(
            cfg.ui.history_file.as_deref(),
            Some("~/.rmut_history"),
            "{toml}"
        );
    }

    #[test]
    fn ts_title_settings_carry_over() {
        let (cfg, toml) = to_config(concat!(
            "set ts_enabled = yes\n",
            "set ts_status_format = \"rmut %f (%m)\"\n",
        ));
        assert_eq!(cfg.ui.set_title, Some(true), "{toml}");
        assert_eq!(cfg.ui.title_format.as_deref(), Some("rmut %f (%m)"));
    }

    #[test]
    fn reply_crypto_settings_carry_over() {
        let (cfg, toml) = to_config(concat!(
            "set crypt_replysign = yes\n",
            "set crypt_replyencrypt = yes\n",
        ));
        assert!(cfg.pgp.reply_sign, "{toml}");
        assert!(cfg.pgp.reply_encrypt);
        assert!(!cfg.pgp.reply_sign_encrypted);
    }

    #[test]
    fn postpone_and_recall_quadoptions_carry_over() {
        let (cfg, toml) = to_config(concat!("set postpone = no\n", "set recall = yes\n",));
        assert_eq!(cfg.mail.postpone.as_deref(), Some("no"), "{toml}");
        assert_eq!(cfg.mail.recall.as_deref(), Some("yes"));
        // The defaults (postpone ask-yes, recall ask) are satisfied,
        // not carried.
        let (cfg, _) = to_config(concat!("set postpone = ask-yes\n", "set recall = ask-no\n",));
        assert_eq!(cfg.mail.postpone, None);
        assert_eq!(cfg.mail.recall, None);
    }

    #[test]
    fn the_envelope_settings_carry_over() {
        let (cfg, toml) = to_config(concat!(
            "set hostname = mail.example.net\n",
            "set user_agent = yes\n",
            "set sig_on_top = yes\n",
        ));
        assert_eq!(
            cfg.mail.hostname.as_deref(),
            Some("mail.example.net"),
            "{toml}"
        );
        assert_eq!(cfg.mail.user_agent, Some(true));
        assert_eq!(cfg.mail.sig_on_top, Some(true));
    }

    #[test]
    fn the_trust_settings_carry_over() {
        let (cfg, toml) = to_config(concat!(
            "set certificate_file = ~/.mutt/certs.pem\n",
            "set ssl_usesystemcerts = no\n",
        ));
        assert_eq!(
            cfg.net.certificate_file.as_deref(),
            Some("~/.mutt/certs.pem"),
            "{toml}"
        );
        assert!(!cfg.net.system_cas, "{toml}");
        // ssl_ca_certificates_file is the same slot, and yes is
        // satisfied rather than carried.
        let (cfg, _) = to_config(concat!(
            "set ssl_ca_certificates_file = /etc/ssl/roots.pem\n",
            "set ssl_usesystemcerts = yes\n",
        ));
        assert_eq!(
            cfg.net.certificate_file.as_deref(),
            Some("/etc/ssl/roots.pem")
        );
        assert!(cfg.net.system_cas);
    }

    #[test]
    fn imap_folder_becomes_an_account() {
        let (cfg, toml) = to_config(concat!(
            "set folder = imaps://mail.example.com\n",
            "set spoolfile = +INBOX\n",
            "set imap_user = jane\n",
            "set imap_pass = hunter2\n",
            "set smtp_url = smtps://jane@smtp.example.com:465\n",
            "set record = +Sent\n",
            "mailboxes +INBOX +Archive\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert_eq!(acct.imap_host, Some("mail.example.com".into()));
        assert_eq!(acct.user, "jane");
        assert_eq!(acct.smtp_host, Some("smtp.example.com".into()));
        assert_eq!(acct.smtp_port, 465);
        assert_eq!(acct.sent_folder, "Sent");
        assert_eq!(
            cfg.mail.mailboxes,
            vec!["imap:mutt/INBOX", "imap:mutt/Archive"]
        );
        // imap_pass carries over as the stored password.
        assert_eq!(acct.password.as_deref(), Some("hunter2"));
        assert_eq!(acct.password().unwrap(), "hunter2");
        assert!(toml.contains("consider password_command"), "{toml}");
    }

    #[test]
    fn plain_imap_url_means_starttls_port_never_disabled_tls() {
        let (cfg, toml) = to_config(concat!(
            "set folder = imap://mail.example.com\n",
            "set imap_user = u\n",
            "set smtp_url = smtps://smtp.example.com\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert_eq!(acct.imap_port, 143);
        assert!(acct.imap_tls, "STARTTLS, not plaintext");
        assert!(!toml.contains("imap_tls"), "{toml}");
        assert_eq!(acct.smtp_port, 465);
    }

    #[test]
    fn sort_pager_and_forward_directives_import() {
        let (cfg, toml) = to_config(concat!(
            "set sort                = \"threads\"\n",
            "set sort_aux            = last-date-sent\n",
            "set date_format         = \"%d.%m.%Y\"\n",
            "set pager_index_lines   = 10\n",
            "set pager_context       = 3\n",
            "set mime_forward        = yes\n",
            "set mime_forward_rest   = yes\n",
            "set query_command       = \"khard email --parsable %s\"\n",
            "set trash               = +Trash\n",
            "set edit_headers        = yes\n",
            "save-hook . +General\n",
            "set folder = ~/Mail\n",
            "bind index G imap-fetch-mail\n",
        ));
        assert_eq!(cfg.index.sort.as_deref(), Some("threads"));
        assert_eq!(cfg.index.sort_aux.as_deref(), Some("last-date-sent"));
        assert_eq!(cfg.index.date_format.as_deref(), Some("%d.%m.%Y"));
        assert_eq!(cfg.pager.index_lines, 10);
        assert_eq!(cfg.pager.context, 3);
        assert_eq!(cfg.mail.forward.as_deref(), Some("attach"));
        assert_eq!(
            cfg.mail.query_command.as_deref(),
            Some("khard email --parsable %s")
        );
        assert_eq!(cfg.mail.save.as_deref(), Some("~/Mail/General"));
        assert_eq!(cfg.mail.trash.as_deref(), Some("~/Mail/Trash"));
        assert_eq!(cfg.mail.edit_headers, Some(true));
        // edit_headers = no matches rmut's default.
        let (cfg2, _) = to_config("set edit_headers = no\n");
        assert!(cfg2.mail.edit_headers.is_none());
        assert_eq!(
            cfg.keys.index.get("fetch-mail").map(String::as_str),
            Some("G")
        );
        assert!(toml.contains("mime_forward_rest"), "{toml}");
        assert!(!toml.contains("# not imported"), "{toml}");
    }

    #[test]
    fn auto_view_and_peek_and_tagged_color() {
        let (cfg, toml) = to_config(concat!(
            "auto_view application/zip\n",
            "auto_view text/x-patch text/x-diff\n",
            "auto_view application/pgp-signature application/pgp\n",
            "auto_view text/html\n",
            "auto_view text/calendar\n",
            "unauto_view text/calendar\n",
            "alternative_order text/enriched text/plain TEXT/HTML\n",
            "unalternative_order text/enriched\n",
            "set imap_peek           = yes\n",
            "set menu_scroll\n",
            "bind index % noop\n",
            "color index black   cyan    \"~T\"\n",
        ));
        // Every auto_view type becomes a [filters] entry; the command
        // is left empty, which is rmut's "take it from mailcap", the
        // very place mutt takes it from.
        for mime in [
            "application/zip",
            "text/x-patch",
            "text/x-diff",
            "text/html",
        ] {
            assert_eq!(
                cfg.filters.get(mime).map(String::as_str),
                Some(""),
                "{mime}"
            );
        }
        // unauto_view takes one back off.
        assert!(!cfg.filters.contains_key("text/calendar"), "{toml}");
        assert_eq!(cfg.pager.alternative_order, ["text/plain", "text/html"]);
        // black-on-cyan keeps both colours, as a rule: the slot holds
        // one, and mutt painted a bar.
        let tagged = cfg
            .color_index
            .iter()
            .find(|r| r.pattern == "~T")
            .expect("a ~T rule");
        assert_eq!(tagged.fg.as_deref(), Some("black"));
        assert_eq!(tagged.bg.as_deref(), Some("cyan"));
        assert!(!cfg.colors.contains_key("tagged"));
        assert!(toml.contains("# satisfied by rmut's defaults"), "{toml}");
        for satisfied in ["pgp-signature", "imap_peek", "menu_scroll", "noop"] {
            assert!(toml.contains(satisfied), "{satisfied} missing:\n{toml}");
        }
        assert!(toml.contains("application/zip"), "{toml}");
    }

    #[test]
    fn imap_pass_without_imap_folder_is_redacted() {
        let (cfg, toml) = to_config("set folder = ~/Mail\nset imap_pass = hunter2\n");
        assert!(cfg.accounts.is_empty());
        assert!(!toml.contains("hunter2"), "password must not leak:\n{toml}");
        assert!(toml.contains("(redacted)"), "{toml}");
    }

    #[test]
    fn url_style_spoolfile_record_and_mailboxes() {
        // The common mutt style: full URLs everywhere, no +shortcuts.
        let (cfg, toml) = to_config(concat!(
            "set folder = \"imaps://mail.example.com/\"\n",
            "set spoolfile = \"imaps://mail.example.com/INBOX\"\n",
            "set record = \"imaps://mail.example.com/Sent\"\n",
            "set imap_user = jane\n",
            "mailboxes imaps://mail.example.com/INBOX imaps://mail.example.com/Archive\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert_eq!(acct.imap_host, Some("mail.example.com".into()));
        assert_eq!(acct.sent_folder, "Sent");
        assert_eq!(
            cfg.mail.mailboxes,
            vec!["imap:mutt/INBOX", "imap:mutt/Archive"]
        );
        assert!(!toml.contains("//INBOX"), "no doubled separators:\n{toml}");
    }

    #[test]
    fn url_with_userinfo_and_empty_port() {
        // Seen in the wild: user@ in the URL and a dangling colon.
        let (cfg, toml) = to_config(concat!(
            "set folder = \"imap://jane@mail.example.com:/\"\n",
            "set spoolfile = \"imap://jane@mail.example.com:/INBOX\"\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert_eq!(acct.user, "jane");
        assert_eq!(acct.imap_host, Some("mail.example.com".into()));
        assert_eq!(acct.imap_port, 143);
        assert!(acct.imap_tls, "imap:// means STARTTLS, not plaintext");
        assert_eq!(cfg.mail.mailboxes, vec!["imap:mutt/INBOX"]);
        assert!(
            !toml.contains("jane@mail.example.com"),
            "no URLs in specs:\n{toml}"
        );
    }

    #[test]
    fn url_spoolfile_alone_identifies_the_server() {
        let (cfg, _) = to_config(concat!(
            "set spoolfile = imaps://mail.example.com:1993/INBOX\n",
            "set imap_user = jane\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert_eq!(acct.imap_host, Some("mail.example.com".into()));
        assert_eq!(acct.imap_port, 1993);
        assert_eq!(cfg.mail.mailboxes, vec!["imap:mutt/INBOX"]);
    }

    #[test]
    fn smtp_only_muttrc_still_gets_an_account() {
        let (cfg, _) = to_config(concat!(
            "set folder = ~/Mail\n",
            "set from = jane@x\n",
            "set smtp_url = smtp://smtp.example.com:587\n",
            "set smtp_pass = sekrit\n",
        ));
        let acct = cfg.account("mutt").unwrap();
        assert!(acct.imap_host.is_none());
        assert_eq!(acct.smtp_host, Some("smtp.example.com".into()));
        assert_eq!(acct.user, "jane@x");
        assert_eq!(acct.password.as_deref(), Some("sekrit"));
    }

    #[test]
    fn differing_smtp_pass_is_noted_and_redacted() {
        let (cfg, toml) = to_config(concat!(
            "set folder = imaps://h\n",
            "set imap_user = u\n",
            "set imap_pass = aaa\n",
            "set smtp_url = smtp://s\n",
            "set smtp_pass = bbb\n",
        ));
        assert_eq!(
            cfg.account("mutt").unwrap().password.as_deref(),
            Some("aaa")
        );
        assert!(!toml.contains("bbb"), "smtp_pass must not leak:\n{toml}");
        assert!(toml.contains("differs from imap_pass"), "{toml}");
    }

    #[test]
    fn default_matching_directives_are_acknowledged() {
        let (cfg, toml) = to_config(concat!(
            "set ssl_starttls        = yes\n",
            "set ssl_force_tls       = yes\n",
            "set charset             = \"UTF-8\"\n",
            "set pgp_auto_decode     = yes\n",
            "set smtp_authenticators =\"login\"\n",
            "set print_command       = \"a2ps\"\n",
        ));
        assert_eq!(cfg.mail.print.as_deref(), Some("a2ps"));
        assert!(toml.contains("# satisfied by rmut's defaults"), "{toml}");
        for directive in [
            "ssl_starttls",
            "ssl_force_tls",
            "charset",
            "pgp_auto_decode",
            "smtp_authenticators",
        ] {
            assert!(toml.contains(directive), "{directive} missing:\n{toml}");
        }
        assert!(!toml.contains("# not imported"), "{toml}");
    }

    #[test]
    fn non_default_tls_and_charset_still_surface() {
        let (_cfg, toml) = to_config(concat!(
            "set ssl_force_tls = no\n",
            "set charset = \"iso-8859-2\"\n",
            "set smtp_authenticators = \"oauthbearer\"\n",
        ));
        assert!(toml.contains("# not imported:"), "{toml}");
        assert!(!toml.contains("# satisfied"), "{toml}");
    }

    #[test]
    fn account_without_imap_pass_gets_a_placeholder() {
        let (cfg, toml) = to_config("set folder = imaps://h.example.com\nset imap_user = u\n");
        assert!(cfg.account("mutt").unwrap().password_command.is_some());
        assert!(toml.contains("pass show mail/TODO"), "{toml}");
    }

    #[test]
    fn aliases_and_unknowns_surface_as_comments() {
        let import = import(
            "alias petr Petr Novak <petr@example.com>\nset sleep_time = 0\nmacro index x \"<shell-escape>ls\\n\"\n",
            Path::new("/"),
        );
        // Aliases come back on their own: rmut keeps them in a
        // mutt-format alias file, so the caller either writes that
        // file or shows them with alias_block.
        assert_eq!(import.aliases, ["alias petr Petr Novak <petr@example.com>"]);
        assert!(!import.toml.contains("alias petr"), "{}", import.toml);
        let block = alias_block(&import.aliases, Path::new("/home/x/.config/rmut/aliases"));
        assert!(
            block.contains("#   alias petr Petr Novak <petr@example.com>"),
            "{block}"
        );
        assert!(block.contains("/home/x/.config/rmut/aliases"), "{block}");
        assert!(alias_block(&[], Path::new("/x")).is_empty());
        assert!(import.toml.contains("set sleep_time = 0"));
        assert!(import.toml.contains("macro index x"), "{}", import.toml);
        assert!(import.toml.contains("mutt function names"));
        // and the output is still valid (if empty) config
        toml::from_str::<Config>(&import.toml).unwrap();
    }

    #[test]
    fn source_includes_are_followed() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("extra"), "set realname = Included\n").unwrap();
        let import = import("source extra\nsource ./missing\n", tmp.path());
        assert!(import.toml.contains("name = \"Included\""));
        assert!(import.toml.contains("source ./missing"));
    }

    #[test]
    fn continuations_and_quoting() {
        let lines = logical_lines("set realname = \\\n  \"Jane # not a comment\"\n# gone\n");
        assert_eq!(lines, vec!["set realname =   \"Jane # not a comment\""]);
        assert_eq!(
            tokenize("bind index \\Cd delete-message"),
            vec!["bind", "index", "\\Cd", "delete-message"]
        );
        assert_eq!(tokenize("set from='a b' c"), vec!["set", "from=a b", "c"]);
    }

    #[test]
    fn convert_key_forms() {
        assert_eq!(convert_key("\\Cx").as_deref(), Some("ctrl+x"));
        assert_eq!(convert_key("\\CX").as_deref(), Some("ctrl+x"));
        assert_eq!(convert_key("\\ev").as_deref(), Some("alt+v"));
        assert_eq!(convert_key("<esc>V").as_deref(), Some("alt+V"));
        assert_eq!(convert_key("<Enter>").as_deref(), Some("enter"));
        assert_eq!(convert_key("<PageDown>").as_deref(), Some("pgdn"));
        assert_eq!(convert_key("G").as_deref(), Some("G"));
        assert_eq!(convert_key("gg"), None);
        assert_eq!(convert_key("<f5>"), None);
    }
}
