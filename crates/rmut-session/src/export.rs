//! Export: the messages a pattern finds, as records for another
//! program (`rmut --export`). It runs with no front end, so where the
//! session would hand a slow IMAP job to the poll loop, export waits
//! for it instead.
//!
//! What a record carries is the mail as it stands: nothing is marked
//! read, nothing is synced, and encrypted mail stays encrypted, since
//! gpg never runs here. The body is cleaned the way triage cleans it,
//! but whole.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use rmut_core::{message, pattern, remote};

use crate::Session;
use crate::worker::Job;

/// One exported message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    /// The normalized `<...>` Message-ID, when there is one.
    pub message_id: Option<String>,
    /// The mailbox as it was opened (a path, or an `imap:` spec).
    pub mailbox: String,
    /// The message's file: on IMAP, its cache copy.
    pub file: PathBuf,
    /// On IMAP, the server's UID for it.
    pub uid: Option<u32>,
    /// The Date header as written.
    pub date: Option<String>,
    /// The same date in Unix seconds, 0 when it would not parse.
    pub timestamp: i64,
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    /// unread, replied, flagged: those that apply.
    pub status: Vec<&'static str>,
    pub list: Option<String>,
    pub label: Option<String>,
    /// Each attachment's file name, or its type when it has none.
    /// None with the bodies left out: it takes the whole message.
    pub attachments: Option<Vec<String>>,
    /// None with the bodies left out (`--no-body`).
    pub body: Option<String>,
    /// Why the body and attachments came out empty, when reading or
    /// converting them failed: the record still goes out, the export
    /// does not stop at one bad message.
    pub error: Option<String>,
}

/// How the records are written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// One JSON object per line.
    Jsonl,
    /// A heading per message, its headers, its body.
    Markdown,
}

impl Format {
    pub fn parse(name: &str) -> Option<Format> {
        match name {
            "jsonl" | "json" => Some(Format::Jsonl),
            "md" | "markdown" => Some(Format::Markdown),
            _ => None,
        }
    }
}

impl Session {
    /// The messages `pattern` matches, in the index's order, as
    /// records. `all` (or an empty pattern) takes every message, as a
    /// limit does. IMAP bodies not yet cached are fetched first; one
    /// that cannot be is an error rather than a record without a body.
    /// Without `bodies` the records stop at the headers, and only what
    /// the pattern itself has to read is fetched: on IMAP that is the
    /// difference between the cache and the whole folder's mail.
    pub fn export(&mut self, pattern: &str, bodies: bool) -> Result<Vec<Record>> {
        let pattern = pattern.trim();
        // Whatever the open left running goes first: one connection,
        // one conversation.
        self.settle();
        // A huge folder's first open mirrors only its newest headers
        // and leaves the rest to a background backfill, which the TUI
        // runs and this never does: fetch them here, or the export
        // would end at the window without a word.
        if let Some(imap) = &mut self.imap {
            let uids = imap.take_backfill();
            if !uids.is_empty() {
                imap.blocking(Job::FetchHeaders(uids))
                    .map_err(|err| anyhow!("cannot fetch the message headers: {err:#}"))?;
                self.rescan();
            }
        }
        if pattern.is_empty() || pattern == "all" {
            self.limit = None;
        } else {
            let patterns = self
                .compile_search(pattern)
                .map_err(|err| anyhow!("bad pattern: {err}"))?;
            // A plain ~b term goes to the server; the answer lands in
            // body_hits (or body_local, when the server would not
            // say), and the next round asks about the next term.
            while !self.body_terms_ready(&patterns, Box::new(|_| {})) {
                self.settle();
            }
            // What the server did not answer (a regex, a ~B, a server
            // that would not search) is read from the cache, where a
            // message may be headers only: those bodies come first,
            // or the match would miss them without a word.
            let local = pattern::mentions_whole(&patterns)
                || pattern::body_terms(&patterns)
                    .iter()
                    .any(|term| !self.body_hits.contains_key(term));
            if local {
                let all: Vec<PathBuf> = self.msgs.iter().map(|m| m.env.file.path.clone()).collect();
                self.fetch_missing(&all)?;
            }
            self.limit = Some((pattern.to_string(), patterns));
        }
        self.rebuild_visible(None);
        let targets = self.visible.clone();
        if bodies {
            let paths: Vec<PathBuf> = targets
                .iter()
                .map(|&i| self.msgs[i].env.file.path.clone())
                .collect();
            self.fetch_missing(&paths)?;
        }
        Ok(targets.iter().map(|&i| self.record(i, bodies)).collect())
    }

    /// The bodies of these messages that the IMAP cache holds as
    /// headers only, fetched (without marking anything read) and
    /// waited for.
    fn fetch_missing(&mut self, paths: &[PathBuf]) -> Result<()> {
        let missing: Vec<PathBuf> = paths
            .iter()
            .filter(|path| remote::is_partial(path))
            .cloned()
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let imap = self
            .imap
            .as_mut()
            .ok_or_else(|| anyhow!("{} message(s) have no body", missing.len()))?;
        imap.blocking(Job::FetchBodies(missing))
            .map_err(|err| anyhow!("cannot fetch the messages: {err:#}"))?;
        Ok(())
    }

    fn record(&self, i: usize, bodies: bool) -> Record {
        let env = &self.msgs[i].env;
        let path = &env.file.path;
        let flags = env.file.flags;
        let mut status = Vec::new();
        if env.file.is_new || !flags.seen {
            status.push("unread");
        }
        if flags.answered {
            status.push("replied");
        }
        if flags.flagged {
            status.push("flagged");
        }
        let (attachments, body, error) = if bodies {
            match guarded(|| self.contents(path)) {
                Ok((attachments, body)) => (Some(attachments), Some(body), None),
                Err(err) => (Some(Vec::new()), Some(String::new()), Some(err)),
            }
        } else {
            (None, None, None)
        };
        Record {
            message_id: env.msg_id.clone(),
            mailbox: self.title.clone(),
            file: path.clone(),
            uid: self.imap.as_ref().and_then(|_| remote::uid_of(path)),
            date: message::first_header(path, "Date"),
            timestamp: env.date,
            from: env.from_full.clone(),
            to: env.to.clone(),
            cc: env.cc.clone(),
            subject: env.subject.clone(),
            status,
            list: env.list.clone(),
            label: env.label.clone(),
            attachments,
            body,
            error,
        }
    }

    /// The attachments' names and the cleaned body text.
    fn contents(&self, path: &std::path::Path) -> Result<(Vec<String>, String)> {
        // A MIME tree that will not split still leaves the body text.
        let attachments = message::parts(path)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.filename.is_some() || !p.is_text)
            .map(|p| p.filename.unwrap_or(p.mimetype))
            .collect();
        let text = message::body_text(path)?;
        let text = crate::triage::cleaned(&text, &self.quote_re, usize::MAX, usize::MAX);
        // The blank lines a dropped quote or signature leaves at the
        // end are not the message's.
        let body = match text.trim_end() {
            "" => String::new(),
            kept => format!("{kept}\n"),
        };
        Ok((attachments, body))
    }
}

/// `f`'s result, with an error or a panic (a bug in reading some odd
/// message) turned into its message.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T, String> {
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(format!("{err:#}")),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            Err(format!("rmut bug: {what}"))
        }
    }
}

/// The records in `format`, ready to write.
pub fn render(records: &[Record], format: Format) -> String {
    let mut out = String::new();
    for record in records {
        match format {
            Format::Jsonl => {
                out.push_str(&json(record));
                out.push('\n');
            }
            Format::Markdown => out.push_str(&markdown(record)),
        }
    }
    out
}

fn json(r: &Record) -> String {
    let opt = |v: &Option<String>| v.as_deref().map_or("null".into(), quote);
    let list = |v: &[String]| {
        let items: Vec<String> = v.iter().map(|s| quote(s)).collect();
        format!("[{}]", items.join(","))
    };
    let status: Vec<String> = r.status.iter().map(|s| quote(s)).collect();
    // Left out with the bodies, rather than null: a null body would
    // read as an empty message.
    let mut tail = String::new();
    if let Some(attachments) = &r.attachments {
        tail.push_str(&format!(",\"attachments\":{}", list(attachments)));
    }
    if let Some(body) = &r.body {
        tail.push_str(&format!(",\"body\":{}", quote(body)));
    }
    if let Some(error) = &r.error {
        tail.push_str(&format!(",\"error\":{}", quote(error)));
    }
    format!(
        "{{\"message_id\":{},\"mailbox\":{},\"file\":{},\"uid\":{},\"date\":{},\
         \"timestamp\":{},\"from\":{},\"to\":{},\"cc\":{},\"subject\":{},\
         \"status\":[{}],\"list\":{},\"label\":{}{tail}}}",
        opt(&r.message_id),
        quote(&r.mailbox),
        quote(&r.file.to_string_lossy()),
        r.uid.map_or("null".into(), |u| u.to_string()),
        opt(&r.date),
        r.timestamp,
        quote(&r.from),
        list(&r.to),
        list(&r.cc),
        quote(&r.subject),
        status.join(","),
        opt(&r.list),
        opt(&r.label),
    )
}

/// A JSON string literal.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn markdown(r: &Record) -> String {
    let subject = match r.subject.trim() {
        "" => "(no subject)",
        s => s,
    };
    let mut out = format!("## {subject}\n\n");
    let mut field = |name: &str, value: &str| {
        if !value.is_empty() {
            out.push_str(&format!("- {name}: {value}\n"));
        }
    };
    field("From", &r.from);
    field("To", &r.to.join(", "));
    field("Cc", &r.cc.join(", "));
    field("Date", r.date.as_deref().unwrap_or(""));
    field("Message-ID", r.message_id.as_deref().unwrap_or(""));
    field("Status", &r.status.join(", "));
    field("List", r.list.as_deref().unwrap_or(""));
    field("Label", r.label.as_deref().unwrap_or(""));
    field(
        "Attachments",
        &r.attachments.as_deref().unwrap_or_default().join(", "),
    );
    field("Error", r.error.as_deref().unwrap_or(""));
    out.push('\n');
    let Some(body) = &r.body else {
        return out;
    };
    // Fenced, so a body's own `#` lines and list markers stay text;
    // the fence is longer than any backtick run inside.
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    out.push_str(&format!("{fence}text\n{body}"));
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("{fence}\n\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        Record {
            message_id: Some("<a1@example.com>".into()),
            mailbox: "/mail/inbox".into(),
            file: PathBuf::from("/mail/inbox/cur/1:2,S"),
            date: Some("Mon, 6 Jul 2026 10:00:00 +0200".into()),
            timestamp: 1_783_324_800,
            from: "Jane Doe <jane@example.com>".into(),
            to: vec!["alex@example.com".into()],
            subject: "Invoice \"July\"".into(),
            status: vec!["unread"],
            attachments: Some(vec!["invoice.pdf".into()]),
            body: Some("Hi,\n\tthe total is 10.\n".into()),
            ..Record::default()
        }
    }

    #[test]
    fn a_record_is_one_json_line() {
        let line = render(&[sample()], Format::Jsonl);
        assert_eq!(line.lines().count(), 1);
        assert!(
            line.starts_with("{\"message_id\":\"<a1@example.com>\","),
            "{line}"
        );
        assert!(line.contains("\"uid\":null,"), "{line}");
        assert!(
            line.contains("\"subject\":\"Invoice \\\"July\\\"\""),
            "{line}"
        );
        assert!(line.contains("\"status\":[\"unread\"]"), "{line}");
        assert!(line.contains("\"cc\":[]"), "{line}");
        assert!(line.contains("\"label\":null"), "{line}");
        assert!(
            line.contains("\"body\":\"Hi,\\n\\tthe total is 10.\\n\"}"),
            "{line}"
        );
        assert_eq!(quote("\u{1}é"), "\"\\u0001é\"");
    }

    #[test]
    fn markdown_fences_the_body() {
        let mut r = sample();
        r.body = Some("# not a heading\n```\ncode\n```\n".into());
        let md = render(&[r], Format::Markdown);
        assert!(
            md.starts_with("## Invoice \"July\"\n\n- From: Jane Doe"),
            "{md}"
        );
        assert!(md.contains("- Attachments: invoice.pdf\n"), "{md}");
        assert!(!md.contains("- Cc:"), "{md}");
        assert!(
            md.contains("````text\n# not a heading\n```\ncode\n```\n````\n"),
            "{md}"
        );
    }

    #[test]
    fn a_failed_body_is_named_and_the_record_kept() {
        let r = Record {
            attachments: Some(Vec::new()),
            body: Some(String::new()),
            error: Some("rmut bug: oops".into()),
            ..sample()
        };
        let line = render(std::slice::from_ref(&r), Format::Jsonl);
        assert!(
            line.ends_with(",\"body\":\"\",\"error\":\"rmut bug: oops\"}\n"),
            "{line}"
        );
        let md = render(&[r], Format::Markdown);
        assert!(md.contains("- Error: rmut bug: oops\n"), "{md}");
    }

    #[test]
    fn a_panic_or_an_error_comes_back_as_its_message() {
        let quiet = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let caught = guarded::<()>(|| panic!("byte index 32 is not a char boundary"));
        std::panic::set_hook(quiet);
        assert_eq!(
            caught,
            Err("rmut bug: byte index 32 is not a char boundary".into())
        );
        let caught = guarded::<()>(|| Err(anyhow!("cannot read it")));
        assert_eq!(caught, Err("cannot read it".into()));
        assert_eq!(guarded(|| Ok(7)), Ok(7));
    }

    #[test]
    fn without_bodies_a_record_stops_at_the_headers() {
        let r = Record {
            attachments: None,
            body: None,
            ..sample()
        };
        let line = render(std::slice::from_ref(&r), Format::Jsonl);
        assert!(line.ends_with("\"label\":null}\n"), "{line}");
        assert!(
            !line.contains("body") && !line.contains("attachments"),
            "{line}"
        );
        let md = render(&[r], Format::Markdown);
        assert!(md.ends_with("- Status: unread\n\n"), "{md}");
        assert!(!md.contains("```"), "{md}");
    }
}
