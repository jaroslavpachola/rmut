//! Draft building and finalizing for outgoing mail.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use chrono::{Local, TimeZone};

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

pub fn reply_subject(orig: &str) -> String {
    let t = orig.trim();
    if t.to_lowercase().starts_with("re:") {
        t.to_string()
    } else {
        format!("Re: {t}")
    }
}

/// mutt's default $forward_format, "[%a: %s]" — the author's address
/// and the original subject.
pub fn forward_subject(from_addr: &str, orig: &str) -> String {
    format!("[{from_addr}: {}]", orig.trim())
}

fn format_date(epoch: i64) -> String {
    match Local.timestamp_opt(epoch, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format("%a, %d %b %Y %H:%M").to_string()
        }
        chrono::LocalResult::None => "an unknown date".into(),
    }
}

pub fn attribution(from: &str, date_epoch: i64) -> String {
    format!("On {}, {from} wrote:", format_date(date_epoch))
}

/// Quote a body mutt-style under an attribution line.
pub fn quote(attribution: &str, body: &str) -> String {
    let mut out = format!("{attribution}\n");
    for line in body.lines() {
        out += &format!("> {line}\n");
    }
    out
}

pub fn forward_body(from: &str, date_epoch: i64, subject: &str, body: &str) -> String {
    format!(
        "----- Forwarded message from {from} -----\nDate: {}\nSubject: {subject}\n\n{}\n----- End forwarded message -----\n",
        format_date(date_epoch),
        body.trim_end(),
    )
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
/// From/Date/Message-ID when the user didn't write them.
pub fn finalize(draft: &str, from: &str, msg_id: &str, date: &str) -> Result<String> {
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
    Ok(format!("{head}\n\n{body}"))
}

/// A file named in an `Attach:` pseudo-header of the draft.
pub struct Attachment {
    pub path: PathBuf,
    /// Content-type override (compose menu ctrl+t); guessed from the
    /// extension otherwise.
    pub mime: Option<String>,
    pub description: Option<String>,
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
        attachments.push(Attachment {
            path: expand_home(path),
            mime,
            description: (!desc.is_empty()).then(|| desc.to_string()),
        });
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

/// The MIME entity (Content-Type header + body, CRLF endings) for a
/// draft body with attachments: multipart/mixed with the text first,
/// files base64-encoded, and optionally the forwarded original as
/// message/rfc822 (mutt's mime_forward). The caller puts it under the
/// draft's top-level headers — or inside a PGP layer.
pub fn mixed_entity(body: &str, files: &[Attachment], original: Option<&[u8]>) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut text = String::from(
        "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
    );
    text += &String::from_utf8_lossy(&crate::pgp::crlf(body.as_bytes()));
    parts.push(text);
    for a in files {
        let bytes =
            std::fs::read(&a.path).with_context(|| format!("reading {}", a.path.display()))?;
        let name = a
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("attachment");
        let mut p = format!(
            "Content-Type: {}\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n",
            a.mime.as_deref().unwrap_or_else(|| content_type(&a.path)),
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

/// First address in an RFC 5322 address field, without display name —
/// e.g. the SMTP envelope sender from a From line.
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

/// mutt's reverse_name: the first of `me` (lowercase bare addresses)
/// the original message was addressed to, in the form it appeared —
/// the display name from the To/Cc header is kept.
pub fn reverse_from(orig_to: &str, orig_cc: &str, me: &[String]) -> Option<String> {
    let mine = |single: &mailparse::SingleInfo| -> Option<String> {
        if !me.contains(&single.addr.to_lowercase()) {
            return None;
        }
        Some(match &single.display_name {
            Some(name) if !name.trim().is_empty() => format!("{name} <{}>", single.addr),
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
    fn subjects_do_not_stack_prefixes() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("RE: Lunch"), "RE: Lunch");
        assert_eq!(
            forward_subject("jane@example.com", "Lunch"),
            "[jane@example.com: Lunch]"
        );
    }

    #[test]
    fn quote_prefixes_every_line() {
        assert_eq!(
            quote("On X, Y wrote:", "a\nb"),
            "On X, Y wrote:\n> a\n> b\n"
        );
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
        let me = vec!["jane@example.com".to_string(), "old@example.com".into()];
        // Display name kept, match case-insensitive.
        assert_eq!(
            reverse_from("Boss Me <Jane@example.com>, bob@y", "", &me).as_deref(),
            Some("Boss Me <Jane@example.com>")
        );
        // Bare address stays bare; Cc is searched after To.
        assert_eq!(
            reverse_from("bob@y", "old@example.com", &me).as_deref(),
            Some("old@example.com")
        );
        assert_eq!(reverse_from("bob@y, eve@z", "", &me), None);
        assert_eq!(reverse_from("", "", &me), None);
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
        }];
        let orig = b"From: jane@x\r\nSubject: hi\r\n\r\noriginal body\r\n";
        let entity = mixed_entity("see attached", &files, Some(orig)).unwrap();
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
        }];
        let err = mixed_entity("hi", &files, None).unwrap_err();
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
}
