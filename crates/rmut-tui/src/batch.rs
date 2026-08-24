//! Sending without the TUI: `rmut -s subject -- addr < body`, the
//! drop-in for scripts and `sendmail`-style one-shots. The draft is
//! built and submitted through the same core pieces the compose menu
//! uses, so an address, an attachment, or an Fcc behaves the same way
//! in both; what is missing here is anything interactive (no editor,
//! no compose menu, no PGP prompts).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rmut_core::config::{Account, Config};
use rmut_core::{compose, maildir};

/// One outgoing message assembled from the command line.
#[derive(Debug, Default)]
pub struct Outgoing {
    pub to: String,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: String,
    pub body: String,
    pub attachments: Vec<PathBuf>,
}

/// Build and submit the message. Returns the note a caller may print;
/// every failure is an error, so the exit code tells a script what
/// happened.
pub fn send(config: &Config, out: &Outgoing) -> Result<String> {
    if compose::addresses(&out.to).is_empty()
        && compose::addresses(out.cc.as_deref().unwrap_or_default()).is_empty()
        && compose::addresses(out.bcc.as_deref().unwrap_or_default()).is_empty()
    {
        bail!("no recipients");
    }
    let host = maildir::hostname();
    // No mailbox is open, so the identity comes from [identity], the
    // account layer, and any [[identities]] rule matching the
    // recipients (the folder glob has nothing to match).
    let rcpts = compose::addresses(&out.to);
    let from = config
        .identity_for("", &rcpts, None)
        .from_line()
        .unwrap_or_else(|| default_from(&host));
    let mut text = compose::draft_text(
        &compose::DraftHeaders {
            from: Some(from),
            to: out.to.clone(),
            cc: out.cc.clone(),
            subject: out.subject.clone(),
            in_reply_to: None,
            references: None,
        },
        &out.body,
    );
    // mutt's my_hdr applies to batch mail too: the same merge the TUI
    // makes when it stages a draft.
    text = compose::apply_my_hdr(&text, &config.mail.my_hdr);
    if let Some(bcc) = out.bcc.as_deref().filter(|b| !b.trim().is_empty()) {
        // draft_text has no Bcc slot; it goes in ahead of the blank
        // line, where smtp_envelope will find and strip it.
        text = insert_header(&text, &format!("Bcc: {bcc}"));
    }
    let attachments: Vec<compose::Attachment> = out
        .attachments
        .iter()
        .map(|path| {
            anyhow::ensure!(path.is_file(), "no such file: {}", path.display());
            Ok(compose::Attachment {
                path: path.clone(),
                mime: None,
                description: None,
            })
        })
        .collect::<Result<_>>()?;
    let final_text = compose::finalize(
        &text,
        &compose::from_address(&text).context("cannot parse the From address")?,
        &compose::make_message_id(&host),
        &compose::rfc2822_now(),
    )?;
    let final_text = if attachments.is_empty() {
        final_text
    } else {
        let (head, body) = final_text
            .split_once("\n\n")
            .context("draft has no header block")?;
        let entity = compose::mixed_entity(body, &attachments, None, config.mail.text_flowed)?;
        // The same join the compose menu makes, MIME-Version included.
        format!("{}\nMIME-Version: 1.0\n{entity}", head.trim_end())
    };
    match smtp_account(config) {
        Some(account) => crate::app::send_via_smtp(&account, &final_text)?,
        None => {
            crate::app::run_sendmail(final_text.as_bytes(), config.mail.sendmail.as_deref(), None)?
        }
    }
    let mut note = String::from("message sent");
    if config.mail.copy == Some(false) {
        return Ok(note);
    }
    if let Some(sent) = fcc_hook_target(config, &final_text).or_else(|| sent_maildir(config)) {
        let flags = maildir::Flags {
            seen: true,
            ..Default::default()
        };
        match maildir::deliver(&sent, final_text.as_bytes(), flags) {
            Ok(_) => note += &format!(", copy in {}", sent.display()),
            // A failed copy is worth saying, but the mail did go out.
            Err(err) => note += &format!(", Fcc failed: {err}"),
        }
    }
    Ok(note)
}

/// mutt's fcc-hook for a batch send: the first entry whose pattern
/// matches the message decides where the copy goes. A bad pattern is
/// skipped rather than failing the send, since the mail already went.
fn fcc_hook_target(config: &Config, text: &str) -> Option<PathBuf> {
    let env = compose::draft_envelope(text, Path::new(""));
    let scope = rmut_core::pattern::Scope::default();
    for hook in &config.fcc_hooks {
        let Ok(patterns) = rmut_core::pattern::parse(&hook.pattern) else {
            continue;
        };
        if rmut_core::pattern::matches_in(&patterns, &env, scope, None) {
            return Some(expand_tilde(&hook.mailbox));
        }
    }
    None
}

/// `[mail] sent` when it is a local maildir. Batch mode keeps no IMAP
/// copy: an APPEND would need the account opened, which is the TUI's
/// job, so a remote Sent folder is left to the interactive send.
fn sent_maildir(config: &Config) -> Option<PathBuf> {
    let sent = config.mail.sent.as_deref()?;
    if sent.starts_with("imap:") {
        return None;
    }
    let dir = expand_tilde(sent);
    dir.join("cur").is_dir().then_some(dir)
}

/// The account outgoing mail goes through, matching the TUI's rule:
/// explicit sendmail configuration wins, else the first account that
/// has an SMTP host.
fn smtp_account(config: &Config) -> Option<Account> {
    if std::env::var("RMUT_SENDMAIL").is_ok() || config.mail.sendmail.is_some() {
        return None;
    }
    config
        .accounts
        .iter()
        .find(|a| a.smtp_host.is_some())
        .cloned()
}

fn insert_header(text: &str, header: &str) -> String {
    match text.split_once("\n\n") {
        Some((head, body)) => format!("{head}\n{header}\n\n{body}"),
        None => format!("{header}\n{text}"),
    }
}

fn default_from(host: &str) -> String {
    std::env::var("EMAIL").unwrap_or_else(|_| {
        let user = std::env::var("USER").unwrap_or_else(|_| "rmut".into());
        format!("{user}@{host}")
    })
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(input)
}

/// The body for a batch send: `-i FILE` when given, otherwise stdin
/// (empty when it is a terminal, so an interactive typo does not hang
/// waiting for input).
pub fn read_body(include: Option<&Path>) -> Result<String> {
    if let Some(path) = include {
        return std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()));
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(String::new());
    }
    let mut body = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut body).context("reading stdin")?;
    Ok(body)
}
