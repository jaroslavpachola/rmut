//! Muttrc importer: translate the common muttrc directives into rmut's
//! TOML config. Meant for a one-time `rmut --import-muttrc` run whose
//! output the user reviews and saves — everything that does not map is
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

#[derive(Default)]
struct State {
    name: Option<String>,
    email: Option<String>,
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
    keys_index: BTreeMap<&'static str, String>,
    keys_pager: BTreeMap<&'static str, String>,
    sign_key: Option<String>,
    sign_by_default: bool,
    encrypt_by_default: bool,
    print: Option<String>,
    imap_user: Option<String>,
    imap_pass: Option<String>,
    smtp_pass: Option<String>,
    smtp_url: Option<String>,
    aliases: Vec<String>,
    skipped: Vec<String>,
    /// Directives that match what rmut always does — acknowledged in
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
            "bind" => st.bind(&tokens[1..], &line),
            "color" => st.color(&tokens[1..], &line),
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
fn tokenize(line: &str) -> Vec<String> {
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
fn assignments(tokens: &[String]) -> Vec<(String, String)> {
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

fn is_yes(value: &str) -> bool {
    matches!(value, "yes" | "ask-yes" | "true" | "1")
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

impl State {
    fn skip(&mut self, line: &str, why: &str) {
        self.skipped.push(format!("{line}  ({why})"));
    }

    fn satisfy(&mut self, line: &str, why: &str) {
        self.satisfied.push(format!("{line}  ({why})"));
    }

    fn set(&mut self, name: &str, value: &str, line: &str) {
        let v = value.to_string();
        match name {
            "realname" => self.name = Some(v),
            "from" => {
                // "Jane Doe <jane@x>" or a bare address.
                if let Some((n, rest)) = v.split_once('<') {
                    let n = n.trim().trim_matches('"');
                    if !n.is_empty() {
                        self.name.get_or_insert_with(|| n.to_string());
                    }
                    self.email = Some(rest.trim_end_matches('>').trim().to_string());
                } else {
                    self.email = Some(v);
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
            "smtp_authenticators" => {
                let m = v.to_lowercase();
                if m.contains("plain") || m.contains("login") {
                    self.satisfy(line, "rmut negotiates AUTH PLAIN/LOGIN by itself");
                } else {
                    self.skip(line, "rmut supports only AUTH PLAIN and LOGIN");
                }
            }
            _ => self.skip(line, "no rmut equivalent"),
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
            self.skip(line, "noop bindings are not needed");
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

    fn color(&mut self, args: &[String], line: &str) {
        let (Some(object), Some(fg), Some(bg)) = (args.first(), args.get(1), args.get(2)) else {
            self.skip(line, "unrecognized color syntax");
            return;
        };
        let fg = convert_color(fg);
        let bg = convert_color(bg);
        match (object.as_str(), args.get(3).map(String::as_str)) {
            ("status", _) => {
                self.colors.insert("status_fg", fg);
                self.colors.insert("status_bg", bg);
            }
            ("header" | "hdrdefault", _) => {
                self.colors.insert("header", fg);
            }
            ("index", Some("~D")) => {
                self.colors.insert("deleted", fg);
            }
            ("index", Some("~F")) => {
                self.colors.insert("flagged", fg);
            }
            _ => self.skip(line, "no rmut color slot"),
        }
    }

    // ---- output ----

    fn to_toml(&self) -> String {
        let mut out = String::from("# generated by rmut --import-muttrc; review before use\n");
        if self.name.is_some() || self.email.is_some() {
            out += "\n[identity]\n";
            if let Some(n) = &self.name {
                out += &format!("name = {}\n", quote(n));
            }
            if let Some(e) = &self.email {
                out += &format!("email = {}\n", quote(e));
            }
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
        {
            out += "\n[mail]\n";
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
        }
        if let Some(f) = &self.index_format {
            out += &format!(
                "\n[index]\n# rmut supports %C %Z %d %F %c %s of mutt's specifiers\nformat = {}\n",
                quote(f)
            );
        }
        if !self.colors.is_empty() {
            out += "\n[colors]\n";
            for (k, v) in &self.colors {
                out += &format!("{k} = {}\n", quote(v));
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
            out += "\n# aliases found — rmut reads mutt-format alias files; put these\n";
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

    fn account_toml(&self, folder_url: Option<&str>, skipped: &mut Vec<String>) -> String {
        let mut out = format!("\n[[accounts]]\nname = {}\n", quote(ACCOUNT));
        let user = self
            .imap_user
            .clone()
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| "TODO".into());
        out += &format!("user = {}\n", quote(&user));
        // One credential per account: imap_pass, or smtp_pass when
        // it is the only one given.
        match (&self.imap_pass, &self.smtp_pass) {
            (Some(a), Some(b)) if a != b => {
                out += "# imported from imap_pass; consider password_command instead\n";
                out += &format!("password = {}\n", quote(a));
                skipped.push(
                    "set smtp_pass = (redacted)  (differs from imap_pass; rmut uses one password per account)"
                        .into(),
                );
            }
            (Some(pass), _) | (None, Some(pass)) => {
                out += "# imported from imap_pass/smtp_pass; consider password_command instead\n";
                out += &format!("password = {}\n", quote(pass));
            }
            (None, None) => {
                out += "# TODO: set a command that prints the password (or password = \"...\"):\n";
                out += "password_command = \"pass show mail/TODO\"\n";
            }
        }
        if let Some(url) = folder_url {
            let (host, port, tls) = split_url(url);
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
            let (host, port, tls) = split_url(smtp);
            let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
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

/// host, explicit port, and whether the scheme implies TLS. Any
/// mailbox path in the URL is ignored here (see `url_mailbox`).
fn split_url(url: &str) -> (&str, Option<u16>, bool) {
    let (tls, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.ends_with('s'), rest),
        None => (true, url),
    };
    let rest = rest.split('/').next().unwrap_or(rest);
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            (host, port.parse().ok(), tls)
        }
        _ => (rest, None, tls),
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
fn convert_key(key: &str) -> Option<String> {
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

/// mutt "bright" colors are close to ratatui's "light" family.
fn convert_color(name: &str) -> String {
    match name.strip_prefix("bright") {
        Some(base) => format!("light{base}"),
        None => name.to_string(),
    }
}

fn index_function(name: &str) -> Option<&'static str> {
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
        "change-folder" => "change-mailbox",
        "view-attachments" => "attachments",
        "collapse-thread" => "fold-thread",
        "collapse-all" => "fold-all",
        "help" => "help",
        _ => return None,
    })
}

fn pager_function(name: &str) -> Option<&'static str> {
    Some(match name {
        "exit" => "back",
        "next-line" => "down",
        "previous-line" => "up",
        "next-page" => "page-down",
        "previous-page" => "page-up",
        "top" => "top",
        "bottom" => "bottom",
        "next-entry" | "next-undeleted" => "next",
        "previous-entry" | "previous-undeleted" => "previous",
        "delete-message" => "delete",
        "display-toggle-weed" => "headers",
        "view-attachments" => "attachments",
        "mail" => "compose",
        "reply" => "reply",
        "group-reply" => "group-reply",
        "forward-message" => "forward",
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
            "alias petr Petr Novak <petr@example.com>\nset sort = threads\nmacro index x y\n",
            Path::new("/"),
        );
        assert_eq!(import.aliases.len(), 1);
        assert!(
            import
                .toml
                .contains("#   alias petr Petr Novak <petr@example.com>")
        );
        assert!(import.toml.contains("set sort = threads"));
        assert!(import.toml.contains("macro index x y"));
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
