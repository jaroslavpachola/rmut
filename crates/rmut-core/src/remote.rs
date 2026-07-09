//! An IMAP folder mirrored into a local cache maildir, so the whole
//! index/pager stack works on it unchanged. Filenames carry the UID
//! and server size (`<uid>.rmut,S=<size>`); new messages start as
//! header-only files (marked with a leading X-Rmut-Partial header) and
//! get their full body on first view. Local flag changes and deletes
//! are pushed with UID STORE / EXPUNGE on sync.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::config::Account;
use crate::imap::{Client, Fetched};
use crate::maildir::{self, Flags, MailFile};

const PARTIAL_MARKER: &[u8] = b"X-Rmut-Partial: 1\r\n";

pub struct Remote {
    /// `imap:account/mailbox`, for the status line and folder browser.
    pub spec: String,
    pub account: Account,
    pub mailbox: String,
    pub cache: PathBuf,
    client: Client,
}

/// `imap:account[/mailbox]` → (account, mailbox); INBOX when omitted.
pub fn parse_spec(spec: &str) -> Option<(&str, &str)> {
    let rest = spec.strip_prefix("imap:")?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once('/') {
        Some((account, mailbox)) if !account.is_empty() && !mailbox.is_empty() => {
            Some((account, mailbox))
        }
        Some(_) => None,
        None => Some((rest, "INBOX")),
    }
}

/// Where a folder's cache maildir lives:
/// `$XDG_CACHE_HOME/rmut/imap/<account>/<mailbox>` (percent-encoded).
pub fn cache_dir(account: &str, mailbox: &str) -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("rmut/imap")
        .join(sanitize(account))
        .join(sanitize(mailbox))
}

/// Filesystem-safe single path component.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.chars().all(|c| c == '.') {
        out = format!("%{out}");
    }
    out
}

/// UID encoded in a cache filename (`<uid>.rmut[,S=n][:2,flags]`).
pub fn uid_of(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let base = name.split(":2,").next()?;
    let base = base.split(",S=").next()?;
    base.strip_suffix(".rmut")?.parse().ok()
}

/// True for cached files that hold only the message headers so far.
pub fn is_partial(path: &Path) -> bool {
    let mut buf = [0u8; PARTIAL_MARKER.len()];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok_and(|()| buf == PARTIAL_MARKER)
}

impl Remote {
    /// Connect, log in, select, and bring the cache maildir up to date.
    pub fn open(account: &Account, mailbox: &str, password: &str) -> Result<Remote> {
        let host = account
            .imap_host
            .as_deref()
            .with_context(|| format!("account {} has no imap_host", account.name))?;
        let mut client = Client::connect(host, account.imap_port, account.imap_tls)?;
        client.login(&account.user, password)?;
        let select = client.select(mailbox)?;
        let cache = cache_dir(&account.name, mailbox);
        maildir::create(&cache)?;
        let uv_file = cache.join(".uidvalidity");
        let cached_uv: u32 = fs::read_to_string(&uv_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if cached_uv != select.uidvalidity {
            // UIDs are meaningless across a validity change: start over.
            for file in maildir::scan(&cache)? {
                let _ = fs::remove_file(&file.path);
            }
            fs::write(&uv_file, format!("{}\n", select.uidvalidity))?;
        }
        let mut remote = Remote {
            spec: format!("imap:{}/{mailbox}", account.name),
            account: account.clone(),
            mailbox: mailbox.to_string(),
            cache,
            client,
        };
        remote.refresh()?;
        Ok(remote)
    }

    /// Reconcile the cache with the server: pull flag changes, drop
    /// expunged messages, download headers of new ones. Returns how
    /// many new messages arrived.
    pub fn refresh(&mut self) -> Result<usize> {
        let mut by_uid: HashMap<u32, MailFile> = HashMap::new();
        for file in maildir::scan(&self.cache)? {
            if let Some(uid) = uid_of(&file.path) {
                by_uid.insert(uid, file);
            }
        }
        let metas = self.client.uid_fetch_flags("1:*")?;
        let mut new_uids: Vec<u32> = Vec::new();
        let mut on_server: HashSet<u32> = HashSet::new();
        for meta in &metas {
            on_server.insert(meta.uid);
            match by_uid.get(&meta.uid) {
                Some(file) if file.flags != meta.flags => {
                    // Server-side change wins in the cache; unsynced
                    // local edits to this message are dropped on rescan.
                    let mut updated = file.clone();
                    updated.flags = meta.flags;
                    let _ = maildir::store_flags(&updated);
                }
                Some(_) => {}
                None => new_uids.push(meta.uid),
            }
        }
        for (uid, file) in &by_uid {
            if !on_server.contains(uid) {
                let _ = fs::remove_file(&file.path);
            }
        }
        for chunk in new_uids.chunks(100) {
            let set = chunk
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            for fetched in self.client.uid_fetch_headers(&set)? {
                self.write_partial(&fetched)?;
            }
        }
        Ok(new_uids.len())
    }

    /// Header-only cache file for a message we haven't viewed yet.
    fn write_partial(&self, fetched: &Fetched) -> Result<()> {
        let header = fetched.body.as_deref().unwrap_or_default();
        let mut content = Vec::with_capacity(PARTIAL_MARKER.len() + header.len());
        content.extend_from_slice(PARTIAL_MARKER);
        content.extend_from_slice(header);
        let sub = if fetched.flags.seen { "cur" } else { "new" };
        let name = format!(
            "{}.rmut,S={}{}",
            fetched.uid,
            fetched.size,
            fetched.flags.to_info()
        );
        let path = self.cache.join(sub).join(name);
        fs::write(&path, &content).with_context(|| format!("writing {}", path.display()))
    }

    /// Replace a header-only cache file with the full message.
    pub fn fetch_body(&mut self, path: &Path) -> Result<()> {
        let uid = uid_of(path).context("not a cached IMAP message")?;
        let body = self.client.uid_fetch_full(uid)?;
        fs::write(path, &body).with_context(|| format!("writing {}", path.display()))
    }

    /// Push a local flag change (from `$` sync) to the server.
    pub fn push_flags(&mut self, path: &Path, flags: Flags) -> Result<()> {
        let uid = uid_of(path).context("not a cached IMAP message")?;
        self.client.uid_store_flags(uid, flags)
    }

    /// Mark the given cached messages \Deleted and expunge them.
    pub fn delete(&mut self, paths: &[PathBuf]) -> Result<()> {
        let uids: Vec<String> = paths
            .iter()
            .filter_map(|p| uid_of(p))
            .map(|u| u.to_string())
            .collect();
        ensure!(uids.len() == paths.len(), "unrecognized cache filename");
        self.client.uid_delete(&uids.join(","))?;
        self.client.expunge()
    }

    /// Selectable folders, for the folder browser.
    pub fn folders(&mut self) -> Result<Vec<String>> {
        Ok(self
            .client
            .list()?
            .into_iter()
            .filter(|f| !f.no_select)
            .map(|f| f.name)
            .collect())
    }

    /// Fcc: file the sent message into the account's Sent folder.
    /// Returns the folder name for the status line.
    pub fn append_sent(&mut self, body: &[u8]) -> Result<String> {
        let folder = self.account.sent_folder.clone();
        let flags = Flags {
            seen: true,
            ..Default::default()
        };
        self.client.append(&folder, flags, body)?;
        Ok(folder)
    }

    /// Poll the server; refresh the cache when it reported changes.
    pub fn check_new(&mut self) -> Result<usize> {
        if self.client.noop()? {
            self.refresh()
        } else {
            Ok(0)
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.client.logout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{self, Expect};

    fn account(port: u16) -> Account {
        Account {
            name: "test".into(),
            user: "jane".into(),
            password_command: None,
            password: None,
            imap_host: Some("127.0.0.1".into()),
            imap_port: port,
            imap_tls: false,
            smtp_host: None,
            smtp_port: 587,
            smtp_tls: true,
            sent_folder: "Sent".into(),
        }
    }

    fn fetch_reply(uid: u32, flags: &str, header: &str) -> String {
        format!(
            "* {uid} FETCH (UID {uid} FLAGS ({flags}) RFC822.SIZE {} BODY[HEADER] {{{}}}\r\n{})\r\n",
            header.len() + 100,
            header.len(),
            header
        )
    }

    fn open_script() -> Vec<Expect> {
        vec![
            Expect::new("LOGIN", String::new()),
            Expect::new(
                "SELECT \"INBOX\"",
                "* 2 EXISTS\r\n* OK [UIDVALIDITY 42] ok\r\n".into(),
            ),
            Expect::new(
                "UID FETCH 1:* (UID FLAGS)",
                "* 1 FETCH (UID 10 FLAGS (\\Seen))\r\n* 2 FETCH (UID 11 FLAGS ())\r\n".into(),
            ),
            Expect::new(
                "UID FETCH 10,11 (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
                fetch_reply(10, "\\Seen", "Subject: first\r\n\r\n")
                    + &fetch_reply(11, "", "Subject: second\r\n\r\n"),
            ),
        ]
    }

    fn with_cache_home<T>(f: impl FnOnce() -> T) -> (T, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        // Serialized by rust's test lock? No — tests run in parallel, so
        // env vars are unsafe to share. Give each test its own subdir
        // through a process-wide lock held for the whole closure.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        unsafe { std::env::set_var("XDG_CACHE_HOME", tmp.path()) };
        let out = f();
        unsafe { std::env::remove_var("XDG_CACHE_HOME") };
        (out, tmp)
    }

    #[test]
    fn spec_parsing() {
        assert_eq!(parse_spec("imap:work"), Some(("work", "INBOX")));
        assert_eq!(
            parse_spec("imap:work/Archive/2026"),
            Some(("work", "Archive/2026"))
        );
        assert_eq!(parse_spec("imap:"), None);
        assert_eq!(parse_spec("imap:work/"), None);
        assert_eq!(parse_spec("~/Maildir"), None);
    }

    #[test]
    fn sanitize_is_fs_safe() {
        assert_eq!(sanitize("INBOX"), "INBOX");
        assert_eq!(sanitize("Archive/2026"), "Archive%2F2026");
        assert_eq!(sanitize(".."), "%..");
        assert_eq!(sanitize("a b"), "a%20b");
    }

    #[test]
    fn uid_from_cache_names() {
        assert_eq!(uid_of(Path::new("/c/cur/15.rmut,S=200:2,S")), Some(15));
        assert_eq!(uid_of(Path::new("/c/new/7.rmut,S=1")), Some(7));
        assert_eq!(uid_of(Path::new("/c/cur/1234.host:2,S")), None);
    }

    #[test]
    fn open_mirrors_headers_into_cache() {
        let (port, handle) = testserver::imap(open_script());
        let ((), _tmp) = with_cache_home(|| {
            let remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let files = maildir::scan(&remote.cache).unwrap();
            assert_eq!(files.len(), 2);
            let seen = files.iter().find(|f| uid_of(&f.path) == Some(10)).unwrap();
            assert!(seen.flags.seen && !seen.is_new);
            assert_eq!(seen.size, "Subject: first\r\n\r\n".len() as u64 + 100);
            let unseen = files.iter().find(|f| uid_of(&f.path) == Some(11)).unwrap();
            assert!(unseen.is_new && !unseen.flags.seen);
            assert!(is_partial(&seen.path) && is_partial(&unseen.path));
            // The partial file still parses as a message.
            let env = crate::message::envelope(seen.clone()).unwrap();
            assert_eq!(env.subject, "first");
        });
        handle.join().unwrap();
    }

    #[test]
    fn refresh_applies_server_changes() {
        let mut script = open_script();
        script.push(Expect::new(
            "UID FETCH 1:* (UID FLAGS)",
            // 10 gone, 11 now seen+flagged, 12 is new.
            "* 1 FETCH (UID 11 FLAGS (\\Seen \\Flagged))\r\n* 2 FETCH (UID 12 FLAGS ())\r\n".into(),
        ));
        script.push(Expect::new(
            "UID FETCH 12 (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
            fetch_reply(12, "", "Subject: third\r\n\r\n"),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let arrived = remote.refresh().unwrap();
            assert_eq!(arrived, 1);
            let files = maildir::scan(&remote.cache).unwrap();
            let uids: Vec<_> = files.iter().filter_map(|f| uid_of(&f.path)).collect();
            assert!(!uids.contains(&10), "expunged message still cached");
            assert!(uids.contains(&12), "new message not cached");
            let updated = files.iter().find(|f| uid_of(&f.path) == Some(11)).unwrap();
            assert!(updated.flags.seen && updated.flags.flagged);
            assert!(!updated.is_new, "flag update should move it to cur/");
        });
        handle.join().unwrap();
    }

    #[test]
    fn fetch_body_completes_a_partial_file() {
        let full = "Subject: first\r\n\r\nthe actual body\r\n";
        let mut script = open_script();
        script.push(Expect::new(
            "UID FETCH 10 (UID BODY.PEEK[])",
            format!(
                "* 1 FETCH (UID 10 BODY[] {{{}}}\r\n{})\r\n",
                full.len(),
                full
            ),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let files = maildir::scan(&remote.cache).unwrap();
            let file = files.iter().find(|f| uid_of(&f.path) == Some(10)).unwrap();
            remote.fetch_body(&file.path).unwrap();
            assert!(!is_partial(&file.path));
            assert_eq!(fs::read_to_string(&file.path).unwrap(), full);
        });
        handle.join().unwrap();
    }

    #[test]
    fn push_and_delete_send_uid_commands() {
        let mut script = open_script();
        script.push(Expect::new(
            "UID STORE 11 FLAGS.SILENT (\\Seen \\Flagged)",
            String::new(),
        ));
        script.push(Expect::new(
            "UID STORE 10 +FLAGS.SILENT (\\Deleted)",
            String::new(),
        ));
        script.push(Expect::new("EXPUNGE", String::new()));
        script.push(Expect::new("APPEND \"Sent\" (\\Seen)", String::new()));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let flags = Flags {
                seen: true,
                flagged: true,
                ..Default::default()
            };
            remote
                .push_flags(Path::new("/c/cur/11.rmut,S=1:2,"), flags)
                .unwrap();
            remote
                .delete(&[PathBuf::from("/c/cur/10.rmut,S=1:2,ST")])
                .unwrap();
            assert_eq!(
                remote.append_sent(b"From: a@b\r\n\r\nx\r\n").unwrap(),
                "Sent"
            );
        });
        handle.join().unwrap();
    }

    #[test]
    fn uidvalidity_change_clears_cache() {
        let mut script = open_script();
        // Second connection: different UIDVALIDITY, one message.
        script.push(Expect::new("LOGIN", String::new()));
        script.push(Expect::new(
            "SELECT \"INBOX\"",
            "* 1 EXISTS\r\n* OK [UIDVALIDITY 43] changed\r\n".into(),
        ));
        script.push(Expect::new(
            "UID FETCH 1:* (UID FLAGS)",
            "* 1 FETCH (UID 1 FLAGS ())\r\n".into(),
        ));
        script.push(Expect::new(
            "UID FETCH 1 (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
            fetch_reply(1, "", "Subject: fresh\r\n\r\n"),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let acct = account(port);
            let cache = {
                let first = Remote::open(&acct, "INBOX", "pw").unwrap();
                first.cache.clone()
            };
            let _second = Remote::open(&acct, "INBOX", "pw").unwrap();
            let uids: Vec<_> = maildir::scan(&cache)
                .unwrap()
                .iter()
                .filter_map(|f| uid_of(&f.path))
                .collect();
            assert_eq!(uids, vec![1]);
        });
        handle.join().unwrap();
    }
}
