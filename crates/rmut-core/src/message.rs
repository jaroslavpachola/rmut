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
    /// mutt's X-Label header, for `%y`, `~y`, and sort by label.
    pub label: Option<String>,
    /// The user broke this message out of its thread: rmut's own
    /// `X-Rmut-Thread: broken`, written by break-thread. mutt has
    /// nothing like it and its subject grouping hangs a broken
    /// message straight back where it was; rmut's break-thread
    /// sticks instead, and this is the header that makes it.
    pub broken: bool,
}

/// The header break-thread leaves behind, so that the subject
/// grouping knows to leave the message alone.
pub const BROKEN_HEADER: &str = "X-Rmut-Thread";
const BROKEN_VALUE: &str = "broken";

/// Header text on its way to a one-line slot in the display. A tab
/// or a stray control character would be written to the terminal as
/// it stands: a tab jumps to the next tab stop, pushing the rest of
/// an index row past the window edge and wrapping it onto a second
/// line. Mail carries them often enough (a header the sender folded
/// by hand, an RFC 2047 word that decoded to one), so every such
/// field passes through here.
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| match c.is_control() {
            true => ' ',
            false => c,
        })
        .collect()
}

/// mutt's list-action menu, in its order: the RFC 2369 List-* headers
/// a message may carry, each with the URL mutt would act on. mutt
/// takes the first `<mailto:...>` in the header and nothing else;
/// here a header holding only other schemes keeps its first URL, so
/// the answer can be "only mailto: is supported" rather than "none".
pub const LIST_ACTIONS: [(&str, &str); 6] = [
    ("Help", "List-Help"),
    ("Post", "List-Post"),
    ("Subscribe", "List-Subscribe"),
    ("Unsubscribe", "List-Unsubscribe"),
    ("Archives", "List-Archive"),
    ("Owner", "List-Owner"),
];

/// The list actions a raw message offers: one entry per action in
/// [`LIST_ACTIONS`] order, None where the header is absent.
pub fn list_actions(raw: &[u8]) -> Vec<(&'static str, Option<String>)> {
    let headers = parse_mail(raw).map(|m| m.headers).unwrap_or_default();
    LIST_ACTIONS
        .iter()
        .map(|&(name, header)| {
            let url = headers
                .get_first_value(header)
                .and_then(|value| list_url(&value));
            (name, url)
        })
        .collect()
}

/// mutt's mutt_parse_list_header: the first `<mailto:...>` among the
/// angle-bracketed URLs of a List-* value, else the first URL of any
/// scheme (mutt would have nothing; this lets the error name it).
fn list_url(value: &str) -> Option<String> {
    let urls: Vec<&str> = value
        .split('<')
        .skip(1)
        .filter_map(|rest| rest.split_once('>').map(|(url, _)| url.trim()))
        .filter(|url| !url.is_empty())
        .collect();
    urls.iter()
        .find(|url| url.to_ascii_lowercase().starts_with("mailto:"))
        .or(urls.first())
        .map(|url| url.to_string())
}

pub fn envelope(file: MailFile) -> Result<Envelope> {
    let raw = fs::read(&file.path).with_context(|| format!("reading {}", file.path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", file.path.display()))?;
    let headers = mail.get_headers();
    let from_full = one_line(&headers.get_first_value("From").unwrap_or_default());
    let from = if from_full.trim().is_empty() {
        "(unknown)".into()
    } else {
        short_from(&from_full)
    };
    let subject = headers
        .get_first_value("Subject")
        .map(|s| one_line(&s))
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
        .and_then(|v| list_name(&v))
        .map(|n| one_line(&n));
    let label = headers
        .get_first_value("X-Label")
        .map(|v| one_line(&v))
        .filter(|v| !v.trim().is_empty());
    let broken = headers
        .get_first_value(BROKEN_HEADER)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(BROKEN_VALUE));
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
        label,
        broken,
    })
}

/// Lines in the body part (after the first blank line), counted on the
/// raw bytes, cheap enough to do for every envelope.
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
/// unignore), then sorted by hdr_order position; unlisted names
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

/// Everything the pager needs to turn a message into text: mutt's
/// auto_view filters, the header weeding rules, $reflow_text and
/// $alternative_order.
#[derive(Debug, Clone)]
pub struct Display {
    /// MIME type → shell command rendering the part (stdin → stdout),
    /// mutt's auto_view. Types are lowercase.
    pub filters: std::collections::HashMap<String, String>,
    pub rules: HeaderRules,
    /// mutt's $reflow_text: put a `format=flowed` part back into
    /// paragraphs rather than keeping the sender's line breaks.
    pub reflow: bool,
    /// mutt's $alternative_order: MIME types (`text/*` allowed), most
    /// wanted first, consulted before anything else in a
    /// multipart/alternative.
    pub alternative_order: Vec<String>,
}

impl Default for Display {
    fn default() -> Display {
        Display {
            filters: std::collections::HashMap::new(),
            rules: HeaderRules::default(),
            reflow: true,
            alternative_order: Vec::new(),
        }
    }
}

pub fn load(path: &Path) -> Result<MessageView> {
    load_with(path, &Display::default())
}

/// Like `load`, but under an explicit `Display`.
pub fn load_with(path: &Path, disp: &Display) -> Result<MessageView> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw).with_context(|| format!("parsing {}", path.display()))?;
    let all: Vec<(String, String)> = mail
        .headers
        .iter()
        .map(|h| (h.get_key(), one_line(&h.get_value())))
        .collect();
    let brief = weed(&all, &disp.rules);
    let mut body = String::new();
    if !render(&mail, disp, &mut body) && body.is_empty() {
        body = "[-- no displayable text part --]".into();
    }
    Ok(MessageView { brief, all, body })
}

/// Render a MIME entity that is not a file of its own — the plaintext
/// gpg hands back for a PGP/MIME message — exactly as the pager
/// renders a message body: the whole tree, attachments announced,
/// filters applied. Text that is not MIME at all comes back as it is.
pub fn render_entity(raw: &[u8], disp: &Display) -> String {
    let Ok(mail) = parse_mail(raw) else {
        return String::from_utf8_lossy(raw).into_owned();
    };
    let mut out = String::new();
    // Nothing displayable means it was not really a MIME entity (a
    // sender who armored plain text without headers): show the text.
    if !render(&mail, disp, &mut out) || out.trim().is_empty() {
        return String::from_utf8_lossy(raw).into_owned();
    }
    out
}

/// Mutt's pager rendering: the whole MIME tree, depth-first. Text
/// parts (any subtype, like mutt, including raw html) and filtered
/// types show inline, multipart/alternative collapses to its best
/// subpart, message/rfc822 shows its weeded headers then its own
/// tree, and every subpart of a multipart is announced with mutt's
/// `[-- Attachment #N --]` / `[-- Type: ... --]` marker block. Parts
/// that cannot display leave a one-line stub. Returns whether
/// anything actually displayed (as opposed to only stubs), so the
/// caller can tell an empty body from an undisplayable message.
fn render(part: &ParsedMail, disp: &Display, out: &mut String) -> bool {
    let ty = part.ctype.mimetype.clone();
    if ty == "multipart/alternative" {
        return match pick_alternative(&part.subparts, disp) {
            Some(best) => render(best, disp, out),
            None => {
                gap(out);
                out.push_str("[-- multipart/alternative: no displayable part --]\n");
                false
            }
        };
    }
    if ty.starts_with("multipart/") {
        let mut shown = false;
        for (i, sub) in part.subparts.iter().enumerate() {
            marker(sub, i + 1, out);
            shown |= render(sub, disp, out);
        }
        return shown;
    }
    if let Some(command) = disp.filters.get(&ty) {
        gap(out);
        let text = part
            .get_body_raw()
            .map_err(anyhow::Error::from)
            .and_then(|raw| run_filter(command, &raw));
        return match text {
            Ok(text) => {
                out.push_str(&format!("[-- Autoview using {command} --]\n\n{text}"));
                ensure_newline(out);
                true
            }
            Err(err) => {
                out.push_str(&format!("[-- filter {command} failed: {err:#} --]\n"));
                false
            }
        };
    }
    if ty == "message/rfc822" {
        if let Ok(raw) = part.get_body_raw()
            && let Ok(embedded) = parse_mail(&raw)
        {
            gap(out);
            let all: Vec<(String, String)> = embedded
                .headers
                .iter()
                .map(|h| (h.get_key(), h.get_value()))
                .collect();
            for (name, value) in weed(&all, &disp.rules) {
                out.push_str(&format!("{name}: {value}\n"));
            }
            out.push('\n');
            return render(&embedded, disp, out);
        }
        gap(out);
        out.push_str("[-- message/rfc822: cannot parse --]\n");
        return false;
    }
    if ty.starts_with("text/")
        && let Ok(text) = part.get_body()
    {
        gap(out);
        // RFC 3676: a flowed part goes back to one line per paragraph
        // so the pager wraps it at the display width, rather than
        // keeping whatever width the sender happened to use.
        match flowed_delsp(part).filter(|_| disp.reflow) {
            Some(delsp) => out.push_str(&crate::flowed::unflow(&text, delsp)),
            None => out.push_str(&text),
        }
        ensure_newline(out);
        return true;
    }
    gap(out);
    out.push_str(&format!(
        "[-- {ty} is unsupported (use 'v' to view this part) --]\n"
    ));
    false
}

/// `Some(delsp)` when the part is `text/plain; format=flowed`, with
/// the DelSp parameter (default no) alongside.
fn flowed_delsp(part: &ParsedMail) -> Option<bool> {
    let value = |name: &str| {
        part.ctype
            .params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_lowercase())
    };
    if part.ctype.mimetype != "text/plain" || value("format").as_deref() != Some("flowed") {
        return None;
    }
    Some(value("delsp").as_deref() == Some("yes"))
}

/// Mutt's alternative_handler order: $alternative_order first, most
/// wanted type first; then a part with an auto_view filter; then the
/// richest text part (html < plain < enriched, later parts win ties,
/// like mutt); then anything displayable at all.
fn pick_alternative<'a, 'b>(
    subs: &'a [ParsedMail<'b>],
    disp: &Display,
) -> Option<&'a ParsedMail<'b>> {
    for want in &disp.alternative_order {
        if let Some(p) = subs
            .iter()
            .find(|p| type_matches(want, &p.ctype.mimetype) && displayable(p, &disp.filters))
        {
            return Some(p);
        }
    }
    if let Some(p) = subs
        .iter()
        .rev()
        .find(|p| disp.filters.contains_key(&p.ctype.mimetype))
    {
        return Some(p);
    }
    let rank = |p: &ParsedMail| match p.ctype.mimetype.as_str() {
        "text/enriched" => 3,
        "text/plain" => 2,
        "text/html" => 1,
        _ => 0,
    };
    if let Some((_, p)) = subs
        .iter()
        .enumerate()
        .filter(|(_, p)| rank(p) > 0)
        .max_by_key(|&(i, p)| (rank(p), i))
    {
        return Some(p);
    }
    subs.iter().find(|p| displayable(p, &disp.filters))
}

/// An $alternative_order entry against a part's type: an exact match,
/// or mutt's `type/*` wildcard (a bare `type` means the same).
fn type_matches(want: &str, mimetype: &str) -> bool {
    let want = want.trim().to_lowercase();
    if want.is_empty() {
        return false;
    }
    match want.strip_suffix("/*").unwrap_or(&want) {
        main if main == want && want.contains('/') => want == mimetype,
        main => mimetype.split('/').next() == Some(main),
    }
}

/// Can this part (or anything inside it) show in the pager?
fn displayable(part: &ParsedMail, filters: &std::collections::HashMap<String, String>) -> bool {
    let ty = &part.ctype.mimetype;
    ty.starts_with("text/")
        || ty == "message/rfc822"
        || filters.contains_key(ty)
        || (ty.starts_with("multipart/") && part.subparts.iter().any(|s| displayable(s, filters)))
}

/// Mutt's attachment announcement in the pager:
/// `[-- Attachment #2: report.pdf --]`
/// `[-- Type: application/pdf, Encoding: base64, Size: 12K --]`
fn marker(part: &ParsedMail, count: usize, out: &mut String) {
    gap(out);
    let name = part
        .get_headers()
        .get_first_value("Content-Description")
        .or_else(|| part_filename(part));
    match name {
        Some(n) => out.push_str(&format!("[-- Attachment #{count}: {n} --]\n")),
        None => out.push_str(&format!("[-- Attachment #{count} --]\n")),
    }
    let encoding = part
        .get_headers()
        .get_first_value("Content-Transfer-Encoding")
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "7bit".into());
    let size = part.get_body_raw().map(|b| b.len()).unwrap_or(0);
    out.push_str(&format!(
        "[-- Type: {}, Encoding: {encoding}, Size: {} --]\n",
        part.ctype.mimetype,
        pretty_size(size)
    ));
}

/// One blank separator line between rendered pieces, none at the top.
fn gap(out: &mut String) {
    if out.is_empty() {
        return;
    }
    while !out.ends_with("\n\n") {
        out.push('\n');
    }
}

fn ensure_newline(out: &mut String) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Mutt's mutt_pretty_size: 0K, 3.2K, 128K, 1.3M, 12M.
fn pretty_size(n: usize) -> String {
    if n == 0 {
        "0K".into()
    } else if n < 10189 {
        format!("{:.1}K", n as f64 / 1024.0)
    } else if n < 1023949 {
        format!("{}K", (n + 51) / 1024)
    } else if n < 10433332 {
        format!("{:.1}M", n as f64 / 1048576.0)
    } else {
        format!("{}M", n / 1048576)
    }
}

/// Decoded part rendered through a filter command (attachment viewer).
pub fn filter_part(path: &Path, index: usize, command: &str) -> Result<String> {
    let raw = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mail = parse_mail(&raw)?;
    let bytes = leaf_at(&mail, index)?.get_body_raw()?;
    run_filter(command, &bytes)
}

/// sh -c `command` with the part on stdin, capturing stdout. A
/// mailcap command with `%s` wants the part in a file instead
/// (RFC 1524), so it gets a temporary one, removed afterwards.
fn run_filter(command: &str, input: &[u8]) -> Result<String> {
    match command.contains("%s") {
        true => {
            let file = TempPart::new(input)?;
            let quoted = format!(
                "'{}'",
                file.path.display().to_string().replace('\'', r"'\''")
            );
            run_piped(&command.replace("%s", &quoted), b"")
        }
        false => run_piped(command, input),
    }
}

/// A part written out for a `%s` filter, deleted when it drops.
struct TempPart {
    path: std::path::PathBuf,
}

impl TempPart {
    fn new(input: &[u8]) -> Result<TempPart> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("rmut-part-{}-{n}", std::process::id()));
        fs::write(&path, input).with_context(|| format!("writing {}", path.display()))?;
        Ok(TempPart { path })
    }
}

impl Drop for TempPart {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn run_piped(command: &str, input: &[u8]) -> Result<String> {
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

/// The message with one header replaced: every existing line for
/// `name` (folded continuations included) is dropped, and, when
/// `value` is Some and non-empty, one `name: value` line is written
/// at the end of the header block. Used by edit-label; the line
/// ending of the original is kept.
pub fn with_header(raw: &[u8], name: &str, value: Option<&str>) -> Vec<u8> {
    let (head, body) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(at) => (&raw[..at + 2], &raw[at + 2..]),
        None => match raw.windows(2).position(|w| w == b"\n\n") {
            Some(at) => (&raw[..at + 1], &raw[at + 1..]),
            None => (raw, &raw[raw.len()..]),
        },
    };
    let crlf = head.windows(2).any(|w| w == b"\r\n");
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let prefix = format!("{}:", name.to_ascii_lowercase());
    let mut out = Vec::with_capacity(raw.len() + name.len() + 32);
    let mut skipping = false;
    for line in head.split_inclusive(|&b| b == b'\n') {
        let folded = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');
        if folded && skipping {
            continue;
        }
        let lower: Vec<u8> = line
            .iter()
            .take(prefix.len())
            .map(u8::to_ascii_lowercase)
            .collect();
        skipping = lower == prefix.as_bytes();
        if !skipping {
            out.extend_from_slice(line);
        }
    }
    if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(eol);
    }
    out.extend_from_slice(body);
    out
}

/// The message with its threading headers replaced: In-Reply-To and
/// References (folded continuation lines included) are dropped, and
/// the given ones written at the end of the header block, when there
/// are any. This is what break-thread and link-threads write back;
/// mutt does the same to the message itself (`mutt_break_thread`
/// clears both, `link_threads` sets In-Reply-To to the parent's id).
/// The line ending of the original is kept.
pub fn with_thread_headers(
    raw: &[u8],
    in_reply_to: Option<&str>,
    references: &[String],
    broken: bool,
) -> Vec<u8> {
    let (head, body) = match raw.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(at) => (&raw[..at + 2], &raw[at + 2..]),
        None => match raw.windows(2).position(|w| w == b"\n\n") {
            Some(at) => (&raw[..at + 1], &raw[at + 1..]),
            None => (raw, &raw[raw.len()..]),
        },
    };
    let crlf = head.windows(2).any(|w| w == b"\r\n");
    let eol: &[u8] = if crlf { b"\r\n" } else { b"\n" };
    let mut out = Vec::with_capacity(raw.len() + 128);
    let mut skipping = false;
    for line in head.split_inclusive(|&b| b == b'\n') {
        let folded = line.first().is_some_and(|b| *b == b' ' || *b == b'\t');
        if folded && skipping {
            continue;
        }
        let lower: Vec<u8> = line.iter().take(14).map(u8::to_ascii_lowercase).collect();
        skipping = lower.starts_with(b"in-reply-to:")
            || lower.starts_with(b"references:")
            || lower.starts_with(b"x-rmut-thread:");
        if !skipping {
            out.extend_from_slice(line);
        }
    }
    if let Some(id) = in_reply_to {
        out.extend_from_slice(b"In-Reply-To: ");
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(eol);
    }
    if !references.is_empty() {
        out.extend_from_slice(b"References: ");
        out.extend_from_slice(references.join(" ").as_bytes());
        out.extend_from_slice(eol);
    }
    if broken {
        out.extend_from_slice(format!("{BROKEN_HEADER}: {BROKEN_VALUE}").as_bytes());
        out.extend_from_slice(eol);
    }
    out.extend_from_slice(body);
    out
}

/// Decoded value of the first `name` header, read from disk (used by
/// the `~e` Sender pattern).
pub fn first_header(path: &Path, name: &str) -> Option<String> {
    let raw = fs::read(path).ok()?;
    let mail = parse_mail(&raw).ok()?;
    mail.get_headers().get_first_value(name)
}

/// The whole decoded header block as `Name: value` lines, for the
/// `~h` pattern (mutt matches the header text, not one field).
pub fn header_text(path: &Path) -> Option<String> {
    let raw = fs::read(path).ok()?;
    let mail = parse_mail(&raw).ok()?;
    let mut out = String::new();
    for header in mail.get_headers() {
        out.push_str(&header.get_key());
        out.push_str(": ");
        out.push_str(&header.get_value());
        out.push('\n');
    }
    Some(out)
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
        .map(|p| Part {
            mimetype: p.ctype.mimetype.clone(),
            filename: part_filename(p),
            size: p.get_body_raw().map(|b| b.len()).unwrap_or(0),
            is_text: p.ctype.mimetype.starts_with("text/"),
        })
        .collect())
}

/// The part's file name: Content-Disposition filename, or the
/// Content-Type name parameter.
fn part_filename(p: &ParsedMail) -> Option<String> {
    p.get_content_disposition()
        .params
        .get("filename")
        .cloned()
        .or_else(|| p.ctype.params.get("name").cloned())
        .map(|n| one_line(&n))
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
    fn control_characters_never_reach_a_one_line_field() {
        // A tab is the one that bites: the terminal expands it, so the
        // rest of an index row is pushed past the window edge and the
        // row wraps onto a second line.
        assert_eq!(one_line("before\ttab after"), "before tab after");
        assert_eq!(one_line("two\u{7}bells\u{1b}"), "two bells ");
        assert_eq!(one_line("nothing to do"), "nothing to do");

        let raw = concat!(
            "From: Tabbed\tSender <t@example.com>\r\n",
            "Subject: before\ttab after\r\n",
            "Date: Mon, 6 Jul 2026 10:00:00 +0200\r\n",
            "\r\nbody\r\n",
        );
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("cur-msg");
        std::fs::write(&path, raw).unwrap();
        let file = crate::maildir::MailFile {
            path: path.clone(),
            is_new: false,
            flags: Default::default(),
            size: raw.len() as u64,
        };
        let env = envelope(file).unwrap();
        assert_eq!(env.subject, "before tab after");
        assert_eq!(env.from, "Tabbed Sender");
        assert!(!env.from_full.contains('\t'));
        // The pager's header block is a set of one-line slots too.
        let view = load(&path).unwrap();
        assert!(
            view.all.iter().all(|(_, v)| !v.contains('\t')),
            "{:?}",
            view.all
        );
    }

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

    fn body_of(raw: &str) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(&path, raw).unwrap();
        load(&path).unwrap().body
    }

    #[test]
    fn render_shows_text_attachments_with_markers() {
        let body = body_of(concat!(
            "From: a@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "the body\r\n",
            "--b\r\n",
            "Content-Type: text/plain; name=\"notes.txt\"\r\n",
            "Content-Disposition: attachment; filename=\"notes.txt\"\r\n",
            "\r\n",
            "attached notes\r\n",
            "--b--\r\n",
        ));
        assert!(body.contains("[-- Attachment #1 --]"), "{body}");
        assert!(body.contains("the body"), "{body}");
        assert!(body.contains("[-- Attachment #2: notes.txt --]"), "{body}");
        assert!(
            body.contains("[-- Type: text/plain, Encoding: 7bit, Size: 0.0K --]"),
            "{body}"
        );
        assert!(body.contains("attached notes"), "{body}");
    }

    #[test]
    fn render_stubs_non_text_attachments() {
        let body = body_of(MULTIPART);
        assert!(body.contains("plain text"), "{body}");
        assert!(body.contains("[-- Attachment #2: report.pdf --]"), "{body}");
        assert!(
            body.contains("[-- Type: application/pdf, Encoding: base64, Size: 0.0K --]"),
            "{body}"
        );
        assert!(
            body.contains("[-- application/pdf is unsupported (use 'v' to view this part) --]"),
            "{body}"
        );
    }

    const ALTERNATIVE: &str = concat!(
        "From: a@example.com\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: multipart/alternative; boundary=\"b\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "plain version\r\n",
        "--b\r\n",
        "Content-Type: text/html\r\n",
        "\r\n",
        "<b>html version</b>\r\n",
        "--b--\r\n",
    );

    const FLOWED: &str = concat!(
        "From: a@example.com\r\n",
        "Subject: flowed\r\n",
        "MIME-Version: 1.0\r\n",
        "Content-Type: text/plain; charset=us-ascii; Format=Flowed\r\n",
        "\r\n",
        "This paragraph was \r\n",
        "split by the sender.\r\n",
        "\r\n",
        "> quoted and \r\n",
        "> continued\r\n",
        "-- \r\n",
        "Jane\r\n",
    );

    #[test]
    fn flowed_parts_come_back_as_paragraphs() {
        // The parameter is matched case-insensitively, like its value.
        let body = body_of(FLOWED);
        assert!(
            body.contains("This paragraph was split by the sender."),
            "{body}"
        );
        assert!(body.contains("> quoted and continued"), "{body}");
        // RFC 3676 keeps the signature separator a fixed line.
        assert!(body.contains("-- \nJane"), "{body:?}");
        // reflow_text = false leaves the sender's line breaks alone.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(&path, FLOWED).unwrap();
        let plain = load_with(
            &path,
            &Display {
                reflow: false,
                ..Display::default()
            },
        )
        .unwrap()
        .body;
        // Untouched, CRLF and all, exactly as the part arrived.
        assert!(plain.contains("This paragraph was \r\nsplit"), "{plain:?}");
    }

    #[test]
    fn alternative_order_outranks_the_text_ranking() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(&path, ALTERNATIVE).unwrap();
        let order = |types: &[&str]| Display {
            alternative_order: types.iter().map(|t| t.to_string()).collect(),
            ..Display::default()
        };
        // html asked for by name beats plain, which the ranking prefers.
        let body = load_with(&path, &order(&["text/html"])).unwrap().body;
        assert!(body.contains("<b>html version</b>"), "{body}");
        assert!(!body.contains("plain version"), "{body}");
        // First entry that is actually there wins.
        let body = load_with(&path, &order(&["text/enriched", "text/plain"]))
            .unwrap()
            .body;
        assert!(body.contains("plain version"), "{body}");
        // A wildcard takes the first part of that main type.
        let body = load_with(&path, &order(&["text/*"])).unwrap().body;
        assert!(body.contains("plain version"), "{body}");
        // Nothing listed matches: back to the ranking.
        let body = load_with(&path, &order(&["application/pdf"])).unwrap().body;
        assert!(body.contains("plain version"), "{body}");
        // An order entry beats an auto_view filter, unlike the ranking.
        let body = load_with(
            &path,
            &Display {
                filters: std::collections::HashMap::from([(
                    "text/html".to_string(),
                    "cat".to_string(),
                )]),
                alternative_order: vec!["text/plain".into()],
                ..Display::default()
            },
        )
        .unwrap()
        .body;
        assert!(body.contains("plain version"), "{body}");
    }

    #[test]
    fn render_entity_shows_the_whole_tree() {
        // What gpg hands back for an encrypted message with an
        // attachment: text plus a part that only a marker can show.
        let entity = concat!(
            "Content-Type: multipart/mixed; boundary=\"m\"\r\n",
            "\r\n",
            "--m\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "the secret plan\r\n",
            "--m\r\n",
            "Content-Type: application/pdf\r\n",
            "Content-Disposition: attachment; filename=\"plan.pdf\"\r\n",
            "Content-Transfer-Encoding: base64\r\n",
            "\r\n",
            "cGxhbg==\r\n",
            "--m--\r\n",
        );
        let body = render_entity(entity.as_bytes(), &Display::default());
        assert!(body.contains("the secret plan"), "{body}");
        assert!(body.contains("[-- Attachment #2: plan.pdf --]"), "{body}");
        // Not MIME at all: the text comes back as it stands.
        assert_eq!(
            render_entity(b"just words", &Display::default()),
            "just words"
        );
    }

    #[test]
    fn render_alternative_prefers_plain_but_autoview_wins() {
        // No filter: mutt's text ranking picks plain over html, no markers.
        let body = body_of(ALTERNATIVE);
        assert!(body.contains("plain version"), "{body}");
        assert!(!body.contains("html version"), "{body}");
        assert!(!body.contains("Attachment #"), "{body}");
        // An auto_view filter for text/html beats the text ranking.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("msg");
        std::fs::write(&path, ALTERNATIVE).unwrap();
        let filters =
            std::collections::HashMap::from([("text/html".to_string(), "cat".to_string())]);
        let body = load_with(
            &path,
            &Display {
                filters,
                ..Display::default()
            },
        )
        .unwrap()
        .body;
        assert!(body.contains("[-- Autoview using cat --]"), "{body}");
        assert!(body.contains("<b>html version</b>"), "{body}");
        assert!(!body.contains("plain version"), "{body}");
    }

    #[test]
    fn render_rfc822_shows_embedded_headers_and_body() {
        let body = body_of(concat!(
            "From: a@example.com\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
            "\r\n",
            "--b\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "see below\r\n",
            "--b\r\n",
            "Content-Type: message/rfc822\r\n",
            "\r\n",
            "From: jane@example.com\r\n",
            "Subject: inner\r\n",
            "\r\n",
            "inner body\r\n",
            "--b--\r\n",
        ));
        assert!(body.contains("[-- Attachment #2 --]"), "{body}");
        assert!(body.contains("[-- Type: message/rfc822"), "{body}");
        assert!(body.contains("From: jane@example.com"), "{body}");
        assert!(body.contains("Subject: inner"), "{body}");
        assert!(body.contains("inner body"), "{body}");
    }

    #[test]
    fn thread_headers_are_replaced_whole() {
        let raw = b"From: a@x\r\nReferences: <a@x>\r\n <b@x>\r\nSubject: s\r\nIn-Reply-To: <b@x>\r\n\r\nbody\r\n";
        let out = with_thread_headers(raw, None, &[], false);
        assert_eq!(out, b"From: a@x\r\nSubject: s\r\n\r\nbody\r\n");
        // break-thread's marker goes on, and comes off again when the
        // message is linked back under a parent.
        let broken = with_thread_headers(raw, None, &[], true);
        assert_eq!(
            broken,
            b"From: a@x\r\nSubject: s\r\nX-Rmut-Thread: broken\r\n\r\nbody\r\n"
        );
        let out = with_thread_headers(
            &broken,
            Some("<p@x>"),
            &["<r@x>".into(), "<p@x>".into()],
            false,
        );
        assert_eq!(
            out,
            b"From: a@x\r\nSubject: s\r\nIn-Reply-To: <p@x>\r\nReferences: <r@x> <p@x>\r\n\r\nbody\r\n"
        );
        // LF mail stays LF, and a message with no body is fine.
        let out = with_thread_headers(b"From: a@x\nSubject: s\n", Some("<p@x>"), &[], false);
        assert_eq!(out, b"From: a@x\nSubject: s\nIn-Reply-To: <p@x>\n");
    }

    #[test]
    fn list_actions_take_the_first_mailto_of_each_header() {
        let raw = b"From: a@x\r\nList-Id: <dev.example.com>\r\n\
List-Unsubscribe: <https://lists.example.com/leave>, <mailto:dev-leave@example.com?subject=x>\r\n\
List-Help: <https://lists.example.com/help>\r\nSubject: s\r\n\r\nbody\r\n";
        let actions = list_actions(raw);
        assert_eq!(actions.len(), 6);
        assert_eq!(
            actions[3],
            (
                "Unsubscribe",
                Some("mailto:dev-leave@example.com?subject=x".to_string())
            ),
            "the mailto wins over the https that came first"
        );
        assert_eq!(
            actions[0],
            ("Help", Some("https://lists.example.com/help".to_string())),
            "no mailto: the first URL, so the refusal can name it"
        );
        assert_eq!(actions[1], ("Post", None));
    }
}
