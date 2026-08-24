//! Muttrc importer: translate the common muttrc directives into rmut's
//! TOML config. Meant for a one-time `rmut --import-muttrc` run whose
//! output the user reviews and saves; everything that does not map is
//! kept visible as `# not imported:` comments, never dropped silently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Parsed import: the TOML text plus the alias lines found inline
/// (rmut reads mutt-format alias files, so these just need moving).
pub struct Import {
    pub toml: String,
    pub aliases: Vec<String>,
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
    /// `set text_flowed`: send text/plain; format=flowed.
    text_flowed: bool,
    /// `set noreflow_text`: leave a flowed part's line breaks alone.
    no_reflow_text: bool,
    hdr_unignore: Vec<String>,
    hdr_order: Vec<String>,
    pager_format: Option<String>,
    wrap: Option<i64>,
    tilde: bool,
    status_format: Option<String>,
    no_beep: bool,
    keys_index: BTreeMap<&'static str, String>,
    keys_pager: BTreeMap<&'static str, String>,
    macros_index: BTreeMap<String, String>,
    macros_pager: BTreeMap<String, String>,
    sign_key: Option<String>,
    sign_by_default: bool,
    encrypt_by_default: bool,
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
            "auto_view" => {
                for mime in &tokens[1..] {
                    st.auto_view(mime, &line);
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
            "imap_user" => self.imap_user = Some(v),
            "smtp_url" => self.smtp_url = Some(v),
            "imap_pass" => self.imap_pass = Some(v),
            "smtp_pass" => self.smtp_pass = Some(v),
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
                    "subject" | "size" | "from" => self.sort = Some(format!("{rev}{name}")),
                    _ => self.skip(line, "no matching rmut sort order"),
                }
            }
            "sort_aux" => match v.as_str() {
                "last-date-sent" | "last-date-received" | "reverse-last-date-sent" => {
                    self.sort_aux = Some("last-date-sent".into());
                }
                "date" | "date-sent" | "date-received" => {
                    self.satisfy(line, "threads are ordered oldest-first by default");
                }
                _ => self.skip(line, "only date / last-date-sent map"),
            },
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
            "beep" => {
                if is_yes(&v) {
                    self.satisfy(line, "beeping on errors is rmut's default");
                } else {
                    self.no_beep = true;
                }
            }
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

    fn auto_view(&mut self, mime: &str, line: &str) {
        match mime {
            "text/html" => {
                // The command lives in mailcap, which is not read;
                // w3m is the usual suspect; adjust after import.
                self.filters
                    .entry("text/html".into())
                    .or_insert_with(|| "w3m -dump -T text/html -O UTF-8".into());
            }
            m if m.starts_with("text/") => {
                self.satisfy(line, "text parts already display inline");
            }
            "application/pgp" | "application/pgp-signature" | "application/pgp-encrypted" => {
                self.satisfy(line, "PGP is handled natively");
            }
            _ => self.skip(
                line,
                "add a [filters] entry with a command that reads the part on stdin",
            ),
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
            ("index", Some("~D")) => {
                self.colors.insert("deleted", vivid);
            }
            ("index", Some("~F")) => {
                self.colors.insert("flagged", vivid);
            }
            ("index", Some("~T")) => {
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
        if self.name.is_some() || self.email.is_some() || self.reverse_name {
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
        if !mailboxes.is_empty()
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
            || self.autoedit
            || self.no_copy
            || self.new_mail_command.is_some()
            || self.edit_headers_on
            || !self.lists.is_empty()
            || !self.subscribed.is_empty()
            || !self.alternates.is_empty()
            || !self.my_hdr.is_empty()
            || self.metoo
            || self.text_flowed
        {
            out += "\n[mail]\n";
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
            if self.autoedit {
                out += "autoedit = true\n";
            }
            if self.no_copy {
                out += "copy = false\n";
            }
            if self.metoo {
                out += "metoo = true\n";
            }
            if self.text_flowed {
                out += "text_flowed = true\n";
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
        }
        if self.pager_index_lines.is_some()
            || self.pager_context.is_some()
            || self.quote_regexp.is_some()
            || self.pager_format.is_some()
            || self.wrap.is_some()
            || self.tilde
            || !self.hdr_ignore.is_empty()
            || !self.hdr_unignore.is_empty()
            || !self.hdr_order.is_empty()
            || self.no_reflow_text
        {
            out += "\n[pager]\n";
            if let Some(n) = self.pager_index_lines {
                out += &format!("index_lines = {n}\n");
            }
            if let Some(n) = self.pager_context {
                out += &format!("context = {n}\n");
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
        }
        if !self.filters.is_empty() {
            out += "\n[filters]\n# auto_view: the command reads the part on stdin (from mailcap\n# in mutt; adjust to taste)\n";
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
        if self.status_format.is_some() || self.no_beep {
            out += "\n[ui]\n";
            if let Some(sf) = &self.status_format {
                out += "# rmut renders %f %m %M %n %u %d %F %t %s %V %r %v and\n";
                out += "# %?X?then&else? conditionals; other specifiers show literally\n";
                out += &format!("status_format = {}\n", quote(sf));
            }
            if self.no_beep {
                out += "beep = false\n";
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
        if self.sign_key.is_some() || self.sign_by_default || self.encrypt_by_default {
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
        if !self.aliases.is_empty() {
            out += "\n# aliases found: rmut reads mutt-format alias files; put these\n";
            out += "# lines in ~/.config/rmut/aliases (or point $RMUT_ALIASES at them):\n";
            for a in &self.aliases {
                out += &format!("#   {a}\n");
            }
        }
        if !self.satisfied.is_empty() {
            out += "\n# satisfied by rmut's defaults (nothing to configure):\n";
            for s in &self.satisfied {
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
        "imap-fetch-mail" | "fetch-mail" => "fetch-mail",
        "tag-entry" | "tag-message" => "tag",
        "tag-prefix" => "tag-prefix",
        "query" => "query",
        "save-message" => "save",
        "print-message" => "print",
        "edit" => "edit",
        "resend-message" => "resend",
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
            "set imap_peek           = yes\n",
            "set menu_scroll\n",
            "bind index % noop\n",
            "color index black   cyan    \"~T\"\n",
        ));
        assert!(
            cfg.filters.get("text/html").unwrap().contains("w3m"),
            "html filter"
        );
        // black-on-cyan: the background is the visible color.
        assert_eq!(cfg.colors.get("tagged").map(String::as_str), Some("cyan"));
        assert!(toml.contains("# satisfied by rmut's defaults"), "{toml}");
        for satisfied in [
            "x-patch",
            "pgp-signature",
            "imap_peek",
            "menu_scroll",
            "noop",
        ] {
            assert!(toml.contains(satisfied), "{satisfied} missing:\n{toml}");
        }
        // zip and calendar have no obvious command: they stay visible.
        assert!(toml.contains("application/zip"), "{toml}");
        assert!(toml.contains("text/calendar"), "{toml}");
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
        assert_eq!(import.aliases.len(), 1);
        assert!(
            import
                .toml
                .contains("#   alias petr Petr Novak <petr@example.com>")
        );
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
