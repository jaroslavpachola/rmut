//! Draft building and finalizing for outgoing mail.

use anyhow::{Result, ensure};
use chrono::{Local, TimeZone};

pub struct DraftHeaders {
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// The text the user edits in $EDITOR: header block, blank line, body.
pub fn draft_text(h: &DraftHeaders, body: &str) -> String {
    let mut out = format!("To: {}\n", h.to);
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

pub fn forward_subject(orig: &str) -> String {
    let t = orig.trim();
    if t.to_lowercase().starts_with("fwd:") {
        t.to_string()
    } else {
        format!("Fwd: {t}")
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_do_not_stack_prefixes() {
        assert_eq!(reply_subject("Lunch"), "Re: Lunch");
        assert_eq!(reply_subject("RE: Lunch"), "RE: Lunch");
        assert_eq!(forward_subject("Lunch"), "Fwd: Lunch");
        assert_eq!(forward_subject("fwd: Lunch"), "fwd: Lunch");
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
                to: "a@x".into(),
                cc: Some("".into()),
                subject: "s".into(),
                in_reply_to: None,
                references: None,
            },
            "hi",
        );
        assert_eq!(text, "To: a@x\nSubject: s\n\nhi\n");
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
}
