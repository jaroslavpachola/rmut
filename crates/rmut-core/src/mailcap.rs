//! RFC 1524 mailcap, read the way mutt reads it for `auto_view`: a
//! `[filters]` entry with an empty command means "the command is in
//! my mailcap", and only a `copiousoutput` entry can answer, since
//! that is the one promising plain text on stdout rather than a
//! window of its own.
//!
//! The files are `$MAILCAPS` (colon-separated) when it is set, and
//! otherwise mutt's default list: `~/.mailcap`, `/etc/mailcap`,
//! `/usr/etc/mailcap`, `/usr/local/etc/mailcap`, in that order. The
//! first entry that matches a type wins, `text/*` wildcards included.

use std::path::PathBuf;

/// One mailcap line: the type it handles, the view command, and the
/// flags rmut cares about.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Lowercased `type/subtype`, possibly `type/*`.
    pub mimetype: String,
    pub command: String,
    /// The command writes plain text to stdout: mutt's requirement
    /// for auto_view, and ours.
    pub copiousoutput: bool,
    /// The command wants the terminal, so it cannot render into the
    /// pager.
    pub needsterminal: bool,
    /// `test=`: a shell command that must succeed for the entry to
    /// apply (this is how a mailcap gates on `$DISPLAY`).
    pub test: Option<String>,
}

/// The mailcap files to consult, in order.
pub fn paths() -> Vec<PathBuf> {
    if let Ok(list) = std::env::var("MAILCAPS")
        && !list.trim().is_empty()
    {
        return list
            .split(':')
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    let mut out = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".mailcap"));
    }
    out.push(PathBuf::from("/etc/mailcap"));
    out.push(PathBuf::from("/usr/etc/mailcap"));
    out.push(PathBuf::from("/usr/local/etc/mailcap"));
    out
}

/// Every entry from every readable mailcap file, in search order. A
/// missing file is not an error, like mutt.
pub fn load() -> Vec<Entry> {
    let mut out = Vec::new();
    for path in paths() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.extend(parse(&text));
        }
    }
    out
}

/// Parse one mailcap file.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for line in logical_lines(text) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = split_fields(line);
        if fields.len() < 2 {
            continue;
        }
        let rest = fields.split_off(2);
        let command = fields.pop().unwrap_or_default().trim().to_string();
        let mimetype = fields.pop().unwrap_or_default().trim().to_lowercase();
        if mimetype.is_empty() || command.is_empty() || !mimetype.contains('/') {
            continue;
        }
        let mut entry = Entry {
            mimetype,
            command,
            copiousoutput: false,
            needsterminal: false,
            test: None,
        };
        for field in rest {
            let field = field.trim();
            let (key, value) = match field.split_once('=') {
                Some((k, v)) => (k.trim().to_lowercase(), Some(v.trim().to_string())),
                None => (field.to_lowercase(), None),
            };
            match (key.as_str(), value) {
                ("copiousoutput", _) => entry.copiousoutput = true,
                ("needsterminal", _) => entry.needsterminal = true,
                ("test", Some(v)) => entry.test = Some(v),
                _ => {}
            }
        }
        out.push(entry);
    }
    out
}

/// The command that renders `mimetype` inline, or None when no entry
/// can: mutt takes the first matching `copiousoutput` entry whose
/// `test` passes. An entry wanting the terminal, or one interpolating
/// a `%{parameter}` rmut does not have, is passed over.
pub fn command_for(entries: &[Entry], mimetype: &str) -> Option<String> {
    let want = mimetype.trim().to_lowercase();
    let main = want.split('/').next().unwrap_or_default();
    entries
        .iter()
        .filter(|e| e.mimetype == want || e.mimetype == format!("{main}/*"))
        .filter(|e| e.copiousoutput && !e.needsterminal && !e.command.contains("%{"))
        .find(|e| e.test.as_deref().is_none_or(test_passes))
        .map(|e| e.command.clone())
}

/// The viewer for `mimetype`, mutt's view-mailcap: the first matching
/// entry whose `test` passes, terminal-wanting or not, with whether
/// it writes to stdout (`copiousoutput`) rather than the terminal.
pub fn viewer_for(entries: &[Entry], mimetype: &str) -> Option<(String, bool)> {
    let want = mimetype.trim().to_lowercase();
    let main = want.split('/').next().unwrap_or_default();
    entries
        .iter()
        .filter(|e| e.mimetype == want || e.mimetype == format!("{main}/*"))
        .filter(|e| !e.command.contains("%{"))
        .find(|e| e.test.as_deref().is_none_or(test_passes))
        .map(|e| (e.command.clone(), e.copiousoutput))
}

/// Run a `test=` field: success is exit status zero, and a test that
/// cannot even start counts as failed.
fn test_passes(command: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Fold mailcap's backslash continuations into one line each.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let continues = trailing_backslashes(line) % 2 == 1;
        let piece = match continues {
            true => &line[..line.len() - 1],
            false => line,
        };
        match &mut pending {
            Some(buf) => buf.push_str(piece),
            None => pending = Some(piece.to_string()),
        }
        if !continues && let Some(done) = pending.take() {
            out.push(done);
        }
    }
    out.extend(pending);
    out
}

fn trailing_backslashes(line: &str) -> usize {
    line.chars().rev().take_while(|c| *c == '\\').count()
}

/// Split on unescaped semicolons, unescaping `\;` and `\\` as we go.
fn split_fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(next @ (';' | '\\')) => cur.push(next),
                Some(next) => {
                    cur.push('\\');
                    cur.push(next);
                }
                None => cur.push('\\'),
            },
            ';' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# a comment
text/html; lynx -dump %s; copiousoutput; nametemplate=%s.html
text/html; sensible-browser %s; test=test -n \"$DISPLAY\"
application/pdf; \\
  pdftotext -layout %s -; copiousoutput
text/x-patch; colordiff; copiousoutput
application/zip; unzip -l %s ; copiousoutput
video/mpeg; mpv %s; needsterminal
image/*; catimg %s; copiousoutput
broken line without a command
";

    #[test]
    fn parses_types_commands_and_flags() {
        let entries = parse(SAMPLE);
        let types: Vec<&str> = entries.iter().map(|e| e.mimetype.as_str()).collect();
        assert_eq!(
            types,
            [
                "text/html",
                "text/html",
                "application/pdf",
                "text/x-patch",
                "application/zip",
                "video/mpeg",
                "image/*",
            ]
        );
        assert!(entries[0].copiousoutput);
        assert!(!entries[1].copiousoutput);
        assert_eq!(entries[1].test.as_deref(), Some("test -n \"$DISPLAY\""));
        // The continuation line is folded into the command.
        assert_eq!(entries[2].command, "pdftotext -layout %s -");
        assert!(entries[5].needsterminal);
    }

    #[test]
    fn command_for_takes_the_first_copiousoutput_entry() {
        let entries = parse(SAMPLE);
        assert_eq!(
            command_for(&entries, "text/html").as_deref(),
            Some("lynx -dump %s")
        );
        assert_eq!(
            command_for(&entries, "TEXT/X-Patch").as_deref(),
            Some("colordiff")
        );
        // needsterminal cannot render into the pager.
        assert_eq!(command_for(&entries, "video/mpeg"), None);
        // A type/* entry answers for the whole main type.
        assert_eq!(
            command_for(&entries, "image/png").as_deref(),
            Some("catimg %s")
        );
        assert_eq!(command_for(&entries, "application/msword"), None);
    }

    #[test]
    fn a_failing_test_field_skips_the_entry() {
        let entries = parse(
            "text/html; never-run; copiousoutput; test=false\n\
             text/html; w3m -dump -T text/html; copiousoutput; test=true\n",
        );
        assert_eq!(
            command_for(&entries, "text/html").as_deref(),
            Some("w3m -dump -T text/html")
        );
    }

    #[test]
    fn escaped_semicolons_stay_in_the_command() {
        let entries = parse("text/plain; sed 's/a/b/'\\; cat; copiousoutput\n");
        assert_eq!(entries[0].command, "sed 's/a/b/'; cat");
        assert!(entries[0].copiousoutput);
    }
}
