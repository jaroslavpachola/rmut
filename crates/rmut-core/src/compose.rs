//! Draft building and finalizing for outgoing mail.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{Local, TimeZone};

use crate::pattern::Me;

pub struct DraftHeaders {
    /// From line when an identity override applies (reverse_name, a
    /// folder/recipient rule, the account); the user can edit it, and
    /// finalize falls back to the default identity when absent.
    pub from: Option<String>,
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// The text the user edits in $EDITOR: header block, blank line, body.
pub fn draft_text(h: &DraftHeaders, body: &str) -> String {
    let mut out = String::new();
    if let Some(from) = &h.from {
        out += &format!("From: {from}\n");
    }
    out += &format!("To: {}\n", h.to);
    if let Some(cc) = &h.cc
        && !cc.trim().is_empty()
    {
        out += &format!("Cc: {cc}\n");
    }
    out += &format!("Subject: {}\n", h.subject);
    if let Some(x) = &h.in_reply_to {
        out += &format!("In-Reply-To: {x}\n");
    }
    if let Some(x) = &h.references {
        out += &format!("References: {x}\n");
    }
    out.push('\n');
    out.push_str(body);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// mutt's $attribution: the line a quoted reply opens with.
pub const DEFAULT_ATTRIBUTION: &str = "On %d, %n wrote:";

/// mutt's $forward_format: the subject a forward carries.
pub const DEFAULT_FORWARD_FORMAT: &str = "[%a: %s]";

/// mutt's $indent_string: what a quoted line is prefixed with.
pub const DEFAULT_INDENT: &str = "> ";

/// The message an attribution or a forward subject is about, for the
/// format strings that describe it.
pub struct Quoted<'a> {
    /// The From header as written, name and address together.
    pub from: &'a str,
    pub subject: &'a str,
    pub message_id: Option<&'a str>,
    /// When it was sent, seconds since the epoch.
    pub date: i64,
}

impl Quoted<'_> {
    /// The author's display name, or their address when the header
    /// carries no name (as mutt's %n does).
    fn name(&self) -> String {
        let trimmed = self.from.trim();
        let name = match trimmed.split_once('<') {
            Some((name, _)) => name.trim().trim_matches('"').trim(),
            None => "",
        };
        match name.is_empty() {
            true => self.address(),
            false => name.to_string(),
        }
    }

    fn address(&self) -> String {
        bare_address(self.from).unwrap_or_else(|| self.from.trim().to_string())
    }
}

/// Expand one of mutt's message format strings: `%a` the author's
/// address, `%n` their name, `%s` the subject, `%i` the message-id,
/// `%d` the date, `%{...}` the date through strftime, `%%` a percent.
/// Padding and conditionals work as they do in the index format.
pub fn render_quoted(fmt: &str, m: &Quoted) -> String {
    // `%{...}` first: the strftime span would confuse the specifier
    // machinery, which reads one character.
    let fmt = expand_strftime(fmt, m.date);
    crate::format::render_with(&fmt, &|spec| match spec {
        'a' => m.address(),
        'n' => m.name(),
        'f' => m.from.trim().to_string(),
        's' => m.subject.trim().to_string(),
        'i' => m
            .message_id
            .unwrap_or_default()
            .trim_matches(['<', '>'])
            .to_string(),
        'd' => format_date(m.date),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    })
}

/// mutt's `%{strftime}`: the message's date, in the caller's words.
fn expand_strftime(fmt: &str, date: i64) -> String {
    let mut out = String::new();
    let mut rest = fmt;
    while let Some(at) = rest.find("%{") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at + 2..].find('}') else {
            break;
        };
        let spec = &rest[at + 2..at + 2 + end];
        out.push_str(&strftime(spec, date));
        rest = &rest[at + 2 + end + 1..];
    }
    out.push_str(rest);
    out
}

fn strftime(spec: &str, epoch: i64) -> String {
    match Local.timestamp_opt(epoch, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format(spec).to_string()
        }
        chrono::LocalResult::None => String::new(),
    }
}

/// mutt's $reply_regexp default: "Re:" with an optional bracketed
/// count, as Re[2]: has it.
pub const REPLY_REGEXP: &str = r"^(re)(\[[0-9]+\])*:[ \t]*";

/// Compile a $reply_regexp the way mutt does: case-insensitively
/// unless the pattern itself has an uppercase letter (mutt's
/// mutt_which_case).
pub fn reply_regexp(spec: &str) -> Result<regex_lite::Regex, regex_lite::Error> {
    let smart = if spec.chars().any(char::is_uppercase) {
        spec.to_string()
    } else {
        format!("(?i){spec}")
    };
    regex_lite::Regex::new(&smart)
}

pub fn default_reply_regexp() -> regex_lite::Regex {
    reply_regexp(REPLY_REGEXP).expect("default reply_regexp compiles")
}

/// The subject of a reply, as mutt makes it: "Re: " over the original
/// with whatever $reply_regexp matched at its start taken off, so an
/// "RE: x" or "Re[2]: x" answers as "Re: x" rather than piling up.
pub fn reply_subject(orig: &str, re: &regex_lite::Regex) -> String {
    let t = orig.trim();
    let rest = match re.find(t) {
        Some(m) if m.start() == 0 => t[m.end()..].trim_start(),
        _ => t,
    };
    format!("Re: {rest}")
}

/// mutt's $forward_format over the message being forwarded.
pub fn forward_subject(fmt: &str, m: &Quoted) -> String {
    render_quoted(fmt, m)
}

fn format_date(epoch: i64) -> String {
    match Local.timestamp_opt(epoch, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format("%a, %d %b %Y %H:%M").to_string()
        }
        chrono::LocalResult::None => "an unknown date".into(),
    }
}

/// mutt's $attribution over the message being replied to.
pub fn attribution(fmt: &str, m: &Quoted) -> String {
    render_quoted(fmt, m)
}

/// Quote a body mutt-style under an attribution line, each line
/// prefixed with $indent_string.
pub fn quote(attribution: &str, indent: &str, body: &str) -> String {
    let mut out = format!("{attribution}\n");
    for line in body.lines() {
        out += &format!("{indent}{line}\n");
    }
    out
}

/// The forwarded original between mutt's markers. With mutt's
/// $forward_quote the message itself is prefixed with $indent_string,
/// the way a reply is quoted; the markers stay flush, since they are
/// rmut's words and not the original's.
pub fn forward_body(
    from: &str,
    date_epoch: i64,
    subject: &str,
    body: &str,
    quote_with: Option<&str>,
) -> String {
    let body = body.trim_end();
    let body = match quote_with {
        Some(indent) => body
            .lines()
            .map(|l| format!("{indent}{l}"))
            .collect::<Vec<_>>()
            .join("\n"),
        None => body.to_string(),
    };
    format!(
        "----- Forwarded message from {from} -----\nDate: {}\nSubject: {subject}\n\n{body}\n----- End forwarded message -----\n",
        format_date(date_epoch),
    )
}

/// mutt's $signature: the text a draft ends with. A name ending in
/// `|` is a command whose standard output is the signature (mutt's
/// rule); anything else is a file. A signature that cannot be read is
/// no signature: a draft is worth more than the ornament on it.
pub fn signature_text(setting: &str) -> Option<String> {
    let setting = setting.trim();
    if setting.is_empty() {
        return None;
    }
    let text = match setting.strip_suffix('|') {
        Some(command) => {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(command.trim())
                .output()
                .ok()?;
            String::from_utf8_lossy(&out.stdout).into_owned()
        }
        None => std::fs::read_to_string(expand_home(setting)).ok()?,
    };
    let text = text.trim_end_matches('\n');
    (!text.is_empty()).then(|| text.to_string())
}

/// The body with the signature under it, mutt's way: a blank line,
/// then $sig_dashes' "-- " line when it is on, then the signature.
pub fn with_signature(body: &str, signature: &str, dashes: bool) -> String {
    with_signature_at(body, signature, dashes, false)
}

/// The signature added to a draft. mutt's $sig_on_top puts it above
/// the quoted original instead of below (with a blank line between,
/// so the reply is written between the signature and the quote).
pub fn with_signature_at(body: &str, signature: &str, dashes: bool, on_top: bool) -> String {
    let mut sig = String::new();
    if dashes {
        sig += "-- \n";
    }
    sig += signature;
    if !sig.ends_with('\n') {
        sig.push('\n');
    }
    if on_top {
        return format!("{sig}\n{body}");
    }
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out += &sig;
    out
}

pub fn make_message_id(hostname: &str) -> String {
    format!(
        "<{}.{}.rmut@{hostname}>",
        Local::now().timestamp_millis(),
        std::process::id(),
    )
}

pub fn rfc2822_now() -> String {
    Local::now().to_rfc2822()
}

fn header_present(head: &str, name: &str) -> bool {
    head.lines().any(|l| {
        l.get(..name.len())
            .is_some_and(|k| k.eq_ignore_ascii_case(name))
            && l.as_bytes().get(name.len()) == Some(&b':')
    })
}

/// Finalize an edited draft for sending: require a recipient, add
/// mutt's $user_agent header, when the caller asks for it.
pub fn user_agent_header() -> String {
    format!("User-Agent: rmut/{}", env!("CARGO_PKG_VERSION"))
}

/// From/Date/Message-ID when the user didn't write them.
pub fn finalize(draft: &str, from: &str, msg_id: &str, date: &str) -> Result<String> {
    finalize_with(draft, from, msg_id, date, false)
}

/// Like `finalize`, adding a `User-Agent` header when `user_agent`
/// and the draft has none of its own.
pub fn finalize_with(
    draft: &str,
    from: &str,
    msg_id: &str,
    date: &str,
    user_agent: bool,
) -> Result<String> {
    let (head, body) = draft.split_once("\n\n").unwrap_or((draft.trim_end(), ""));
    let has_recipients = head.lines().any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            ["to", "cc", "bcc"].contains(&k.trim().to_lowercase().as_str()) && !v.trim().is_empty()
        })
    });
    ensure!(has_recipients, "no recipients (To/Cc/Bcc)");
    let mut head = head.trim_end().to_string();
    if !header_present(&head, "From") {
        head += &format!("\nFrom: {from}");
    }
    if !header_present(&head, "Date") {
        head += &format!("\nDate: {date}");
    }
    if !header_present(&head, "Message-ID") {
        head += &format!("\nMessage-ID: {msg_id}");
    }
    if user_agent && !header_present(&head, "User-Agent") {
        head += &format!("\n{}", user_agent_header());
    }
    Ok(format!("{head}\n\n{body}"))
}

/// A file named in an `Attach:` pseudo-header of the draft.
pub struct Attachment {
    pub path: PathBuf,
    /// Content-type override (compose menu ctrl+t); guessed from the
    /// extension otherwise.
    pub mime: Option<String>,
    pub description: Option<String>,
    /// mutt's rename-attachment: the filename the part is sent under,
    /// when not the file's own. `@name="..."` on the line.
    pub name: Option<String>,
    /// mutt's toggle-disposition: Content-Disposition inline rather
    /// than attachment. `@inline` on the line.
    pub inline: bool,
    /// mutt's toggle-unlink: the file goes once the message has been
    /// sent. `@unlink` on the line.
    pub unlink: bool,
}

impl Attachment {
    /// A plain attachment of this file: nothing overridden.
    pub fn of(path: PathBuf) -> Attachment {
        Attachment {
            path,
            mime: None,
            description: None,
            name: None,
            inline: false,
            unlink: false,
        }
    }

    /// The filename the part goes out under.
    pub fn send_name(&self) -> &str {
        self.name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                self.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("attachment")
            })
    }
}

/// A `type/subtype` token (letters, digits, `.+-`), so a content-type
/// between the path and the description is recognizable.
fn looks_like_mime(token: &str) -> bool {
    match token.split_once('/') {
        Some((t, s)) if !t.is_empty() && !s.is_empty() => token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '+' | '-')),
        _ => false,
    }
}

/// One Attach: line back from its parts (quotes around a path with
/// spaces, then the optional type and description).
pub fn attach_line(a: &Attachment) -> String {
    let p = a.path.display().to_string();
    let mut line = if p.contains(' ') {
        format!("Attach: \"{p}\"")
    } else {
        format!("Attach: {p}")
    };
    if let Some(m) = &a.mime {
        line += &format!(" {m}");
    }
    // The @-options sit between the type and the description, where
    // a description is not expected to start with an @.
    if let Some(n) = a.name.as_deref().filter(|n| !n.is_empty()) {
        line += &format!(" @name=\"{}\"", n.replace('"', ""));
    }
    if a.inline {
        line += " @inline";
    }
    if a.unlink {
        line += " @unlink";
    }
    if let Some(d) = &a.description {
        line += &format!(" {d}");
    }
    line
}

/// Pull mutt-style `Attach: <path> [type/subtype] [description]`
/// pseudo-headers out of a draft's header block; quotes allow a path
/// with spaces, `~/` means $HOME. Returns the draft without those
/// lines.
pub fn extract_attachments(draft: &str) -> (String, Vec<Attachment>) {
    let (head, body) = match draft.split_once("\n\n") {
        Some((h, b)) => (h, Some(b)),
        None => (draft, None),
    };
    let mut attachments = Vec::new();
    let mut kept = Vec::new();
    for line in head.lines() {
        let value = match line.split_once(':') {
            Some((k, v)) if k.trim().eq_ignore_ascii_case("attach") => v.trim(),
            _ => {
                kept.push(line);
                continue;
            }
        };
        if value.is_empty() {
            continue;
        }
        let (path, desc) = match value.strip_prefix('"') {
            Some(rest) => rest.split_once('"').unwrap_or((rest, "")),
            None => value.split_once(char::is_whitespace).unwrap_or((value, "")),
        };
        let mut desc = desc.trim();
        let mut mime = None;
        match desc.split_once(char::is_whitespace) {
            Some((first, rest)) if looks_like_mime(first) => {
                mime = Some(first.to_string());
                desc = rest.trim();
            }
            None if looks_like_mime(desc) => {
                mime = Some(desc.to_string());
                desc = "";
            }
            _ => {}
        }
        // The @-options, in any order, ahead of the description.
        let mut a = Attachment::of(expand_home(path));
        a.mime = mime;
        while let Some(rest) = desc.strip_prefix('@') {
            if let Some(rest) = rest.strip_prefix("inline") {
                a.inline = true;
                desc = rest.trim_start();
            } else if let Some(rest) = rest.strip_prefix("unlink") {
                a.unlink = true;
                desc = rest.trim_start();
            } else if let Some(rest) = rest.strip_prefix("name=") {
                let (name, rest) = match rest.strip_prefix('"') {
                    Some(q) => q.split_once('"').unwrap_or((q, "")),
                    None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
                };
                a.name = (!name.is_empty()).then(|| name.to_string());
                desc = rest.trim_start();
            } else {
                break; // a description that happens to start with @
            }
        }
        a.description = (!desc.is_empty()).then(|| desc.to_string());
        attachments.push(a);
    }
    let mut out = kept.join("\n");
    if let Some(body) = body {
        out += "\n\n";
        out += body;
    }
    (out, attachments)
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(path)
}

/// Content type guessed from the filename extension.
pub fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("txt" | "log" | "md" | "patch" | "diff") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("csv") => "text/csv",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        Some("gz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

/// Base64 in MIME shape: 76-character lines, CRLF line endings.
fn b64_wrapped(bytes: &[u8]) -> String {
    let s = crate::smtp::b64(bytes);
    let mut out = String::with_capacity(s.len() + s.len() / 38 + 2);
    for chunk in s.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push_str("\r\n");
    }
    out
}

/// The text/plain entity of an outgoing message: its Content-Type
/// header and the body in canonical CRLF form. With mutt's
/// $text_flowed the part is declared `format=flowed` and the body is
/// space-stuffed, so a reader may rewrap it (RFC 3676). The
/// paragraphs are the editor's doing: rmut adds no trailing spaces.
pub fn text_entity(body: &str, flowed: bool) -> String {
    let mut out = String::from("Content-Type: text/plain; charset=utf-8");
    if flowed {
        out += "; format=flowed";
    }
    out += "\r\nContent-Transfer-Encoding: 8bit\r\n\r\n";
    let body = match flowed {
        true => crate::flowed::space_stuff(body),
        false => body.to_string(),
    };
    out += &String::from_utf8_lossy(&crate::pgp::crlf(body.as_bytes()));
    out
}

/// mutt's $text_flowed for a message that goes out with no MIME
/// wrapper at all: declare the body and space-stuff it, in place, on
/// a finalized draft. One that already carries a Content-Type (the
/// user wrote their own) is left alone.
pub fn flow_plain(text: &str) -> String {
    let (head, body) = match text.split_once("\n\n") {
        Some(pair) => pair,
        None => return text.to_string(),
    };
    if header_present(head, "Content-Type") {
        return text.to_string();
    }
    format!(
        "{}\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8; format=flowed\n\
         Content-Transfer-Encoding: 8bit\n\n{}",
        head.trim_end(),
        crate::flowed::space_stuff(body),
    )
}

/// The MIME entity (Content-Type header + body, CRLF endings) for a
/// draft body with attachments: multipart/mixed with the text first,
/// files base64-encoded, and optionally the forwarded original as
/// message/rfc822 (mutt's mime_forward). The caller puts it under the
/// draft's top-level headers, or inside a PGP layer.
pub fn mixed_entity(
    body: &str,
    files: &[Attachment],
    original: Option<&[u8]>,
    flowed: bool,
) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    parts.push(text_entity(body, flowed));
    for a in files {
        let bytes =
            std::fs::read(&a.path).with_context(|| format!("reading {}", a.path.display()))?;
        let mime = a.mime.as_deref().unwrap_or_else(|| content_type(&a.path));
        let disposition = if a.inline { "inline" } else { "attachment" };
        // An attached message (mutt's attach-message) goes in as it
        // is, a message/rfc822 part with no encoding and no filename.
        if mime.eq_ignore_ascii_case("message/rfc822") {
            let mut p =
                format!("Content-Type: message/rfc822\r\nContent-Disposition: {disposition}\r\n");
            if let Some(d) = &a.description {
                p += &format!("Content-Description: {d}\r\n");
            }
            p += "\r\n";
            p += &String::from_utf8_lossy(&crate::pgp::crlf(&bytes));
            parts.push(p);
            continue;
        }
        let mut p = format!(
            "Content-Type: {mime}\r\nContent-Disposition: {disposition}; filename=\"{}\"\r\n",
            a.send_name(),
        );
        if let Some(d) = &a.description {
            p += &format!("Content-Description: {d}\r\n");
        }
        p += "Content-Transfer-Encoding: base64\r\n\r\n";
        p += &b64_wrapped(&bytes);
        parts.push(p);
    }
    if let Some(orig) = original {
        let mut p =
            String::from("Content-Type: message/rfc822\r\nContent-Disposition: attachment\r\n\r\n");
        p += &String::from_utf8_lossy(&crate::pgp::crlf(orig));
        parts.push(p);
    }
    let boundary = {
        let mut n = 0usize;
        loop {
            let b = format!("=-rmut-mixed-{}-{n}", std::process::id());
            if !parts.iter().any(|p| p.contains(&b)) {
                break b;
            }
            n += 1;
        }
    };
    let mut out = format!("Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n");
    for p in &parts {
        out += &format!("--{boundary}\r\n");
        out += p;
        if !out.ends_with("\r\n") {
            out += "\r\n";
        }
    }
    out += &format!("--{boundary}--\r\n");
    Ok(out)
}

/// Message text for a bounce: the original with a fresh Resent-* block
/// prepended, per RFC 5322 (the newest resend goes first).
pub fn bounce_text(original: &[u8], from: &str, to: &str, date: &str, msg_id: &str) -> String {
    format!(
        "Resent-From: {from}\r\nResent-Date: {date}\r\nResent-Message-ID: {msg_id}\r\nResent-To: {to}\r\n{}",
        String::from_utf8_lossy(original),
    )
}

/// First address in an RFC 5322 address field, without display name,
/// e.g. the SMTP envelope sender from a From line.
/// The posting address from a List-Post header value:
/// `<mailto:dev@example.com>`, with the RFC 2369 `NO` meaning the list
/// takes no posts. Extra mailto parameters are dropped.
pub fn list_post_address(value: &str) -> Option<String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("NO") {
        return None;
    }
    let start = value.to_ascii_lowercase().find("mailto:")? + "mailto:".len();
    let rest = &value[start..];
    let addr = rest
        .split(['>', '?', ',', ' '])
        .next()
        .unwrap_or(rest)
        .trim();
    (!addr.is_empty()).then(|| addr.to_string())
}

/// mutt's $followup_to: the Mail-Followup-To for a message going to a
/// mailing list. Every recipient goes in; your own address is left out
/// when you are subscribed (the list copy is the one you will get) and
/// kept when you are not.
pub fn followup_to(to: &str, cc: &str, me: Me, subscribed: bool, my_from: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut push = |single: &mailparse::SingleInfo| {
        if subscribed && me.is_me(&single.addr) {
            return;
        }
        let written = match &single.display_name {
            Some(name) if !name.trim().is_empty() => format!("{name} <{}>", single.addr),
            _ => single.addr.clone(),
        };
        if !out.iter().any(|a| a == &written) {
            out.push(written);
        }
    };
    for field in [to, cc] {
        let Ok(list) = mailparse::addrparse(field) else {
            continue;
        };
        for addr in list.iter() {
            match addr {
                mailparse::MailAddr::Single(single) => push(single),
                mailparse::MailAddr::Group(group) => group.addrs.iter().for_each(&mut push),
            }
        }
    }
    if !subscribed
        && let Some(from) = bare_address(my_from)
        && !out
            .iter()
            .any(|a| bare_address(a).is_some_and(|b| b == from))
    {
        out.push(my_from.trim().to_string());
    }
    out.join(", ")
}

/// The Cc a group reply gets: everyone the original named in To and
/// Cc, written the way they were written, minus anyone already in
/// `to` (the sender, normally) and, unless mutt's $metoo is set, minus
/// me. Duplicates collapse on the bare address, so a person listed in
/// both To and Cc appears once.
pub fn group_recipients(orig_to: &str, orig_cc: &str, to: &str, me: Me, metoo: bool) -> String {
    let mut seen: Vec<String> = addresses(to).iter().map(|a| a.to_lowercase()).collect();
    let mut out: Vec<String> = Vec::new();
    let mut push = |single: &mailparse::SingleInfo| {
        let bare = single.addr.to_lowercase();
        if seen.contains(&bare) || (!metoo && me.is_me(&bare)) {
            return;
        }
        seen.push(bare);
        out.push(match &single.display_name {
            Some(name) if !name.trim().is_empty() => format!("{name} <{}>", single.addr),
            _ => single.addr.clone(),
        });
    };
    for field in [orig_to, orig_cc] {
        let Ok(list) = mailparse::addrparse(field) else {
            continue;
        };
        for addr in list.iter() {
            match addr {
                mailparse::MailAddr::Single(single) => push(single),
                mailparse::MailAddr::Group(group) => group.addrs.iter().for_each(&mut push),
            }
        }
    }
    out.join(", ")
}

/// A draft seen as a message, so hook patterns (`~t`, `~c`, `~s`,
/// `~f`, ...) can be matched against outgoing mail the way mutt
/// matches fcc-hook. `path` is where the body lives, so `~b` and `~h`
/// still have something to read; the date is now, since the draft has
/// no Date header yet. Bcc joins the Cc
/// addresses, so a hook on `~c` sees a blind recipient too.
pub fn draft_envelope(text: &str, path: &Path) -> crate::message::Envelope {
    let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
    let header = |name: &str| -> String {
        head.lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let bare = |field: &str| -> Vec<String> {
        addresses(field).iter().map(|a| a.to_lowercase()).collect()
    };
    let from_full = header("From");
    crate::message::Envelope {
        file: crate::maildir::MailFile {
            path: path.to_path_buf(),
            is_new: false,
            flags: crate::maildir::Flags {
                seen: true,
                ..Default::default()
            },
            size: text.len() as u64,
        },
        from: crate::message::short_from(&from_full),
        from_full,
        subject: header("Subject"),
        date: Local::now().timestamp(),
        msg_id: None,
        references: Vec::new(),
        tagged: false,
        to: bare(&header("To")),
        cc: [header("Cc"), header("Bcc")]
            .iter()
            .flat_map(|f| bare(f))
            .collect(),
        lines: Some(body.lines().count()),
        list: None,
        label: None,
        broken: false,
    }
}

/// mutt's `my_hdr`: extra header lines that go on every draft. An
/// entry naming a header the draft already carries replaces it, so
/// `my_hdr From:` and `my_hdr Reply-To:` win over what rmut chose;
/// To, Cc and Bcc instead gain the address, like mutt, so a standing
/// `my_hdr Bcc: me@example.com` cannot erase a reply's recipients.
/// Malformed entries (no colon, no name) are ignored.
pub fn apply_my_hdr(text: &str, my_hdr: &[String]) -> String {
    if my_hdr.is_empty() {
        return text.to_string();
    }
    let (head, body) = match text.split_once("\n\n") {
        Some((head, body)) => (head, body),
        None => (text.trim_end(), ""),
    };
    let mut lines: Vec<String> = head.lines().map(String::from).collect();
    for entry in my_hdr {
        let Some((name, value)) = entry.split_once(':') else {
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            continue;
        }
        let at = lines.iter().position(|l| {
            l.get(..name.len())
                .is_some_and(|k| k.eq_ignore_ascii_case(name))
                && l.as_bytes().get(name.len()) == Some(&b':')
        });
        let addressy = ["to", "cc", "bcc"].contains(&name.to_lowercase().as_str());
        match at {
            Some(i) if addressy => {
                let old = lines[i]
                    .split_once(':')
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default();
                lines[i] = if old.is_empty() {
                    format!("{name}: {value}")
                } else {
                    format!("{name}: {old}, {value}")
                };
            }
            Some(i) => lines[i] = format!("{name}: {value}"),
            None => lines.push(format!("{name}: {value}")),
        }
    }
    format!("{}\n\n{body}", lines.join("\n"))
}

pub fn bare_address(field: &str) -> Option<String> {
    match mailparse::addrparse(field)
        .ok()?
        .into_inner()
        .into_iter()
        .next()?
    {
        mailparse::MailAddr::Single(single) => Some(single.addr),
        mailparse::MailAddr::Group(group) => group.addrs.first().map(|a| a.addr.clone()),
    }
}

/// The SMTP envelope sender: the bare address of the From header.
pub fn from_address(text: &str) -> Option<String> {
    use mailparse::MailHeaderMap;
    let mail = mailparse::parse_mail(text.as_bytes()).ok()?;
    bare_address(&mail.get_headers().get_first_value("From")?)
}

fn field_addresses(value: &str, out: &mut Vec<String>) {
    let Ok(list) = mailparse::addrparse(value) else {
        return;
    };
    for addr in list.iter() {
        match addr {
            mailparse::MailAddr::Single(single) => out.push(single.addr.clone()),
            mailparse::MailAddr::Group(group) => {
                out.extend(group.addrs.iter().map(|a| a.addr.clone()));
            }
        }
    }
}

/// Every bare address in an RFC 5322 address field.
pub fn addresses(field: &str) -> Vec<String> {
    let mut out = Vec::new();
    field_addresses(field, &mut out);
    out
}

/// mutt's reverse_name: the first of my addresses (`alternates`
/// included) the original was addressed to, in the form it appeared:
/// the display name from the To/Cc header is kept.
pub fn reverse_from(orig_to: &str, orig_cc: &str, me: Me, realname: bool) -> Option<String> {
    let mine = |single: &mailparse::SingleInfo| -> Option<String> {
        if !me.is_me(&single.addr) {
            return None;
        }
        Some(match &single.display_name {
            // mutt's $reverse_realname off: the address is theirs,
            // the name stays whatever the identity says.
            Some(name) if realname && !name.trim().is_empty() => {
                format!("{name} <{}>", single.addr)
            }
            _ => single.addr.clone(),
        })
    };
    for field in [orig_to, orig_cc] {
        let Ok(list) = mailparse::addrparse(field) else {
            continue;
        };
        for addr in list.iter() {
            match addr {
                mailparse::MailAddr::Single(single) => {
                    if let Some(from) = mine(single) {
                        return Some(from);
                    }
                }
                mailparse::MailAddr::Group(group) => {
                    if let Some(from) = group.addrs.iter().find_map(&mine) {
                        return Some(from);
                    }
                }
            }
        }
    }
    None
}

/// SMTP envelope for a finalized draft: every To/Cc/Bcc address, and
/// the text with Bcc headers removed (they must not go on the wire).
pub fn smtp_envelope(text: &str) -> Result<(Vec<String>, String)> {
    let mail = mailparse::parse_mail(text.as_bytes())?;
    let mut rcpts = Vec::new();
    for header in &mail.headers {
        let key = header.get_key();
        if ["to", "cc", "bcc"].contains(&key.to_lowercase().as_str()) {
            field_addresses(&header.get_value(), &mut rcpts);
        }
    }
    rcpts.dedup();
    let (head, body) = text.split_once("\n\n").unwrap_or((text.trim_end(), ""));
    let mut out = String::new();
    let mut skipping = false;
    for line in head.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Folded continuation belongs to the previous header.
            if skipping {
                continue;
            }
        } else {
            skipping = line
                .split_once(':')
                .is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case("bcc"));
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(body);
    Ok((rcpts, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_can_come_in_quoted() {
        let plain = forward_body("Ann <ann@x>", 0, "hi", "one\ntwo\n", None);
        assert!(plain.contains("\none\ntwo\n"), "{plain}");
        let quoted = forward_body("Ann <ann@x>", 0, "hi", "one\ntwo\n", Some("> "));
        assert!(quoted.contains("\n> one\n> two\n"), "{quoted}");
        // The markers are rmut's words, so they stay flush.
        assert!(quoted.starts_with("----- Forwarded message from Ann <ann@x> -----"));
        assert!(quoted.ends_with("----- End forwarded message -----\n"));
    }

    #[test]
    fn the_signature_sits_under_a_dashes_line() {
        let body = with_signature("hello\n", "Ann\nx.example", true);
        assert_eq!(body, "hello\n\n-- \nAnn\nx.example\n");
        // nosig_dashes: the text alone, still a blank line down.
        assert_eq!(with_signature("hello\n", "Ann", false), "hello\n\nAnn\n");
        // An empty draft starts with the blank line all the same.
        assert_eq!(with_signature("", "Ann", true), "\n-- \nAnn\n");
    }

    #[test]
    fn a_signature_comes_from_a_file_or_a_command() {
        let dir = std::env::temp_dir().join(format!("rmut-sig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("signature");
        std::fs::write(&file, "Ann\n\n\n").unwrap();
        // Trailing blank lines are the file's, not the signature's.
        assert_eq!(
            signature_text(file.to_str().unwrap()).as_deref(),
            Some("Ann")
        );
        assert_eq!(signature_text("echo hello|").as_deref(), Some("hello"));
        // Nothing readable is no signature, not an empty one.
        assert_eq!(signature_text(dir.join("gone").to_str().unwrap()), None);
        assert_eq!(signature_text("  "), None);
        assert_eq!(signature_text("true|"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_post_addresses() {
        assert_eq!(
            list_post_address("<mailto:dev@example.com>").as_deref(),
            Some("dev@example.com")
        );
        assert_eq!(
            list_post_address("<mailto:dev@example.com?subject=help>").as_deref(),
            Some("dev@example.com")
        );
        assert_eq!(
            list_post_address("NOTE: <mailto:dev@example.com>, <http://x/post>").as_deref(),
            Some("dev@example.com")
        );
        // RFC 2369: a list that takes no posts, and a non-mail method.
        assert_eq!(list_post_address("NO"), None);
        assert_eq!(list_post_address("<http://example.com/post>"), None);
    }

    #[test]
    fn followup_to_drops_me_only_when_subscribed() {
        let addrs = vec!["alex@example.com".to_string()];
        let me = Me::addresses(&addrs);
        let subscribed = followup_to(
            "dev@example.com, Alex <alex@example.com>",
            "",
            me,
            true,
            "Alex <alex@example.com>",
        );
        assert_eq!(subscribed, "dev@example.com");
        let unsubscribed = followup_to(
            "dev@example.com",
            "petr@example.com",
            me,
            false,
            "Alex <alex@example.com>",
        );
        assert_eq!(
            unsubscribed,
            "dev@example.com, petr@example.com, Alex <alex@example.com>"
        );
        // Already listed: no second copy of my address.
        let once = followup_to(
            "dev@example.com, alex@example.com",
            "",
            me,
            false,
            "Alex <alex@example.com>",
        );
        assert_eq!(once, "dev@example.com, alex@example.com");
    }

    #[test]
    fn group_reply_drops_me_and_the_sender() {
        let addrs = vec!["alex@example.com".to_string()];
        let alternates = vec![crate::pattern::Matcher::new("^jp@old\\.example\\.com$")];
        let me = Me::new(&addrs, &alternates);
        let cc = group_recipients(
            "Team <team@example.com>, Alex <alex@example.com>, jp@old.example.com",
            "boss@example.com, team@example.com",
            "Petr <petr@example.com>",
            me,
            false,
        );
        // My exact address and my alternate are gone, the sender in To
        // is gone, and team@ appears once despite being listed twice.
        assert_eq!(cc, "Team <team@example.com>, boss@example.com");
        // $metoo keeps me on the copy.
        let cc = group_recipients(
            "team@example.com, alex@example.com",
            "",
            "petr@example.com",
            me,
            true,
        );
        assert_eq!(cc, "team@example.com, alex@example.com");
    }

    #[test]
    fn text_flowed_declares_and_stuffs_the_body() {
        let entity = text_entity("plain\n>looks quoted\n", true);
        assert!(
            entity.starts_with("Content-Type: text/plain; charset=utf-8; format=flowed\r\n"),
            "{entity}"
        );
        assert!(
            entity.ends_with("plain\r\n >looks quoted\r\n"),
            "{entity:?}"
        );
        // Off, the part is what it always was.
        let plain = text_entity("plain\n>looks quoted\n", false);
        assert!(plain.starts_with("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(plain.ends_with("plain\r\n>looks quoted\r\n"), "{plain:?}");
    }

    #[test]
    fn flow_plain_declares_an_unwrapped_draft() {
        let draft = "From: a@x\nTo: b@x\nSubject: s\n\n>quoted line\n";
        let out = flow_plain(draft);
        assert!(out.contains("Content-Type: text/plain; charset=utf-8; format=flowed\n"));
        assert!(out.ends_with("\n\n >quoted line\n"), "{out:?}");
        // A draft that declares its own type is left alone.
        let typed = "From: a@x\nContent-Type: text/x-diff\n\nbody\n";
        assert_eq!(flow_plain(typed), typed);
    }

    #[test]
    fn draft_envelope_reads_the_header_block() {
        let draft = "From: Jane Doe <jane@example.com>\n\
                     To: Bob <BOB@work.example.com>, team@x\n\
                     Cc: boss@x\n\
                     Bcc: archive@x\n\
                     Subject: quarterly\n\n\
                     two\nlines\n";
        let env = draft_envelope(draft, std::path::Path::new("/tmp/draft"));
        assert_eq!(env.subject, "quarterly");
        assert_eq!(env.from_full, "Jane Doe <jane@example.com>");
        assert_eq!(env.to, ["bob@work.example.com", "team@x"]);
        // Bcc joins Cc, so a hook on ~c sees a blind recipient too.
        assert_eq!(env.cc, ["boss@x", "archive@x"]);
        assert_eq!(env.lines, Some(2));
        let hit = |p: &str| {
            crate::pattern::matches_in(
                &crate::pattern::parse(p).unwrap(),
                &env,
                crate::pattern::Scope::default(),
                None,
            )
        };
        assert!(hit("~t @work\\.example\\.com"));
        assert!(hit("~c archive@"));
        assert!(hit("~A"));
        assert!(!hit("~t nobody@"));
    }

    #[test]
    fn my_hdr_merges_into_the_draft_head() {
        let draft = "From: jane@example.com\nTo: bob@x\nSubject: s\n\nbody\n";
        let merged = apply_my_hdr(
            draft,
            &[
                "Organization: Acme".to_string(),
                "From: Jane <jane@work.example.com>".into(),
                "Bcc: jane@example.com".into(),
                "To: archive@x".into(),
                "bogus".into(),
            ],
        );
        assert_eq!(
            merged,
            "From: Jane <jane@work.example.com>\n\
             To: bob@x, archive@x\n\
             Subject: s\n\
             Organization: Acme\n\
             Bcc: jane@example.com\n\
             \nbody\n"
        );
        // Nothing configured: the draft is untouched.
        assert_eq!(apply_my_hdr(draft, &[]), draft);
    }

    /// A message sent at a known moment, for the format strings.
    fn quoted() -> Quoted<'static> {
        Quoted {
            from: "Jane Doe <jane@example.com>",
            subject: "Lunch",
            message_id: Some("<m1@example.com>"),
            // 2024-03-11 10:00:00 UTC.
            date: 1_710_151_200,
        }
    }

    #[test]
    fn subjects_do_not_stack_prefixes() {
        let re = default_reply_regexp();
        assert_eq!(reply_subject("Lunch", &re), "Re: Lunch");
        assert_eq!(reply_subject("RE: Lunch", &re), "Re: Lunch");
        assert_eq!(reply_subject("Re[2]: Lunch", &re), "Re: Lunch");
        // A locale's own prefixes, and mutt's smart case: an
        // uppercase letter in the regex makes it exact.
        let aw = reply_regexp("^(re|aw|sv):[ \t]*").unwrap();
        assert_eq!(reply_subject("AW: Lunch", &aw), "Re: Lunch");
        let exact = reply_regexp("^(Re):[ \t]*").unwrap();
        assert_eq!(reply_subject("RE: Lunch", &exact), "Re: RE: Lunch");
        assert_eq!(
            forward_subject(DEFAULT_FORWARD_FORMAT, &quoted()),
            "[jane@example.com: Lunch]"
        );
    }

    #[test]
    fn quote_prefixes_every_line_with_the_indent_string() {
        assert_eq!(
            quote("On X, Y wrote:", DEFAULT_INDENT, "a\nb"),
            "On X, Y wrote:\n> a\n> b\n"
        );
        assert_eq!(quote("head", "| ", "a"), "head\n| a\n");
    }

    #[test]
    fn an_attribution_says_who_and_when() {
        let m = quoted();
        // The default names the author and the date it was sent.
        let line = attribution(DEFAULT_ATTRIBUTION, &m);
        assert!(line.starts_with("On "), "{line}");
        assert!(line.ends_with(", Jane Doe wrote:"), "{line}");
        // Every specifier mutt's does, and an unknown one stays put.
        assert_eq!(render_quoted("%a", &m), "jane@example.com");
        assert_eq!(render_quoted("%n", &m), "Jane Doe");
        assert_eq!(render_quoted("%f", &m), "Jane Doe <jane@example.com>");
        assert_eq!(render_quoted("%s", &m), "Lunch");
        assert_eq!(render_quoted("%i", &m), "m1@example.com");
        assert_eq!(render_quoted("100%%", &m), "100%");
        assert_eq!(render_quoted("%q", &m), "%q");
        // %{...} is the date in the caller's own words.
        assert_eq!(render_quoted("%{%Y}", &m), "2024");
        assert_eq!(render_quoted("[%{%Y}] %s", &m), "[2024] Lunch");
    }

    #[test]
    fn a_name_falls_back_to_the_address() {
        let m = Quoted {
            from: "bare@example.com",
            subject: "x",
            message_id: None,
            date: 0,
        };
        assert_eq!(render_quoted("%n", &m), "bare@example.com");
        assert_eq!(render_quoted("%i", &m), "");
    }

    #[test]
    fn draft_text_skips_empty_optional_headers() {
        let text = draft_text(
            &DraftHeaders {
                from: None,
                to: "a@x".into(),
                cc: Some("".into()),
                subject: "s".into(),
                in_reply_to: None,
                references: None,
            },
            "hi",
        );
        assert_eq!(text, "To: a@x\nSubject: s\n\nhi\n");
        let text = draft_text(
            &DraftHeaders {
                from: Some("Jane Work <jane@work.example.com>".into()),
                to: "a@x".into(),
                cc: None,
                subject: "s".into(),
                in_reply_to: None,
                references: None,
            },
            "hi",
        );
        assert!(text.starts_with("From: Jane Work <jane@work.example.com>\nTo: a@x\n"));
    }

    #[test]
    fn reverse_from_finds_my_address_as_it_appeared() {
        let addrs = vec!["jane@example.com".to_string(), "old@example.com".into()];
        let me = Me::addresses(&addrs);
        // Display name kept, match case-insensitive.
        assert_eq!(
            reverse_from("Boss Me <Jane@example.com>, bob@y", "", me, true).as_deref(),
            Some("Boss Me <Jane@example.com>")
        );
        // mutt's $reverse_realname off: the address alone comes over.
        assert_eq!(
            reverse_from("Boss Me <Jane@example.com>, bob@y", "", me, false).as_deref(),
            Some("Jane@example.com")
        );
        // Bare address stays bare; Cc is searched after To.
        assert_eq!(
            reverse_from("bob@y", "old@example.com", me, true).as_deref(),
            Some("old@example.com")
        );
        assert_eq!(reverse_from("bob@y, eve@z", "", me, true), None);
        assert_eq!(reverse_from("", "", me, true), None);
    }

    #[test]
    fn finalize_adds_missing_headers_once() {
        let draft = "To: a@x\nSubject: s\n\nbody\n";
        let out = finalize(draft, "me@host", "<id@host>", "DATE").unwrap();
        assert!(out.contains("From: me@host"));
        assert!(out.contains("Message-ID: <id@host>"));
        assert!(out.contains("Date: DATE"));
        assert!(out.ends_with("\n\nbody\n"));
        // User-provided From wins.
        let draft2 = "To: a@x\nFrom: custom@x\n\nbody\n";
        let out2 = finalize(draft2, "me@host", "<i>", "D").unwrap();
        assert!(out2.contains("From: custom@x"));
        assert!(!out2.contains("me@host"));
    }

    #[test]
    fn user_agent_and_signature_placement() {
        let draft = "To: a@b\n\nhi";
        let plain = finalize_with(draft, "me@x", "<id@h>", "Mon", false).unwrap();
        assert!(!plain.contains("User-Agent"));
        let ua = finalize_with(draft, "me@x", "<id@h>", "Mon", true).unwrap();
        assert!(ua.contains(&user_agent_header()), "{ua}");
        // A draft that already carries one is left alone.
        let own = finalize_with(
            "To: a@b\nUser-Agent: mine\n\nhi",
            "me@x",
            "<id@h>",
            "Mon",
            true,
        )
        .unwrap();
        assert_eq!(own.matches("User-Agent").count(), 1, "{own}");
        // sig_on_top puts the signature above the body.
        let below = with_signature_at("the reply", "Jane", true, false);
        assert!(below.trim_end().ends_with("Jane"), "{below}");
        let above = with_signature_at("the reply", "Jane", true, true);
        assert!(above.starts_with("-- \nJane\n"), "{above}");
        assert!(above.trim_end().ends_with("the reply"), "{above}");
    }

    #[test]
    fn finalize_rejects_missing_recipients() {
        assert!(finalize("Subject: s\n\nbody", "f", "<i>", "d").is_err());
        assert!(finalize("To:   \nSubject: s\n\nbody", "f", "<i>", "d").is_err());
        assert!(finalize("Bcc: a@x\n\nbody", "f", "<i>", "d").is_ok());
    }

    #[test]
    fn bare_address_drops_display_name() {
        assert_eq!(
            bare_address("Jane <jane@x.org>").as_deref(),
            Some("jane@x.org")
        );
        assert_eq!(bare_address("jane@x.org").as_deref(), Some("jane@x.org"));
        assert_eq!(bare_address(""), None);
    }

    #[test]
    fn smtp_envelope_collects_rcpts_and_strips_bcc() {
        let text =
            "To: Alice <a@x>, b@y\nCc: c@z\nBcc: hidden@q,\n also-hidden@q\nSubject: s\n\nbody\n";
        let (rcpts, out) = smtp_envelope(text).unwrap();
        assert_eq!(
            rcpts,
            vec!["a@x", "b@y", "c@z", "hidden@q", "also-hidden@q"]
        );
        assert!(!out.to_lowercase().contains("bcc"));
        assert!(!out.contains("hidden@q"));
        assert!(out.contains("To: Alice <a@x>, b@y\n"));
        assert!(out.ends_with("\n\nbody\n"));
    }

    #[test]
    fn extract_attachments_takes_the_pseudo_headers_out() {
        let draft = "To: a@x\nAttach: /tmp/report.pdf the Q2 numbers\n\
                     attach: \"/tmp/two words.png\"\nAttach:\nSubject: s\n\nbody\n";
        let (out, files) = extract_attachments(draft);
        assert_eq!(out, "To: a@x\nSubject: s\n\nbody\n");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, PathBuf::from("/tmp/report.pdf"));
        assert_eq!(files[0].description.as_deref(), Some("the Q2 numbers"));
        assert_eq!(files[1].path, PathBuf::from("/tmp/two words.png"));
        assert_eq!(files[1].description, None);
    }

    #[test]
    fn attach_lines_carry_a_type_override() {
        let draft = "Attach: /tmp/x.bin application/x-custom raw dump\n\
                     Attach: /tmp/y.txt see notes\n\n";
        let (_, files) = extract_attachments(draft);
        assert_eq!(files[0].mime.as_deref(), Some("application/x-custom"));
        assert_eq!(files[0].description.as_deref(), Some("raw dump"));
        // "see notes" is not a type/subtype token.
        assert_eq!(files[1].mime, None);
        assert_eq!(files[1].description.as_deref(), Some("see notes"));
        // attach_line writes back what extract_attachments reads.
        let line = attach_line(&files[0]);
        assert_eq!(line, "Attach: /tmp/x.bin application/x-custom raw dump");
        let (_, roundtrip) = extract_attachments(&format!("{line}\n\n"));
        assert_eq!(roundtrip[0].mime.as_deref(), Some("application/x-custom"));
        // A quoted path with spaces survives too.
        let spaced = Attachment {
            path: PathBuf::from("/tmp/two words.png"),
            mime: Some("image/png".into()),
            description: None,
            name: None,
            inline: false,
            unlink: false,
        };
        let (_, files) = extract_attachments(&format!("{}\n\n", attach_line(&spaced)));
        assert_eq!(files[0].path, PathBuf::from("/tmp/two words.png"));
        assert_eq!(files[0].mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn extract_attachments_leaves_plain_drafts_alone() {
        let draft = "To: a@x\nSubject: s\n\nAttach: not a header, body text\n";
        let (out, files) = extract_attachments(draft);
        assert_eq!(out, draft);
        assert!(files.is_empty());
    }

    #[test]
    fn mixed_entity_encodes_files_and_original() {
        use mailparse::MailHeaderMap;
        let dir = std::env::temp_dir().join(format!("rmut-attach-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blob: Vec<u8> = (0..=255u8).collect();
        std::fs::write(dir.join("blob.bin"), &blob).unwrap();
        let files = [Attachment {
            path: dir.join("blob.bin"),
            mime: None,
            description: Some("raw bytes".into()),
            name: None,
            inline: false,
            unlink: false,
        }];
        let orig = b"From: jane@x\r\nSubject: hi\r\n\r\noriginal body\r\n";
        let entity = mixed_entity("see attached", &files, Some(orig), false).unwrap();
        let mail = mailparse::parse_mail(entity.as_bytes()).unwrap();
        assert_eq!(mail.ctype.mimetype, "multipart/mixed");
        assert_eq!(mail.subparts.len(), 3);
        assert_eq!(mail.subparts[0].get_body().unwrap().trim(), "see attached");
        let file = &mail.subparts[1];
        assert_eq!(file.ctype.mimetype, "application/octet-stream");
        assert_eq!(file.get_body_raw().unwrap(), blob);
        let disp = file.get_headers().get_first_value("Content-Disposition");
        assert!(disp.unwrap().contains("filename=\"blob.bin\""));
        assert_eq!(
            file.get_headers()
                .get_first_value("Content-Description")
                .as_deref(),
            Some("raw bytes")
        );
        assert_eq!(mail.subparts[2].ctype.mimetype, "message/rfc822");
        assert!(
            mail.subparts[2]
                .get_body()
                .unwrap()
                .contains("original body")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mixed_entity_reports_a_missing_file() {
        let files = [Attachment {
            path: PathBuf::from("/nonexistent/nope.pdf"),
            mime: None,
            description: None,
            name: None,
            inline: false,
            unlink: false,
        }];
        let err = mixed_entity("hi", &files, None, false).unwrap_err();
        assert!(err.to_string().contains("/nonexistent/nope.pdf"));
    }

    #[test]
    fn bounce_text_prepends_resent_headers() {
        let orig = b"From: jane@x\nSubject: hi\n\nbody\n";
        let out = bounce_text(orig, "Me <me@x>", "bob@y", "DATE", "<id@x>");
        assert!(out.starts_with("Resent-From: Me <me@x>\r\n"));
        assert!(out.contains("Resent-Date: DATE\r\n"));
        assert!(out.contains("Resent-To: bob@y\r\n"));
        assert!(out.ends_with("From: jane@x\nSubject: hi\n\nbody\n"));
    }

    #[test]
    fn smtp_envelope_without_bcc_is_unchanged() {
        let text = "To: a@x\nSubject: s\n\nbody\n";
        let (rcpts, out) = smtp_envelope(text).unwrap();
        assert_eq!(rcpts, vec!["a@x"]);
        assert_eq!(out, text);
    }

    #[test]
    fn attach_line_options_round_trip() {
        let mut a = Attachment::of(PathBuf::from("/tmp/q2 report.pdf"));
        a.mime = Some("application/pdf".into());
        a.name = Some("report.pdf".into());
        a.inline = true;
        a.unlink = true;
        a.description = Some("the Q2 numbers".into());
        let line = attach_line(&a);
        assert_eq!(
            line,
            "Attach: \"/tmp/q2 report.pdf\" application/pdf @name=\"report.pdf\" @inline @unlink the Q2 numbers"
        );
        let (_, back) = extract_attachments(&format!("To: x\n{line}\n\nbody"));
        assert_eq!(back.len(), 1);
        let b = &back[0];
        assert_eq!(b.path, a.path);
        assert_eq!(b.mime.as_deref(), Some("application/pdf"));
        assert_eq!(b.name.as_deref(), Some("report.pdf"));
        assert!(b.inline && b.unlink);
        assert_eq!(b.description.as_deref(), Some("the Q2 numbers"));
        // A line without options reads as before, and a description
        // starting with @ is a description.
        let (_, plain) = extract_attachments("Attach: /tmp/a.txt text/plain @home notes\n\n");
        assert!(!plain[0].inline && plain[0].name.is_none());
        assert_eq!(plain[0].description.as_deref(), Some("@home notes"));
    }

    #[test]
    fn mixed_entity_honours_name_disposition_and_rfc822() {
        let dir = std::env::temp_dir().join(format!("rmut-attach-opts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("data.bin");
        std::fs::write(&file, b"xyz").unwrap();
        let msg = dir.join("1.host:2,S");
        std::fs::write(&msg, "From: a@x\nSubject: inner\n\nhello\n").unwrap();
        let mut a = Attachment::of(file.clone());
        a.name = Some("renamed.bin".into());
        a.inline = true;
        let mut m = Attachment::of(msg.clone());
        m.mime = Some("message/rfc822".into());
        let entity = mixed_entity("see attached", &[a, m], None, false).unwrap();
        assert!(
            entity.contains("Content-Disposition: inline; filename=\"renamed.bin\""),
            "{entity}"
        );
        assert!(
            entity.contains(
                "Content-Type: message/rfc822\r\nContent-Disposition: attachment\r\n\r\nFrom: a@x"
            ),
            "{entity}"
        );
        assert!(
            !entity.contains("filename=\"1.host"),
            "a message has no filename"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
