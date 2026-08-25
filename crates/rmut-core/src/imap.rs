//! Minimal IMAP4rev1 client: exactly the commands rmut needs (LOGIN,
//! LIST, SELECT, UID FETCH/STORE, EXPUNGE, APPEND, NOOP, LOGOUT), over
//! plain TCP or TLS. Responses are read as logical lines with RFC 3501
//! `{N}` literals collected alongside the text.

use anyhow::{Context, Result, ensure};

use crate::maildir::Flags;
use crate::net::{self, Conn};

pub struct Client {
    conn: Conn,
    tag: u32,
}

/// One logical response line; the bytes of each `{N}` literal are in
/// `literals`, in order of appearance in `text`.
#[derive(Debug)]
struct Line {
    text: String,
    literals: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    pub no_select: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Select {
    pub exists: u32,
    pub uidvalidity: u32,
}

/// What a NOOP's untagged responses amounted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Changes {
    None,
    /// Only new arrivals: fetching from the last known UID suffices.
    NewOnly,
    /// Flag changes / expunges / anything else: reconcile everything.
    Full,
}

/// One message from a FETCH response; `body` holds whatever body
/// section the command asked for (headers or the full message).
#[derive(Debug)]
pub struct Fetched {
    pub uid: u32,
    pub flags: Flags,
    pub size: u64,
    pub body: Option<Vec<u8>>,
}

/// How many read timeouts make up mutt's ~25 minute IDLE window, so a
/// short io timeout does not turn into a chatty re-IDLE loop.
fn idle_waits(timeout: std::time::Duration) -> u32 {
    const WINDOW_SECS: u64 = 25 * 60;
    (WINDOW_SECS / timeout.as_secs().max(1)).max(1) as u32
}

impl Client {
    pub fn connect(host: &str, port: u16, tls: bool) -> Result<Client> {
        Client::connect_with(host, port, tls, &net::Cutoff::default())
    }

    /// The same, with a way to cut the connection short from another
    /// thread (mutt's Ctrl+G).
    pub fn connect_with(host: &str, port: u16, tls: bool, cutoff: &net::Cutoff) -> Result<Client> {
        // Port 993 is TLS from the first byte; on any other port the
        // session is upgraded with STARTTLS before LOGIN (unless
        // imap_tls = false, for tests).
        let implicit = tls && port == 993;
        let mut conn = Conn::new(
            net::connect(host, port, implicit, cutoff)?,
            format!("{host}:{port}"),
        );
        let greeting = read_line(&mut conn)?;
        ensure!(
            greeting.text.starts_with("* OK") || greeting.text.starts_with("* PREAUTH"),
            "unexpected IMAP greeting: {}",
            greeting.text
        );
        if tls && !implicit {
            conn.write_all(b"rmut0 STARTTLS\r\n")?;
            loop {
                let line = read_line(&mut conn)?;
                if let Some(rest) = line.text.strip_prefix("rmut0 ") {
                    ensure!(
                        rest.starts_with("OK"),
                        "server refused STARTTLS: {}",
                        line.text
                    );
                    break;
                }
            }
            let tcp = conn.into_stream().into_tcp()?;
            conn = Conn::new(net::wrap_tls(tcp, host)?, format!("{host}:{port}"));
        }
        Ok(Client { conn, tag: 0 })
    }

    pub fn login(&mut self, user: &str, password: &str) -> Result<()> {
        match (quoted(user), quoted(password)) {
            (Some(u), Some(p)) => {
                self.command(&format!("LOGIN {u} {p}"))
                    .context("IMAP login")?;
            }
            _ => {
                // Something unquotable, so send both as literals.
                let tag = self.next_tag();
                self.conn
                    .write_all(format!("{tag} LOGIN {{{}}}\r\n", user.len()).as_bytes())?;
                self.expect_continuation()?;
                self.conn
                    .write_all(format!("{user} {{{}}}\r\n", password.len()).as_bytes())?;
                self.expect_continuation()?;
                self.conn.write_all(format!("{password}\r\n").as_bytes())?;
                self.finish(&tag).context("IMAP login")?;
            }
        }
        Ok(())
    }

    /// SASL AUTHENTICATE with one client response (the OAuth
    /// mechanisms): send it on the server's first continuation; a
    /// second continuation carries an error blob, which an empty line
    /// converts into the tagged NO.
    pub fn authenticate(&mut self, mechanism: &str, response_b64: &str) -> Result<()> {
        let tag = self.next_tag();
        self.conn
            .write_all(format!("{tag} AUTHENTICATE {mechanism}\r\n").as_bytes())?;
        let mut response = Some(response_b64);
        loop {
            let line = read_line(&mut self.conn)?;
            if let Some(rest) = line
                .text
                .strip_prefix(tag.as_str())
                .and_then(|r| r.strip_prefix(' '))
            {
                ensure!(rest.starts_with("OK"), "server said: {rest}");
                return Ok(());
            }
            if line.text.starts_with('+') {
                match response.take() {
                    Some(payload) => self.conn.write_all(format!("{payload}\r\n").as_bytes())?,
                    None => self.conn.write_all(b"\r\n")?,
                }
            }
        }
    }

    pub fn list(&mut self) -> Result<Vec<Folder>> {
        let lines = self.command("LIST \"\" \"*\"")?;
        Ok(lines.iter().filter_map(parse_list).collect())
    }

    pub fn select(&mut self, mailbox: &str) -> Result<Select> {
        let lines = self.command(&format!("SELECT {}", mailbox_arg(mailbox)?))?;
        let mut sel = Select::default();
        for line in &lines {
            if let Some(n) = line
                .text
                .strip_prefix("* ")
                .and_then(|r| r.strip_suffix(" EXISTS"))
                .and_then(|n| n.parse().ok())
            {
                sel.exists = n;
            }
            if let Some(n) = number_after(&line.text, "[UIDVALIDITY ") {
                sel.uidvalidity = n as u32;
            }
        }
        Ok(sel)
    }

    /// UID and flags of the messages in `set` (e.g. `1:*`).
    pub fn uid_fetch_flags(&mut self, set: &str) -> Result<Vec<Fetched>> {
        let lines = self.command(&format!("UID FETCH {set} (UID FLAGS)"))?;
        Ok(lines.iter().filter_map(parse_fetch).collect())
    }

    /// Flags, size, and the full header block, for indexing new mail.
    pub fn uid_fetch_headers(&mut self, set: &str) -> Result<Vec<Fetched>> {
        let lines = self.command(&format!(
            "UID FETCH {set} (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])"
        ))?;
        Ok(lines.iter().filter_map(parse_fetch).collect())
    }

    /// The complete raw message.
    pub fn uid_fetch_full(&mut self, uid: u32) -> Result<Vec<u8>> {
        let lines = self.command(&format!("UID FETCH {uid} (UID BODY.PEEK[])"))?;
        lines
            .iter()
            .filter_map(parse_fetch)
            .find_map(|f| if f.uid == uid { f.body } else { None })
            .with_context(|| format!("server returned no body for UID {uid}"))
    }

    /// Overwrite the message's flags (deletion goes via `uid_delete`).
    pub fn uid_store_flags(&mut self, uid: u32, flags: Flags) -> Result<()> {
        self.command(&format!(
            "UID STORE {uid} FLAGS.SILENT ({})",
            imap_flags(flags)
        ))
        .map(drop)
    }

    pub fn uid_delete(&mut self, set: &str) -> Result<()> {
        self.command(&format!("UID STORE {set} +FLAGS.SILENT (\\Deleted)"))
            .map(drop)
    }

    pub fn expunge(&mut self) -> Result<()> {
        self.command("EXPUNGE").map(drop)
    }

    /// Server-side copy into another folder ($trash before a purge).
    pub fn uid_copy(&mut self, set: &str, mailbox: &str) -> Result<()> {
        self.command(&format!("UID COPY {set} {}", mailbox_arg(mailbox)?))
            .map(drop)
    }

    pub fn append(&mut self, mailbox: &str, flags: Flags, body: &[u8]) -> Result<()> {
        let arg = mailbox_arg(mailbox)?;
        let tag = self.next_tag();
        self.conn.write_all(
            format!(
                "{tag} APPEND {arg} ({}) {{{}}}\r\n",
                imap_flags(flags),
                body.len()
            )
            .as_bytes(),
        )?;
        self.expect_continuation()?;
        self.conn.write_all(body)?;
        self.conn.write_all(b"\r\n")?;
        self.finish(&tag).map(drop)
    }

    /// Server-side body search: UIDs whose text contains `text`
    /// (ASCII only; anything else needs CHARSET negotiation, so the
    /// caller falls back to matching locally).
    pub fn uid_search_body(&mut self, text: &str) -> Result<Vec<u32>> {
        let arg = quoted(text).context("search text needs a charset")?;
        let lines = self.command(&format!("UID SEARCH BODY {arg}"))?;
        let mut out = Vec::new();
        for line in &lines {
            if let Some(rest) = line.text.trim_end().strip_prefix("* SEARCH") {
                out.extend(
                    rest.split_whitespace()
                        .filter_map(|t| t.parse::<u32>().ok()),
                );
            }
        }
        Ok(out)
    }

    /// NOOP, classifying the server's untagged report: nothing, only
    /// new arrivals (EXISTS/RECENT), or anything else (flag changes,
    /// expunges, unknown lines) that needs a full reconciliation.
    pub fn noop_changes(&mut self) -> Result<Changes> {
        let lines = self.command("NOOP")?;
        if lines.is_empty() {
            return Ok(Changes::None);
        }
        let new_only = lines.iter().all(|l| {
            let t = l.text.trim_end();
            t.ends_with(" EXISTS") || t.ends_with(" RECENT")
        });
        let arrivals = lines.iter().any(|l| l.text.trim_end().ends_with(" EXISTS"));
        Ok(if new_only && arrivals {
            Changes::NewOnly
        } else {
            Changes::Full
        })
    }

    /// True when the server advertises IDLE (RFC 2177).
    pub fn supports_idle(&mut self) -> Result<bool> {
        let lines = self.command("CAPABILITY")?;
        Ok(lines.iter().any(|l| {
            l.text
                .to_ascii_uppercase()
                .split_whitespace()
                .any(|word| word == "IDLE")
        }))
    }

    /// UNSEEN count of a mailbox (STATUS must not target the currently
    /// selected one).
    pub fn status_unseen(&mut self, mailbox: &str) -> Result<u32> {
        let lines = self.command(&format!("STATUS {} (UNSEEN)", mailbox_arg(mailbox)?))?;
        Ok(lines
            .iter()
            .find_map(|l| number_after(&l.text, "UNSEEN "))
            .unwrap_or(0) as u32)
    }

    /// RFC 2177 IDLE: block until the server announces a change, `stop`
    /// is set (checked whenever the socket's read timeout fires), or
    /// ~25 minutes pass; re-issue before the server's half-hour
    /// limit. True = the mailbox changed.
    pub fn idle(&mut self, stop: &std::sync::atomic::AtomicBool) -> Result<bool> {
        use std::sync::atomic::Ordering;
        let tag = self.next_tag();
        self.conn.write_all(format!("{tag} IDLE\r\n").as_bytes())?;
        self.expect_continuation()?;
        let mut event = false;
        let mut waits = 0;
        // However long a read waits, keep the IDLE itself to about
        // 25 minutes: the timeout is the heartbeat, not the limit.
        let budget = idle_waits(net::io_timeout());
        while !stop.load(Ordering::Relaxed) && waits < budget {
            match read_line(&mut self.conn) {
                Ok(line) if line.text.starts_with('*') => {
                    event = true;
                    break;
                }
                Ok(_) => {}
                Err(err) if net::is_timeout(&err) => waits += 1,
                Err(err) => return Err(err),
            }
        }
        self.conn.write_all(b"DONE\r\n")?;
        self.finish(&tag)?;
        Ok(event)
    }

    /// Best-effort; the connection is unusable afterwards.
    pub fn logout(&mut self) {
        let tag = self.next_tag();
        let _ = self.conn.write_all(format!("{tag} LOGOUT\r\n").as_bytes());
    }

    // ---- protocol plumbing ----

    fn next_tag(&mut self) -> String {
        self.tag += 1;
        format!("a{}", self.tag)
    }

    fn command(&mut self, cmd: &str) -> Result<Vec<Line>> {
        let tag = self.next_tag();
        self.conn.write_all(format!("{tag} {cmd}\r\n").as_bytes())?;
        self.finish(&tag)
    }

    /// Collect untagged lines until our tagged OK/NO/BAD.
    fn finish(&mut self, tag: &str) -> Result<Vec<Line>> {
        let mut lines = Vec::new();
        loop {
            let line = read_line(&mut self.conn)?;
            if let Some(rest) = line
                .text
                .strip_prefix(tag)
                .and_then(|r| r.strip_prefix(' '))
            {
                ensure!(rest.starts_with("OK"), "server said: {rest}");
                return Ok(lines);
            }
            lines.push(line);
        }
    }

    fn expect_continuation(&mut self) -> Result<()> {
        loop {
            let line = read_line(&mut self.conn)?;
            if line.text.starts_with('+') {
                return Ok(());
            }
            ensure!(
                line.text.starts_with('*'),
                "expected continuation, server said: {}",
                line.text
            );
        }
    }
}

fn read_line(conn: &mut Conn) -> Result<Line> {
    let mut text = Vec::new();
    let mut literals = Vec::new();
    loop {
        loop {
            let b = conn.read_byte()?;
            if b == b'\n' {
                if text.last() == Some(&b'\r') {
                    text.pop();
                }
                break;
            }
            text.push(b);
            ensure!(text.len() <= 1 << 20, "response line too long");
        }
        match literal_len(&text) {
            Some(n) => {
                ensure!(n <= 256 << 20, "literal too large ({n} bytes)");
                let mut lit = Vec::with_capacity(n.min(1 << 20));
                conn.read_exact_to(&mut lit, n)?;
                literals.push(lit);
            }
            None => break,
        }
    }
    Ok(Line {
        text: String::from_utf8_lossy(&text).into_owned(),
        literals,
    })
}

/// Byte count when the line ends with an RFC 3501 literal marker `{N}`.
fn literal_len(text: &[u8]) -> Option<usize> {
    if text.last() != Some(&b'}') {
        return None;
    }
    let open = text.iter().rposition(|&b| b == b'{')?;
    let digits = &text[open + 1..text.len() - 1];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// The string as an IMAP quoted string, when its bytes allow one
/// (printable ASCII; quotes and backslashes escaped).
fn quoted(s: &str) -> Option<String> {
    if !s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return None;
    }
    Some(format!(
        "\"{}\"",
        s.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn mailbox_arg(mailbox: &str) -> Result<String> {
    quoted(mailbox).with_context(|| format!("unsupported mailbox name: {mailbox}"))
}

fn number_after(text: &str, marker: &str) -> Option<u64> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// `* LIST (\Noselect) "/" "INBOX/sub"`; name may be quoted, a
/// literal, or a bare atom.
fn parse_list(line: &Line) -> Option<Folder> {
    let rest = line.text.strip_prefix("* LIST ")?;
    let close = rest.find(')')?;
    let no_select = rest[..close].to_ascii_lowercase().contains("\\noselect");
    let mut rest = rest[close + 1..].trim_start();
    if let Some(after) = rest.strip_prefix("NIL") {
        rest = after;
    } else if rest.starts_with('"') {
        rest = take_quoted(rest)?.1;
    } else {
        return None;
    }
    let rest = rest.trim_start();
    let name = if rest.starts_with('"') {
        take_quoted(rest)?.0
    } else if rest.starts_with('{') {
        String::from_utf8_lossy(line.literals.first()?).into_owned()
    } else {
        rest.to_string()
    };
    Some(Folder { name, no_select })
}

/// Parse a leading quoted string: (contents, rest after closing quote).
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                out.push(bytes[i + 1]);
                i += 2;
            }
            b'"' => {
                return Some((String::from_utf8_lossy(&out).into_owned(), &s[i + 1..]));
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    None
}

/// `* 12 FETCH (UID 34 FLAGS (\Seen) RFC822.SIZE 120 BODY[...] {N})`.
/// Attribute order is up to the server, so scan by keyword; the body is
/// the line's (only) literal since that's all our FETCHes ask for.
fn parse_fetch(line: &Line) -> Option<Fetched> {
    let rest = line.text.strip_prefix("* ")?;
    let (_, attrs) = rest.split_once(" FETCH ")?;
    let uid = number_after(attrs, "UID ")? as u32;
    let flags = attrs
        .find("FLAGS (")
        .map(|i| {
            let inner = &attrs[i + "FLAGS (".len()..];
            parse_flags(&inner[..inner.find(')').unwrap_or(inner.len())])
        })
        .unwrap_or_default();
    Some(Fetched {
        uid,
        flags,
        size: number_after(attrs, "RFC822.SIZE ").unwrap_or(0),
        body: line.literals.first().cloned(),
    })
}

fn parse_flags(inner: &str) -> Flags {
    let mut flags = Flags::default();
    for token in inner.split_whitespace() {
        match token.to_ascii_lowercase().as_str() {
            "\\seen" => flags.seen = true,
            "\\answered" => flags.answered = true,
            "\\flagged" => flags.flagged = true,
            "\\deleted" => flags.deleted = true,
            "\\draft" => flags.draft = true,
            _ => {}
        }
    }
    flags
}

/// Maildir flags as an IMAP flag list, S/R/F/D/T → standard flags.
fn imap_flags(flags: Flags) -> String {
    let mut out: Vec<&str> = Vec::new();
    if flags.seen {
        out.push("\\Seen");
    }
    if flags.answered {
        out.push("\\Answered");
    }
    if flags.flagged {
        out.push("\\Flagged");
    }
    if flags.draft {
        out.push("\\Draft");
    }
    if flags.deleted {
        out.push("\\Deleted");
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver;

    #[test]
    fn starttls_refusal_is_an_error() {
        // tls on a non-993 port means STARTTLS; a server that refuses
        // it must fail the connect, never a plaintext LOGIN.
        let (port, handle) = testserver::imap(vec![testserver::Expect::fail(
            "STARTTLS",
            "NO too old for that",
        )]);
        let err = match Client::connect("127.0.0.1", port, true) {
            Err(err) => err,
            Ok(_) => panic!("connect must fail when STARTTLS is refused"),
        };
        assert!(
            err.to_string().contains("refused STARTTLS"),
            "unexpected error: {err:#}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn literal_len_only_at_line_end() {
        assert_eq!(literal_len(b"* 1 FETCH (BODY[] {42}"), Some(42));
        assert_eq!(literal_len(b"a1 OK done"), None);
        assert_eq!(literal_len(b"{} no digits"), None);
        assert_eq!(literal_len(b"{12} trailing"), None);
    }

    #[test]
    fn quoted_escapes_and_rejects() {
        assert_eq!(quoted("plain"), Some("\"plain\"".into()));
        assert_eq!(quoted(r#"a"b\c"#), Some(r#""a\"b\\c""#.into()));
        assert_eq!(quoted("naïve"), None);
        assert_eq!(quoted("nl\n"), None);
    }

    #[test]
    fn take_quoted_handles_escapes() {
        assert_eq!(
            take_quoted(r#""IN \"Q\" BOX" rest"#),
            Some(("IN \"Q\" BOX".into(), " rest"))
        );
        assert_eq!(take_quoted("\"unterminated"), None);
    }

    #[test]
    fn parse_list_variants() {
        let line = |text: &str| Line {
            text: text.into(),
            literals: vec![],
        };
        let f = parse_list(&line(r#"* LIST (\HasNoChildren) "/" "INBOX/sub""#)).unwrap();
        assert_eq!(f.name, "INBOX/sub");
        assert!(!f.no_select);
        let f = parse_list(&line(r#"* LIST (\Noselect) "." Public"#)).unwrap();
        assert_eq!(f.name, "Public");
        assert!(f.no_select);
        let lit = Line {
            text: r#"* LIST () "/" {9}"#.into(),
            literals: vec![b"Wei\xc3\x9fes B".to_vec()],
        };
        assert_eq!(parse_list(&lit).unwrap().name, "Weißes B");
        assert!(parse_list(&line("* STATUS foo")).is_none());
    }

    #[test]
    fn parse_fetch_reads_uid_flags_size_and_body() {
        let f = parse_fetch(&Line {
            text: "* 3 FETCH (UID 77 FLAGS (\\Seen \\Flagged $Junk) RFC822.SIZE 1234 BODY[HEADER] {20})"
                .into(),
            literals: vec![b"Subject: x\r\n\r\n".to_vec()],
        })
        .unwrap();
        assert_eq!(f.uid, 77);
        assert!(f.flags.seen && f.flags.flagged && !f.flags.deleted);
        assert_eq!(f.size, 1234);
        assert_eq!(f.body.unwrap(), b"Subject: x\r\n\r\n");
        // Flags-only fetch, server order reversed.
        let f = parse_fetch(&Line {
            text: "* 1 FETCH (FLAGS (\\Deleted) UID 5)".into(),
            literals: vec![],
        })
        .unwrap();
        assert_eq!(f.uid, 5);
        assert!(f.flags.deleted && f.body.is_none());
    }

    #[test]
    fn imap_flags_roundtrip_through_parse() {
        let flags = Flags {
            seen: true,
            answered: true,
            flagged: false,
            deleted: true,
            draft: true,
        };
        assert_eq!(parse_flags(&imap_flags(flags)), flags);
        assert_eq!(imap_flags(Flags::default()), "");
    }

    #[test]
    fn session_against_scripted_server() {
        let header = "Subject: hello\r\nFrom: a@example.com\r\n\r\n";
        let (port, handle) = testserver::imap(vec![
            testserver::Expect::new("LOGIN \"jane\" \"secret\"", String::new()),
            testserver::Expect::new(
                "SELECT \"INBOX\"",
                "* 2 EXISTS\r\n* OK [UIDVALIDITY 99] UIDs valid\r\n".into(),
            ),
            testserver::Expect::new(
                "UID FETCH 1:* (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
                format!(
                    "* 1 FETCH (UID 10 FLAGS (\\Seen) RFC822.SIZE 500 BODY[HEADER] {{{}}}\r\n{})\r\n",
                    header.len(),
                    header
                ),
            ),
            testserver::Expect::new(
                "UID STORE 10 FLAGS.SILENT (\\Seen \\Flagged)",
                String::new(),
            ),
            testserver::Expect::new("UID STORE 10,11 +FLAGS.SILENT (\\Deleted)", String::new()),
            testserver::Expect::new("EXPUNGE", "* 1 EXPUNGE\r\n".into()),
            testserver::Expect::new("APPEND \"Sent\" (\\Seen)", String::new()),
            testserver::Expect::new("NOOP", "* 3 EXISTS\r\n".into()),
            testserver::Expect::new("LOGOUT", String::new()),
        ]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        client.login("jane", "secret").unwrap();
        let sel = client.select("INBOX").unwrap();
        assert_eq!(sel.exists, 2);
        assert_eq!(sel.uidvalidity, 99);
        let fetched = client.uid_fetch_headers("1:*").unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].uid, 10);
        assert_eq!(fetched[0].body.as_deref().unwrap(), header.as_bytes());
        client
            .uid_store_flags(
                10,
                Flags {
                    seen: true,
                    flagged: true,
                    ..Default::default()
                },
            )
            .unwrap();
        client.uid_delete("10,11").unwrap();
        client.expunge().unwrap();
        client
            .append(
                "Sent",
                Flags {
                    seen: true,
                    ..Default::default()
                },
                b"From: a@b\r\n\r\nhi\r\n",
            )
            .unwrap();
        assert_eq!(client.noop_changes().unwrap(), Changes::NewOnly);
        client.logout();
        handle.join().unwrap();
    }

    #[test]
    fn noop_classifies_changes() {
        let (port, handle) = testserver::imap(vec![
            testserver::Expect::new("NOOP", String::new()),
            testserver::Expect::new("NOOP", "* 4 EXISTS\r\n* 1 RECENT\r\n".into()),
            testserver::Expect::new("NOOP", "* 2 EXPUNGE\r\n".into()),
            testserver::Expect::new(
                "NOOP",
                "* 1 FETCH (FLAGS (\\Seen))\r\n* 5 EXISTS\r\n".into(),
            ),
            testserver::Expect::new("NOOP", "* 0 RECENT\r\n".into()),
        ]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        assert_eq!(client.noop_changes().unwrap(), Changes::None);
        assert_eq!(client.noop_changes().unwrap(), Changes::NewOnly);
        assert_eq!(client.noop_changes().unwrap(), Changes::Full);
        assert_eq!(client.noop_changes().unwrap(), Changes::Full);
        // RECENT without EXISTS says nothing certain: reconcile.
        assert_eq!(client.noop_changes().unwrap(), Changes::Full);
        handle.join().unwrap();
    }

    #[test]
    fn authenticate_oauth_success_and_failure() {
        // XOAUTH2 for user=jane token=tok, precomputed base64.
        let blob = "dXNlcj1qYW5lAWF1dGg9QmVhcmVyIHRvawEB";
        let (port, handle) = testserver::imap(vec![
            testserver::Expect::untagged("AUTHENTICATE XOAUTH2", "+ \r\n".into()),
            testserver::Expect::new(blob, String::new()),
            // Second attempt: the server answers the response with an
            // error blob; the client's empty line fetches the NO.
            testserver::Expect::untagged("AUTHENTICATE XOAUTH2", "+ \r\n".into()),
            testserver::Expect::untagged(blob, "+ eyJzdGF0dXMiOiI0MDEifQ==\r\n".into()),
            testserver::Expect::fail("", "NO [AUTHENTICATIONFAILED] bad token"),
        ]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        client.authenticate("XOAUTH2", blob).unwrap();
        let err = client.authenticate("XOAUTH2", blob).unwrap_err();
        assert!(
            format!("{err:#}").contains("AUTHENTICATIONFAILED"),
            "{err:#}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn capability_and_status() {
        let (port, handle) = testserver::imap(vec![
            testserver::Expect::new("CAPABILITY", "* CAPABILITY IMAP4rev1 IDLE\r\n".into()),
            testserver::Expect::new(
                "STATUS \"Archive\" (UNSEEN)",
                "* STATUS \"Archive\" (UNSEEN 3)\r\n".into(),
            ),
            testserver::Expect::new("CAPABILITY", "* CAPABILITY IMAP4rev1\r\n".into()),
        ]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        assert!(client.supports_idle().unwrap());
        assert_eq!(client.status_unseen("Archive").unwrap(), 3);
        // IMAP4rev1 must not read as IDLE support.
        assert!(!client.supports_idle().unwrap());
        handle.join().unwrap();
    }

    #[test]
    fn the_idle_window_is_the_same_however_long_a_read_waits() {
        // mutt re-issues IDLE before the server's half hour; the read
        // timeout is only how often the stop flag is looked at.
        assert_eq!(idle_waits(std::time::Duration::from_secs(60)), 25);
        assert_eq!(idle_waits(std::time::Duration::from_secs(30)), 50);
        assert_eq!(idle_waits(std::time::Duration::from_secs(5)), 300);
        assert_eq!(idle_waits(std::time::Duration::from_secs(0)), 1500);
    }

    #[test]
    fn idle_reports_events_and_stop() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (port, handle) = testserver::imap(vec![
            testserver::Expect::untagged("IDLE", "+ idling\r\n* 3 EXISTS\r\n".into()),
            testserver::Expect::new("DONE", String::new()),
            testserver::Expect::untagged("IDLE", "+ idling\r\n".into()),
            testserver::Expect::new("DONE", String::new()),
            testserver::Expect::new("LOGOUT", String::new()),
        ]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        let stop = AtomicBool::new(false);
        // The server announces a change: idle reports it.
        assert!(client.idle(&stop).unwrap());
        // Stop already set: idle sends DONE straight away, no event.
        stop.store(true, Ordering::Relaxed);
        assert!(!client.idle(&stop).unwrap());
        client.logout();
        handle.join().unwrap();
    }

    #[test]
    fn login_failure_is_reported() {
        let (port, handle) = testserver::imap(vec![testserver::Expect::fail(
            "LOGIN",
            "NO [AUTHENTICATIONFAILED] bad credentials",
        )]);
        let mut client = Client::connect("127.0.0.1", port, false).unwrap();
        let err = client.login("jane", "wrong").unwrap_err();
        assert!(format!("{err:#}").contains("AUTHENTICATIONFAILED"));
        handle.join().unwrap();
    }
}
