//! mbox support (system spools like /var/mail/$USER): the file is
//! mirrored into a cache maildir so the whole index/pager stack works
//! on it unchanged, and `$` sync writes the changes back — Status:/
//! X-Status: headers rewritten, purged messages dropped — with the
//! file flock()ed and rewritten in place (mboxrd `>From` quoting).
//! Messages are keyed by a content hash, so flags survive re-mirrors
//! when the spool grows.

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail, ensure};

use crate::maildir::{self, Flags};
use crate::remote::sanitize;

pub struct Mbox {
    pub path: PathBuf,
    pub cache: PathBuf,
    /// (mtime, len) of the file at mirror time; a mismatch at
    /// write-back time means someone else changed the spool.
    snapshot: (Option<SystemTime>, u64),
}

/// One message split out of the file.
struct Raw {
    /// The `From sender date` separator line, kept for the rewrite.
    from_line: String,
    /// Unescaped message bytes (headers + body).
    bytes: Vec<u8>,
}

impl Raw {
    fn id(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.bytes.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}

/// Where an mbox's cache maildir lives:
/// `$XDG_CACHE_HOME/rmut/mbox/<percent-encoded absolute path>`.
pub fn cache_dir(path: &Path) -> PathBuf {
    let abs = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    crate::remote::cache_base()
        .join("mbox")
        .join(sanitize(&abs))
}

/// Message id encoded in a cache filename (`<hash>.mbox[,S=n][:2,f]`).
pub fn id_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let base = name.split(":2,").next()?;
    let base = base.split(",S=").next()?;
    base.strip_suffix(".mbox").map(str::to_string)
}

/// True when the file looks like an mbox: empty, or starting with a
/// `From ` separator line.
pub fn looks_like_mbox(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    if meta.len() == 0 {
        return true;
    }
    let mut buf = [0u8; 5];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok_and(|()| &buf == b"From ")
}

fn stat(path: &Path) -> Result<(Option<SystemTime>, u64)> {
    let meta = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    Ok((meta.modified().ok(), meta.len()))
}

/// flock the file, briefly retrying (delivery holds locks for
/// moments, not minutes).
fn lock(file: &fs::File) -> Result<()> {
    use std::os::fd::AsRawFd;
    for _ in 0..25 {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    bail!("the mbox is locked by another program")
}

/// Split mbox bytes into messages: a `From ` line at the start of the
/// file or after a blank line separates; one level of `>From` quoting
/// is removed (mboxrd). The blank separator line stays out of the
/// message.
fn split(data: &[u8]) -> Vec<Raw> {
    let mut out: Vec<Raw> = Vec::new();
    let mut prev_blank = true;
    for line in data.split_inclusive(|&b| b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        if prev_blank && text.starts_with(b"From ") {
            // Drop the separator blank line collected into the
            // previous message.
            if let Some(last) = out.last_mut()
                && last.bytes.ends_with(b"\n\n")
            {
                last.bytes.pop();
            }
            out.push(Raw {
                from_line: String::from_utf8_lossy(text).into_owned(),
                bytes: Vec::new(),
            });
            prev_blank = false;
            continue;
        }
        prev_blank = text.is_empty();
        if let Some(msg) = out.last_mut() {
            // Unescape one quoting level: ">From " → "From ".
            let trimmed = trim_quoting(text);
            if trimmed.starts_with(b"From ") && text != trimmed {
                msg.bytes.extend_from_slice(&text[1..]);
            } else {
                msg.bytes.extend_from_slice(text);
            }
            msg.bytes.push(b'\n');
        }
    }
    out
}

fn trim_quoting(line: &[u8]) -> &[u8] {
    let mut rest = line;
    while let Some(r) = rest.strip_prefix(b">") {
        rest = r;
    }
    rest
}

/// Seen/old/flagged/answered from the Status: and X-Status: headers;
/// `old` is mbox's "not new" (Status: O).
fn status_flags(bytes: &[u8]) -> (Flags, bool) {
    let head_end = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .unwrap_or(bytes.len());
    let head = String::from_utf8_lossy(&bytes[..head_end]);
    let mut chars = String::new();
    for line in head.lines() {
        if let Some((key, value)) = line.split_once(':')
            && matches!(
                key.trim().to_ascii_lowercase().as_str(),
                "status" | "x-status"
            )
        {
            chars += value.trim();
        }
    }
    let flags = Flags {
        seen: chars.contains('R'),
        answered: chars.contains('A'),
        flagged: chars.contains('F'),
        deleted: false,
        draft: false,
    };
    (flags, chars.contains('O'))
}

/// The message with its Status:/X-Status: headers replaced to encode
/// `flags` and old-ness.
fn set_status(bytes: &[u8], flags: Flags, old: bool) -> Vec<u8> {
    let head_end = bytes
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|i| i + 1)
        .unwrap_or(bytes.len());
    let (head, body) = bytes.split_at(head_end);
    let mut out = Vec::with_capacity(bytes.len() + 32);
    for line in head.split_inclusive(|&b| b == b'\n') {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(b"status:") || lower.starts_with(b"x-status:") {
            continue;
        }
        out.extend_from_slice(line);
    }
    let mut status = String::new();
    if flags.seen {
        status.push('R');
    }
    if old || flags.seen {
        status.push('O');
    }
    if !status.is_empty() {
        out.extend_from_slice(format!("Status: {status}\n").as_bytes());
    }
    let mut xstatus = String::new();
    if flags.answered {
        xstatus.push('A');
    }
    if flags.flagged {
        xstatus.push('F');
    }
    if !xstatus.is_empty() {
        out.extend_from_slice(format!("X-Status: {xstatus}\n").as_bytes());
    }
    out.extend_from_slice(body);
    out
}

impl Mbox {
    /// Read the file and bring the cache maildir up to date: messages
    /// already mirrored keep whatever flags the cache has, new ones
    /// land in new/ or cur/ per their Status: header, ones gone from
    /// the file leave the cache.
    pub fn open(path: &Path) -> Result<Mbox> {
        Mbox::open_at(path, cache_dir(path))
    }

    fn open_at(path: &Path, cache: PathBuf) -> Result<Mbox> {
        ensure!(
            looks_like_mbox(path),
            "{} does not look like an mbox file",
            path.display()
        );
        let mut mbox = Mbox {
            path: path.to_path_buf(),
            cache,
            snapshot: (None, 0),
        };
        mbox.mirror()?;
        Ok(mbox)
    }

    fn mirror(&mut self) -> Result<()> {
        let file = fs::File::open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        // Best-effort shared lock against a delivery mid-read.
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        let mut data = Vec::new();
        (&file).read_to_end(&mut data)?;
        drop(file);
        maildir::create(&self.cache)?;
        let mut cached: HashMap<String, PathBuf> = HashMap::new();
        for f in maildir::scan(&self.cache)? {
            if let Some(id) = id_of(&f.path) {
                cached.insert(id, f.path.clone());
            }
        }
        for raw in split(&data) {
            let id = raw.id();
            if cached.remove(&id).is_some() {
                continue; // known: the cache's flags are newer
            }
            let (flags, old) = status_flags(&raw.bytes);
            // new/ carries no flag info: anything marked goes to cur/.
            let plain_new = !old && flags == Flags::default();
            let name = format!("{id}.mbox,S={}", raw.bytes.len());
            let target = if plain_new {
                self.cache.join("new").join(name)
            } else {
                self.cache.join("cur").join(name + &flags.to_info())
            };
            fs::write(&target, &raw.bytes)
                .with_context(|| format!("writing {}", target.display()))?;
        }
        for (_, gone) in cached {
            let _ = fs::remove_file(gone);
        }
        self.snapshot = stat(&self.path)?;
        Ok(())
    }

    /// Re-mirror when the file changed on disk (the new-mail poll).
    pub fn refresh(&mut self) -> Result<()> {
        if stat(&self.path)? != self.snapshot {
            self.mirror()?;
        }
        Ok(())
    }

    /// Rewrite the mbox to the wanted end state, keyed by message id:
    /// None drops the message (purge), Some((flags, is_new)) rewrites
    /// its Status:/X-Status:; ids not in the map keep their current
    /// state. Runs under an exclusive flock and refuses when the file
    /// changed since the last mirror (refresh first, then sync again).
    pub fn write_back(&mut self, state: &HashMap<String, Option<(Flags, bool)>>) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)
            .with_context(|| format!("opening {} for writing", self.path.display()))?;
        lock(&file)?;
        ensure!(
            stat(&self.path)? == self.snapshot,
            "{} changed on disk — check for new mail (G), then sync again",
            self.path.display()
        );
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        let mut out = Vec::with_capacity(data.len());
        for raw in split(&data) {
            let (flags, old) = match state.get(&raw.id()) {
                Some(None) => continue, // purged
                Some(Some((flags, is_new))) => (*flags, !*is_new),
                None => status_flags(&raw.bytes),
            };
            let bytes = set_status(&raw.bytes, flags, old);
            out.extend_from_slice(raw.from_line.as_bytes());
            out.push(b'\n');
            for line in bytes.split_inclusive(|&b| b == b'\n') {
                // mboxrd quoting: any From-ish line gains one '>'.
                if trim_quoting(line.strip_suffix(b"\n").unwrap_or(line)).starts_with(b"From ") {
                    out.push(b'>');
                }
                out.extend_from_slice(line);
            }
            if !out.ends_with(b"\n") {
                out.push(b'\n');
            }
            out.push(b'\n'); // the blank separator line
        }
        file.rewind()?;
        file.write_all(&out)?;
        file.set_len(out.len() as u64)?;
        file.sync_all()?;
        drop(file); // releases the flock
        self.snapshot = stat(&self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "From jane@example.com Thu Jul  9 10:00:00 2026\n\
                       From: jane@example.com\nSubject: first\n\
                       Status: RO\n\nhello\n>From me, quoted\n\n\
                       From petr@example.com Thu Jul  9 11:00:00 2026\n\
                       From: petr@example.com\nSubject: second\n\nfresh mail\n";

    #[test]
    fn split_finds_messages_and_unescapes() {
        let msgs = split(TWO.as_bytes());
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].from_line.starts_with("From jane@example.com"));
        let first = String::from_utf8_lossy(&msgs[0].bytes);
        assert!(first.contains("Subject: first"));
        assert!(first.contains("\nFrom me, quoted\n"), "{first}");
        assert!(!first.ends_with("\n\n"), "separator kept out: {first:?}");
        let second = String::from_utf8_lossy(&msgs[1].bytes);
        assert!(second.contains("fresh mail"));
        // A "From " mid-paragraph (no blank line before) is body text.
        let tricky = "From a@x Thu Jul  9 10:00:00 2026\n\nbody\nFrom here on\n";
        assert_eq!(split(tricky.as_bytes()).len(), 1);
        assert!(split(b"").is_empty());
    }

    #[test]
    fn status_flags_read_and_written() {
        let msgs = split(TWO.as_bytes());
        let (flags, old) = status_flags(&msgs[0].bytes);
        assert!(flags.seen && old && !flags.flagged);
        let (flags, old) = status_flags(&msgs[1].bytes);
        assert!(!flags.seen && !old);
        let updated = set_status(
            &msgs[1].bytes,
            Flags {
                seen: true,
                flagged: true,
                ..Default::default()
            },
            true,
        );
        let text = String::from_utf8_lossy(&updated);
        assert!(text.contains("Status: RO\n"), "{text}");
        assert!(text.contains("X-Status: F\n"), "{text}");
        assert!(text.ends_with("fresh mail\n"));
        // Replacing drops the old Status line instead of stacking.
        let cleared = set_status(&msgs[0].bytes, Flags::default(), false);
        let text = String::from_utf8_lossy(&cleared);
        assert!(!text.contains("Status:"), "{text}");
    }

    #[test]
    fn mirror_and_remirror_keep_cache_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        fs::write(&spool, TWO).unwrap();
        let mut mbox = Mbox::open_at(&spool, tmp.path().join("cache")).unwrap();
        let files = maildir::scan(&mbox.cache).unwrap();
        assert_eq!(files.len(), 2);
        // Read message in cur/, fresh one in new/.
        assert_eq!(files.iter().filter(|f| f.is_new).count(), 1);
        // Flag the read one in the cache, like the app would.
        let read = files.iter().find(|f| !f.is_new).unwrap();
        let mut flagged = read.clone();
        flagged.flags.flagged = true;
        let new_path = maildir::store_flags(&flagged).unwrap();
        // Append a third message to the spool; refresh mirrors it and
        // keeps the flag.
        let mut data = fs::read(&spool).unwrap();
        data.extend_from_slice(
            b"\nFrom ci@example.com Thu Jul  9 12:00:00 2026\nSubject: third\n\njob done\n",
        );
        fs::write(&spool, &data).unwrap();
        mbox.refresh().unwrap();
        let files = maildir::scan(&mbox.cache).unwrap();
        assert_eq!(files.len(), 3);
        let kept = files.iter().find(|f| f.path == new_path).unwrap();
        assert!(kept.flags.flagged, "cache flag lost on re-mirror");
    }

    #[test]
    fn write_back_purges_and_rewrites_status() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        fs::write(&spool, TWO).unwrap();
        let mut mbox = Mbox::open_at(&spool, tmp.path().join("cache")).unwrap();
        let msgs = split(TWO.as_bytes());
        let mut state = HashMap::new();
        state.insert(msgs[0].id(), None); // purge the first
        state.insert(
            msgs[1].id(),
            Some((
                Flags {
                    seen: true,
                    answered: true,
                    ..Default::default()
                },
                false,
            )),
        );
        mbox.write_back(&state).unwrap();
        let text = fs::read_to_string(&spool).unwrap();
        assert!(!text.contains("Subject: first"), "{text}");
        assert!(text.contains("Subject: second"));
        assert!(text.contains("Status: RO\n"));
        assert!(text.contains("X-Status: A\n"));
        assert!(text.starts_with("From petr@example.com"));
        // The rewritten file still parses to the one message.
        assert_eq!(split(text.as_bytes()).len(), 1);
        // Quoting roundtrip: a From-line in a body survives a rewrite.
        fs::write(&spool, TWO).unwrap();
        let mut mbox = Mbox::open_at(&spool, tmp.path().join("cache")).unwrap();
        mbox.write_back(&HashMap::new()).unwrap();
        let msgs = split(&fs::read(&spool).unwrap());
        assert_eq!(msgs.len(), 2);
        assert!(
            String::from_utf8_lossy(&msgs[0].bytes).contains("\nFrom me, quoted\n"),
            "quoting did not roundtrip"
        );
    }

    #[test]
    fn write_back_refuses_a_changed_spool() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        fs::write(&spool, TWO).unwrap();
        let mut mbox = Mbox::open_at(&spool, tmp.path().join("cache")).unwrap();
        let mut data = fs::read(&spool).unwrap();
        data.extend_from_slice(b"From x@y Thu Jul  9 13:00:00 2026\n\nsurprise\n");
        fs::write(&spool, &data).unwrap();
        let err = mbox.write_back(&HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("changed on disk"), "{err}");
        // The surprise message is still there.
        assert!(fs::read_to_string(&spool).unwrap().contains("surprise"));
    }

    #[test]
    fn looks_like_mbox_checks_the_first_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good");
        fs::write(&good, TWO).unwrap();
        assert!(looks_like_mbox(&good));
        let empty = tmp.path().join("empty");
        fs::write(&empty, "").unwrap();
        assert!(looks_like_mbox(&empty));
        let bad = tmp.path().join("bad");
        fs::write(&bad, "not a spool\n").unwrap();
        assert!(!looks_like_mbox(&bad));
        assert!(!looks_like_mbox(&tmp.path().join("missing")));
    }
}
