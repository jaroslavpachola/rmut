use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use mailparse::{MailHeaderMap, ParsedMail, parse_mail};

use crate::maildir::MailFile;

/// Summary of one message for the index view.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub file: MailFile,
    /// Short display form of the sender (name when there is one).
    pub from: String,
    /// The whole decoded From header, so `~f` can match the address
    /// too, like mutt (empty in entries from a pre-1.24 header cache).
    pub from_full: String,
    pub subject: String,
    /// Unix epoch seconds from the Date header, 0 if missing/unparsable.
    pub date: i64,
    /// Normalized `<...>` Message-ID, if present.
    pub msg_id: Option<String>,
    /// References chain (oldest first), In-Reply-To appended if novel.
    pub references: Vec<String>,
    /// Runtime tag mark (mutt's `t`); never persisted.
    pub tagged: bool,
    /// Bare lowercase To / Cc addresses, for the "addressed to me"
    /// index mark.
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// Body line count for `%l`; None for header-only IMAP cache
    /// files, whose `%?l?…&…?` else branch shows until the body is
    /// fetched.
    pub lines: Option<usize>,
    /// Mailing-list name from List-Id, for `%L` ("To <name>").
    pub list: Option<String>,
}

pub fn envelope(file: MailFile) -> Result<Envelope> {
    let raw = fs::read(&file.path).with_context(|| format!("reading {}", file.path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", file.path.display()))?;
    let headers = mail.get_headers();
    let from_full = headers.get_first_value("From").unwrap_or_default();
    let from = if from_full.trim().is_empty() {
        "(unknown)".into()
    } else {
        short_from(&from_full)
    };
    let subject = headers
        .get_first_value("Subject")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "(no subject)".into());
    let date = headers
        .get_first_value("Date")
        .and_then(|d| mailparse::dateparse(&d).ok())
        .unwrap_or(0);
    let msg_id = headers
        .get_first_value("Message-ID")
        .and_then(|v| parse_msg_ids(&v).into_iter().next());
    let to = field_addresses(&headers.get_all_values("To").join(", "));
    let cc = field_addresses(&headers.get_all_values("Cc").join(", "));
    let mut references = headers
        .get_first_value("References")
        .map(|v| parse_msg_ids(&v))
        .unwrap_or_default();
    if let Some(irt) = headers.get_first_value("In-Reply-To")
        && let Some(id) = parse_msg_ids(&irt).into_iter().next()
        && references.last() != Some(&id)
    {
        references.push(id);
    }
    let lines = if headers.get_first_value("X-Rmut-Partial").is_some() {
        None // header-only IMAP cache file: the body is elsewhere
    } else {
        Some(body_lines(&raw))
    };
    let list = headers
        .get_first_value("List-Id")
        .and_then(|v| list_name(&v));
    Ok(Envelope {
        file,
        from,
        from_full,
        subject,
        date,
        msg_id,
        references,
        tagged: false,
        to,
        cc,
        lines,
        list,
    })
}

/// Lines in the body part (after the first blank line), counted on the
/// raw bytes — cheap enough to do for every envelope.
pub fn body_lines(raw: &[u8]) -> usize {
    let mut offset = None;
    let mut i = 0;
    while i < raw.len() {
        let Some(end) = raw[i..].iter().position(|&b| b == b'\n').map(|p| i + p) else {
            break;
        };
        let line = &raw[i..end];
        if line.is_empty() || line == b"\r" {
            offset = Some(end + 1);
            break;
        }
        i = end + 1;
    }
    let Some(offset) = offset else { return 0 };
    let body = &raw[offset..];
    body.iter().filter(|&&b| b == b'\n').count()
        + usize::from(!body.is_empty() && !body.ends_with(b"\n"))
}

/// A short list name from a List-Id value: the display name when
/// there is one ("Dev talk <dev.lists.example.com>"), otherwise the
/// id's first dot-separated label.
fn list_name(value: &str) -> Option<String> {
    if let Some((name, _)) = value.split_once('<') {
        let name = name.trim().trim_matches('"').trim();
        if !name.is_empty() {
            return Some(name.to_string());
        }
    }
    let inner = value.trim().trim_start_matches('<').trim_end_matches('>');
    let label = inner.split('.').next().unwrap_or(inner).trim();
    (!label.is_empty()).then(|| label.to_string())
}

/// Bare lowercase addresses in an address header value.
fn field_addresses(value: &str) -> Vec<String> {
    let Ok(list) = mailparse::addrparse(value) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for addr in list.iter() {
        match addr {
            mailparse::MailAddr::Single(single) => out.push(single.addr.to_lowercase()),
            mailparse::MailAddr::Group(group) => {
                out.extend(group.addrs.iter().map(|a| a.addr.to_lowercase()));
            }
        }
    }
    out
}

/// Extract all `<...>` message-id tokens from a header value.
pub fn parse_msg_ids(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else {
            break;
        };
        ids.push(rest[start..=start + len].to_string());
        rest = &rest[start + len + 1..];
    }
    ids
}

/// Display name if present, otherwise the bare address.
pub fn short_from(from: &str) -> String {
    if let Ok(list) = mailparse::addrparse(from) {
        for addr in list.iter() {
            match addr {
                mailparse::MailAddr::Single(info) => {
                    return match &info.display_name {
                        Some(name) if !name.trim().is_empty() => name.clone(),
                        _ => info.addr.clone(),
                    };
                }
                mailparse::MailAddr::Group(group) => {
                    if let Some(info) = group.addrs.first() {
                        return info
                            .display_name
                            .clone()
                            .unwrap_or_else(|| info.addr.clone());
                    }
                }
            }
        }
    }
    from.trim().to_string()
}

pub fn format_index_date(epoch: i64) -> String {
    format_index_date_with(epoch, None)
}

/// Index date column, with an optional strftime override (mutt's
/// date_format); "%b %e" when unset, like mutt's default index date.
pub fn format_index_date_with(epoch: i64, format: Option<&str>) -> String {
    match Local.timestamp_opt(epoch, 0) {
        chrono::LocalResult::Single(dt) | chrono::LocalResult::Ambiguous(dt, _) => {
            dt.format(format.unwrap_or("%b %e")).to_string()
        }
        chrono::LocalResult::None => "      ".into(),
    }
}

/// Full message ready for the pager: mutt-style brief header block, the
/// complete header list for the `h` toggle, and the decoded text body.
#[derive(Debug, Clone)]
pub struct MessageView {
    pub brief: Vec<(String, String)>,
    pub all: Vec<(String, String)>,
    pub body: String,
}

/// mutt's ignore/unignore/hdr_order: which headers the pager's brief
/// view shows, and in what order. Entries are lowercase name
/// prefixes; `*` matches everything; unignore wins over ignore.
#[derive(Debug, Clone)]
pub struct HeaderRules {
    pub ignore: Vec<String>,
    pub unignore: Vec<String>,
    pub order: Vec<String>,
}

impl Default for HeaderRules {
    /// The classic view: everything hidden except the usual five, in
    /// their usual order.
    fn default() -> HeaderRules {
        let five = || {
            ["date", "from", "to", "cc", "subject"]
                .map(String::from)
                .to_vec()
        };
        HeaderRules {
            ignore: vec!["*".into()],
            unignore: five(),
            order: five(),
        }
    }
}

fn prefix_match(prefixes: &[String], name: &str) -> bool {
    prefixes.iter().any(|p| p == "*" || name.starts_with(p))
}

/// The brief header view under `rules`: weeded (ignore minus
/// unignore), then sorted by hdr_order position — unlisted names
/// keep message order after the listed ones.
pub fn weed(all: &[(String, String)], rules: &HeaderRules) -> Vec<(String, String)> {
    let mut shown: Vec<(String, String)> = all
        .iter()
        .filter(|(name, _)| {
            let name = name.to_lowercase();
            !prefix_match(&rules.ignore, &name) || prefix_match(&rules.unignore, &name)
        })
        .cloned()
        .collect();
    shown.sort_by_key(|(name, _)| {
        let name = name.to_lowercase();
        rules
            .order
            .iter()
            .position(|p| name.starts_with(p))
            .unwrap_or(rules.order.len())
    });
    shown
}

pub fn load(path: &Path) -> Result<MessageView> {
    load_with(
        path,
        &std::collections::HashMap::new(),
        &HeaderRules::default(),
    )
}

/// Like `load`, but when the message has no text/plain part, a part
/// whose MIME type appears in `filters` is rendered through its shell
/// command (stdin → stdout), mutt's auto_view; `rules` weeds the
/// brief header block.
pub fn load_with(
    path: &Path,
    filters: &std::collections::HashMap<String, String>,
    rules: &HeaderRules,
) -> Result<MessageView> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let all: Vec<(String, String)> = mail
        .headers
        .iter()
        .map(|h| (h.get_key(), h.get_value()))
        .collect();
    let brief = weed(&all, rules);
    let body = find_plain(&mail)
        .or_else(|| filtered_body(&mail, filters))
        .or_else(|| extract_text(&mail))
        .unwrap_or_else(|| "[-- no displayable text part --]".into());
    Ok(MessageView { brief, all, body })
}

/// The first text/plain leaf, depth-first.
fn find_plain(mail: &ParsedMail) -> Option<String> {
    if mail.subparts.is_empty() {
        return (mail.ctype.mimetype == "text/plain")
            .then(|| mail.get_body().ok())
            .flatten();
    }
    mail.subparts.iter().find_map(find_plain)
}

/// First leaf with a configured filter, rendered through it.
fn filtered_body(
    mail: &ParsedMail,
    filters: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let mut all = Vec::new();
    leaves(mail, &mut all);
    for part in all {
        if let Some(command) = filters.get(&part.ctype.mimetype)
            && let Ok(raw) = part.get_body_raw()
        {
            return match run_filter(command, &raw) {
                Ok(text) => Some(format!(
                    "[-- {} rendered by {command} --]\n\n{text}",
                    part.ctype.mimetype
                )),
                Err(err) => Some(format!("[-- filter {command} failed: {err:#} --]")),
            };
        }
    }
    None
}

/// Decoded part rendered through a filter command (attachment viewer).
pub fn filter_part(path: &Path, index: usize, command: &str) -> Result<String> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw)?;
    let bytes = leaf_at(&mail, index)?.get_body_raw()?;
    run_filter(command, &bytes)
}

/// sh -c `command` with the part on stdin, capturing stdout.
fn run_filter(command: &str, input: &[u8]) -> Result<String> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("running {command}"))?;
    let mut stdin = child.stdin.take().context("no stdin on filter child")?;
    let input = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    anyhow::ensure!(out.status.success(), "{command} exited with {}", out.status);
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Decoded value of the first `name` header, read from disk (used by
/// the `~e` Sender pattern).
pub fn first_header(path: &Path, name: &str) -> Option<String> {
    let raw = fs::read(path).ok()?;
    let mail = parse_mail(&raw).ok()?;
    mail.get_headers().get_first_value(name)
}

/// Decoded text body only (used by `~b` pattern matching).
pub fn body_text(path: &Path) -> Result<String> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(extract_text(&mail).unwrap_or_default())
}

/// Depth-first search for the first text/plain part (falling back to any
/// text/* part), with transfer encoding and charset decoded by mailparse.
pub(crate) fn extract_text(mail: &ParsedMail) -> Option<String> {
    if mail.subparts.is_empty() {
        if mail.ctype.mimetype.starts_with("text/") {
            return mail.get_body().ok();
        }
        return None;
    }
    for sub in &mail.subparts {
        if sub.ctype.mimetype == "text/plain"
            && sub.subparts.is_empty()
            && let Ok(body) = sub.get_body()
        {
            return Some(body);
        }
    }
    for sub in &mail.subparts {
        if let Some(body) = extract_text(sub) {
            return Some(body);
        }
    }
    None
}

/// One leaf MIME part, for the attachment menu.
#[derive(Debug, Clone)]
pub struct Part {
    pub mimetype: String,
    pub filename: Option<String>,
    /// Decoded size in bytes.
    pub size: usize,
    pub is_text: bool,
}

fn leaves<'a, 'b>(mail: &'a ParsedMail<'b>, out: &mut Vec<&'a ParsedMail<'b>>) {
    if mail.subparts.is_empty() {
        out.push(mail);
    } else {
        for sub in &mail.subparts {
            leaves(sub, out);
        }
    }
}

pub fn parts(path: &Path) -> Result<Vec<Part>> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let mut all = Vec::new();
    leaves(&mail, &mut all);
    Ok(all
        .iter()
        .map(|p| {
            let filename = p
                .get_content_disposition()
                .params
                .get("filename")
                .cloned()
                .or_else(|| p.ctype.params.get("name").cloned());
            Part {
                mimetype: p.ctype.mimetype.clone(),
                filename,
                size: p.get_body_raw().map(|b| b.len()).unwrap_or(0),
                is_text: p.ctype.mimetype.starts_with("text/"),
            }
        })
        .collect())
}

fn leaf_at<'a, 'b>(mail: &'a ParsedMail<'b>, index: usize) -> Result<&'a ParsedMail<'b>> {
    let mut all = Vec::new();
    leaves(mail, &mut all);
    all.get(index).copied().context("no such part")
}

/// Decoded text of the given leaf part.
pub fn part_text(path: &Path, index: usize) -> Result<String> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw)?;
    Ok(leaf_at(&mail, index)?.get_body()?)
}

/// Decoded bytes of the given leaf part (for saving to a file).
pub fn part_bytes(path: &Path, index: usize) -> Result<Vec<u8>> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw)?;
    Ok(leaf_at(&mail, index)?.get_body_raw()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weed_applies_ignore_unignore_and_order() {
        let all: Vec<(String, String)> = [
            ("Received", "relay"),
            ("Subject", "hi"),
            ("X-Topic", "budget"),
            ("From", "jane@example.com"),
            ("X-Spam-Score", "0"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .to_vec();
        let names = |v: &[(String, String)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        // The classic default: only the usual five, in their order.
        assert_eq!(
            names(&weed(&all, &HeaderRules::default())),
            ["From", "Subject"]
        );
        // Prefix ignore with an unignore exception; no order keeps
        // message order.
        let rules = HeaderRules {
            ignore: vec!["x-".into(), "received".into()],
            unignore: vec!["x-topic".into()],
            order: vec![],
        };
        assert_eq!(names(&weed(&all, &rules)), ["Subject", "X-Topic", "From"]);
        // hdr_order sorts the listed prefixes first, the rest after.
        let rules = HeaderRules {
            ignore: vec!["*".into()],
            unignore: vec!["subject".into(), "x-topic".into(), "from".into()],
            order: vec!["x-topic".into(), "from".into()],
        };
        assert_eq!(names(&weed(&all, &rules)), ["X-Topic", "From", "Subject"]);
    }

    const MULTIPART: &str = concat!(
        "From: a@example.com\r\n",
        "Subject: multi\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "plain text\r\n",
        "--b\r\n",
        "Content-Type: application/pdf; name=\"report.pdf\"\r\n",
        "Content-Disposition: attachment; filename=\"report.pdf\"\r\n",
        "Content-Transfer-Encoding: base64\r\n",
        "\r\n",
        "JVBERg==\r\n",
        "--b--\r\n",
    );

    #[test]
    fn short_from_prefers_display_name() {
        assert_eq!(short_from("Jane Doe <jane@example.com>"), "Jane Doe");
        assert_eq!(short_from("jane@example.com"), "jane@example.com");
        assert_eq!(short_from(""), "");
    }

    #[test]
    fn extract_text_picks_plain_from_multipart() {
        let mail = parse_mail(MULTIPART.as_bytes()).unwrap();
        assert_eq!(extract_text(&mail).unwrap().trim(), "plain text");
    }

    #[test]
    fn parts_lists_leaves_with_filenames() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(&path, MULTIPART).unwrap();
        let parts = parts(&path).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].is_text && parts[0].filename.is_none());
        assert_eq!(parts[1].mimetype, "application/pdf");
        assert_eq!(parts[1].filename.as_deref(), Some("report.pdf"));
        // base64 "JVBERg==" decodes to %PDF.
        assert_eq!(part_bytes(&path, 1).unwrap(), b"%PDF");
        assert_eq!(part_text(&path, 0).unwrap().trim(), "plain text");
    }

    #[test]
    fn parse_msg_ids_handles_lists_and_garbage() {
        assert_eq!(parse_msg_ids("<a@x> <b@y>"), vec!["<a@x>", "<b@y>"]);
        assert_eq!(parse_msg_ids("junk <a@x> junk"), vec!["<a@x>"]);
        assert!(parse_msg_ids("no ids here <broken").is_empty());
    }

    #[test]
    fn envelope_counts_lines_and_finds_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str, content: &str| {
            let path = tmp.path().join(name);
            std::fs::write(&path, content).unwrap();
            envelope(crate::maildir::MailFile {
                path,
                is_new: false,
                flags: Default::default(),
                size: 0,
            })
            .unwrap()
        };
        let env = file(
            "listed",
            "From: a@x\r\nList-Id: Dev talk <dev.lists.example.com>\r\nSubject: s\r\n\r\none\r\ntwo\r\nthree",
        );
        assert_eq!(env.lines, Some(3)); // last line unterminated
        assert_eq!(env.list.as_deref(), Some("Dev talk"));
        let env = file(
            "bare-list",
            "From: a@x\r\nList-Id: <announce.example.com>\r\nSubject: s\r\n\r\nhi\r\n",
        );
        assert_eq!(env.list.as_deref(), Some("announce"));
        assert_eq!(env.lines, Some(1));
        let env = file("plain", "From: a@x\r\nSubject: s\r\n\r\n");
        assert!(env.list.is_none());
        assert_eq!(env.lines, Some(0));
        // Header-only IMAP cache file: the count is unknown.
        let env = file(
            "partial",
            "X-Rmut-Partial: 1\r\nFrom: a@x\r\nSubject: s\r\n\r\n",
        );
        assert_eq!(env.lines, None);
    }

    #[test]
    fn envelope_decodes_rfc2047_subject() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(
            &path,
            "From: Jane <j@example.com>\r\nSubject: =?utf-8?q?p=C5=99=C3=ADli=C5=A1?=\r\nDate: Mon, 6 Jul 2026 10:00:00 +0200\r\nMessage-ID: <one@x>\r\nReferences: <root@x>\r\nIn-Reply-To: <parent@x>\r\n\r\nhi\r\n",
        )
        .unwrap();
        let env = envelope(crate::maildir::MailFile {
            path,
            is_new: true,
            flags: Default::default(),
            size: 0,
        })
        .unwrap();
        assert_eq!(env.subject, "příliš");
        assert_eq!(env.from, "Jane");
        assert!(env.date > 0);
        assert_eq!(env.msg_id.as_deref(), Some("<one@x>"));
        assert_eq!(env.references, vec!["<root@x>", "<parent@x>"]);
    }

    #[test]
    fn load_collects_brief_and_all_headers() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(
            &path,
            "From: a@x\r\nTo: b@y\r\nSubject: s\r\nX-Custom: z\r\n\r\nbody\r\n",
        )
        .unwrap();
        let view = load(&path).unwrap();
        assert_eq!(view.brief.len(), 3); // From, To, Subject (no Date/Cc)
        assert_eq!(view.all.len(), 4);
        assert!(view.all.iter().any(|(k, _)| k == "X-Custom"));
    }
}
