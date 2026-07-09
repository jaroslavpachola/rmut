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
    imap_user: Option<String>,
    smtp_url: Option<String>,
    aliases: Vec<String>,
    skipped: Vec<String>,
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
            "imap_user" => self.imap_user = Some(v),
            "smtp_url" => self.smtp_url = Some(v),
            "imap_pass" | "smtp_pass" => {
                // Redact — the skip comment must not echo the secret.
                self.skip(
                    &format!("set {name} = (redacted)"),
                    "passwords are not imported; use password_command",
                );
            }
            _ => self.skip(line, "no rmut equivalent"),
        }
    }

    /// mutt's +x / =x mean "under $folder"; for an IMAP folder that is
    /// an account folder spec, locally a joined path.
    fn expand_mailbox(&self, value: &str) -> String {
        let Some(rest) = value.strip_prefix(['+', '=']) else {
            return value.to_string();
        };
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
        let imap = self.folder.as_deref().filter(|f| is_imap_url(f));
        let mut mailboxes = Vec::new();
        if let Some(spool) = &self.spoolfile {
            mailboxes.push(match imap {
                Some(_) => {
                    let inbox = spool.trim_start_matches(['+', '=']);
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
        if let Some(url) = imap {
            out += &self.account_toml(url);
        }
        if !self.aliases.is_empty() {
            out += "\n# aliases found — rmut reads mutt-format alias files; put these\n";
            out += "# lines in ~/.config/rmut/aliases (or point $RMUT_ALIASES at them):\n";
            for a in &self.aliases {
                out += &format!("#   {a}\n");
            }
        }
        if !self.skipped.is_empty() {
            out += "\n# not imported:\n";
            for s in &self.skipped {
                out += &format!("#   {s}\n");
            }
        }
        out
    }

    fn account_toml(&self, folder_url: &str) -> String {
        let mut out = format!("\n[[accounts]]\nname = {}\n", quote(ACCOUNT));
        let user = self
            .imap_user
            .clone()
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| "TODO".into());
        out += &format!("user = {}\n", quote(&user));
        out += "# TODO: rmut never stores passwords — set a command that prints it:\n";
        out += "password_command = \"pass show mail/TODO\"\n";
        let (host, port, tls) = split_url(folder_url);
        out += &format!("imap_host = {}\n", quote(host));
        if let Some(p) = port {
            out += &format!("imap_port = {p}\n");
        }
        if !tls {
            out += "imap_tls = false\n";
        }
        if let Some(smtp) = &self.smtp_url {
            let (host, port, _tls) = split_url(smtp);
            let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
            out += &format!("smtp_host = {}\n", quote(host));
            if let Some(p) = port {
                out += &format!("smtp_port = {p}\n");
            }
        }
        if let Some(sent) = &self.sent {
            out += &format!(
                "sent_folder = {}\n",
                quote(sent.trim_start_matches(['+', '=']))
            );
        }
        out
    }
}

fn is_imap_url(value: &str) -> bool {
    value.starts_with("imap://") || value.starts_with("imaps://")
}

/// host, explicit port, and whether the scheme implies TLS.
fn split_url(url: &str) -> (&str, Option<u16>, bool) {
    let (tls, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.ends_with('s'), rest),
        None => (true, url),
    };
    let rest = rest.trim_end_matches('/');
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            (host, port.parse().ok(), tls)
        }
        _ => (rest, None, tls),
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
        assert!(toml.contains("passwords are not imported"));
        assert!(!toml.contains("hunter2"), "password must not leak:\n{toml}");
        assert!(toml.contains("password_command"));
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
