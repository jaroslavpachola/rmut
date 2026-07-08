use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Maildir flags stored in the filename after `:2,` (see
/// <https://cr.yp.to/proto/maildir.html>).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags {
    pub seen: bool,
    pub answered: bool,
    pub flagged: bool,
    pub deleted: bool,
    pub draft: bool,
}

impl Flags {
    pub fn from_filename(name: &str) -> Self {
        let mut flags = Flags::default();
        if let Some((_, info)) = name.rsplit_once(":2,") {
            for c in info.chars() {
                match c {
                    'S' => flags.seen = true,
                    'R' => flags.answered = true,
                    'F' => flags.flagged = true,
                    'T' => flags.deleted = true,
                    'D' => flags.draft = true,
                    _ => {}
                }
            }
        }
        flags
    }

    /// The `:2,...` info suffix encoding these flags, letters in the
    /// ASCII order the spec requires.
    pub fn to_info(self) -> String {
        let mut info = String::from(":2,");
        if self.draft {
            info.push('D');
        }
        if self.flagged {
            info.push('F');
        }
        if self.answered {
            info.push('R');
        }
        if self.seen {
            info.push('S');
        }
        if self.deleted {
            info.push('T');
        }
        info
    }

    /// Status column as mutt renders it in the index.
    pub fn status_char(&self, is_new: bool) -> char {
        if self.deleted {
            'D'
        } else if is_new {
            'N'
        } else if self.answered {
            'r'
        } else if !self.seen {
            'O'
        } else {
            ' '
        }
    }
}

#[derive(Debug, Clone)]
pub struct MailFile {
    pub path: PathBuf,
    pub is_new: bool,
    pub flags: Flags,
    pub size: u64,
}

/// Scan a maildir's `new/` and `cur/` subdirectories.
pub fn scan(dir: &Path) -> Result<Vec<MailFile>> {
    let cur = dir.join("cur");
    let new = dir.join("new");
    if !cur.is_dir() || !new.is_dir() {
        bail!("{} is not a maildir (missing cur/ or new/)", dir.display());
    }
    let mut out = Vec::new();
    for (sub, is_new) in [(cur, false), (new, true)] {
        let entries = sub
            .read_dir()
            .with_context(|| format!("reading {}", sub.display()))?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            out.push(MailFile {
                path: entry.path(),
                is_new,
                flags: Flags::from_filename(&name),
                size: size_from_name(&name).unwrap_or(entry.metadata()?.len()),
            });
        }
    }
    Ok(out)
}

/// Filename without the `:2,` info suffix.
fn base_name(name: &str) -> &str {
    name.rsplit_once(":2,").map_or(name, |(base, _)| base)
}

/// The `,S=<bytes>` message size some tools (and our IMAP cache) put in
/// the base name — authoritative when present, since a cached file may
/// hold only the headers.
fn size_from_name(name: &str) -> Option<u64> {
    let rest = base_name(name).rsplit_once(",S=")?.1;
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Write the file's current flags to disk by renaming it into `cur/`
/// with the matching info suffix. Returns the new path.
pub fn store_flags(file: &MailFile) -> Result<PathBuf> {
    let name = file
        .path
        .file_name()
        .context("mail file has no filename")?
        .to_string_lossy()
        .into_owned();
    let maildir = file
        .path
        .parent()
        .and_then(Path::parent)
        .context("mail file is not inside a maildir")?;
    let target = maildir
        .join("cur")
        .join(format!("{}{}", base_name(&name), file.flags.to_info()));
    if target != file.path {
        fs::rename(&file.path, &target)
            .with_context(|| format!("renaming to {}", target.display()))?;
    }
    Ok(target)
}

pub fn remove(file: &MailFile) -> Result<()> {
    fs::remove_file(&file.path).with_context(|| format!("removing {}", file.path.display()))
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

/// Create an empty maildir (cur/new/tmp).
pub fn create(dir: &Path) -> Result<()> {
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.join(sub))
            .with_context(|| format!("creating {}", dir.join(sub).display()))?;
    }
    Ok(())
}

/// Deliver a message into a maildir the spec way: write to tmp/, then
/// rename into cur/ with the given flags. Returns the delivered path.
pub fn deliver(dir: &Path, content: &[u8], flags: Flags) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    if !dir.join("cur").is_dir() || !dir.join("tmp").is_dir() {
        bail!("{} is not a maildir", dir.display());
    }
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let name = format!(
        "{epoch}.{}_{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        hostname(),
    );
    let tmp = dir.join("tmp").join(&name);
    fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    let target = dir.join("cur").join(format!("{name}{}", flags.to_info()));
    fs::rename(&tmp, &target).with_context(|| format!("renaming to {}", target.display()))?;
    Ok(target)
}

/// Find a nearby maildir whose name (ignoring a leading dot) matches one
/// of `names` case-insensitively — e.g. Sent/.Sent or Drafts.
pub fn find_special(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    discover(dir).into_iter().find(|p| {
        p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            let n = n.trim_start_matches('.');
            names.iter().any(|w| n.eq_ignore_ascii_case(w))
        })
    })
}

/// Maildirs to offer in the folder browser: subdirectories of `dir`
/// (Maildir++ style children) and of its parent (sibling mailboxes).
pub fn discover(dir: &Path) -> Vec<PathBuf> {
    fn push_children(parent: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = parent.read_dir() else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("cur").is_dir() && p.join("new").is_dir() {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    push_children(dir, &mut out);
    if let Some(parent) = dir.parent() {
        push_children(parent, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_maildir(root: &Path) {
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
    }

    #[test]
    fn flags_from_filename() {
        let f = Flags::from_filename("1234.abc.host:2,FS");
        assert!(f.seen && f.flagged);
        assert!(!f.deleted && !f.answered && !f.draft);
        assert_eq!(Flags::from_filename("1234.abc.host"), Flags::default());
    }

    #[test]
    fn flags_roundtrip_info() {
        let f = Flags {
            seen: true,
            flagged: true,
            deleted: true,
            ..Default::default()
        };
        assert_eq!(f.to_info(), ":2,FST");
        assert_eq!(Flags::from_filename(&format!("x{}", f.to_info())), f);
    }

    #[test]
    fn size_comes_from_name_when_present() {
        assert_eq!(size_from_name("12.rmut,S=345:2,S"), Some(345));
        assert_eq!(size_from_name("12.rmut,S=345"), Some(345));
        assert_eq!(size_from_name("1234.abc.host:2,S"), None);
        let tmp = tempfile::tempdir().unwrap();
        make_maildir(tmp.path());
        fs::write(tmp.path().join("cur/9.rmut,S=777:2,S"), "tiny").unwrap();
        let files = scan(tmp.path()).unwrap();
        assert_eq!(files[0].size, 777);
    }

    #[test]
    fn scan_rejects_non_maildir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan(tmp.path()).is_err());
    }

    #[test]
    fn scan_finds_new_and_cur() {
        let tmp = tempfile::tempdir().unwrap();
        make_maildir(tmp.path());
        fs::write(tmp.path().join("cur/1.host:2,S"), "Subject: a\n\nx\n").unwrap();
        fs::write(tmp.path().join("new/2.host"), "Subject: b\n\ny\n").unwrap();
        let mut files = scan(tmp.path()).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(files.len(), 2);
        assert!(!files[0].is_new && files[0].flags.seen);
        assert!(files[1].is_new);
        assert!(files[0].size > 0);
    }

    #[test]
    fn store_flags_moves_new_to_cur_with_info() {
        let tmp = tempfile::tempdir().unwrap();
        make_maildir(tmp.path());
        let src = tmp.path().join("new/99.host");
        fs::write(&src, "Subject: a\n\nx\n").unwrap();
        let file = MailFile {
            path: src.clone(),
            is_new: true,
            flags: Flags {
                seen: true,
                ..Default::default()
            },
            size: 14,
        };
        let new_path = store_flags(&file).unwrap();
        assert_eq!(new_path, tmp.path().join("cur/99.host:2,S"));
        assert!(!src.exists());
        assert!(new_path.exists());
    }

    #[test]
    fn store_flags_rewrites_existing_info() {
        let tmp = tempfile::tempdir().unwrap();
        make_maildir(tmp.path());
        let src = tmp.path().join("cur/7.host:2,S");
        fs::write(&src, "x").unwrap();
        let mut file = MailFile {
            path: src.clone(),
            is_new: false,
            flags: Flags::from_filename("7.host:2,S"),
            size: 1,
        };
        file.flags.flagged = true;
        let new_path = store_flags(&file).unwrap();
        assert_eq!(new_path, tmp.path().join("cur/7.host:2,FS"));
        assert!(!src.exists());
    }

    #[test]
    fn discover_finds_children_and_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let sent = tmp.path().join("sent");
        let sub = inbox.join(".archive");
        for d in [&inbox, &sent, &sub] {
            make_maildir(d);
        }
        fs::create_dir(tmp.path().join("not-a-maildir")).unwrap();
        let found = discover(&inbox);
        assert!(found.contains(&inbox));
        assert!(found.contains(&sent));
        assert!(found.contains(&sub));
        assert!(!found.iter().any(|p| p.ends_with("not-a-maildir")));
    }
}
