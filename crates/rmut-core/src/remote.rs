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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, ensure};

use crate::config::{Account, AuthKind};
use crate::imap::{Changes, Client, Fetched};
use crate::maildir::{self, Flags, MailFile};
use crate::net;

const PARTIAL_MARKER: &[u8] = b"X-Rmut-Partial: 1\r\n";

pub struct Remote {
    /// `imap:account/mailbox`, for the status line and folder browser.
    pub spec: String,
    pub account: Account,
    pub mailbox: String,
    pub cache: PathBuf,
    client: Client,
    /// Credential kept for transparent reconnects.
    secret: String,
    uidvalidity: u32,
    /// Highest UID mirrored so far; arrivals are fetched from here.
    last_uid: u32,
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

/// Tidy a mailbox name: collapse `//` runs and trim `/` from the ends
/// (servers reject "adjacent hierarchy separators"); empty → INBOX.
/// A mutt-style imap[s]:// URL (from an unconverted config) means the
/// mailbox in its path.
pub fn clean_mailbox(name: &str) -> String {
    let name = match name
        .strip_prefix("imap://")
        .or_else(|| name.strip_prefix("imaps://"))
    {
        Some(rest) => rest.split_once('/').map_or("", |(_, path)| path),
        None => name,
    };
    let cleaned = name
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if cleaned.is_empty() {
        "INBOX".into()
    } else {
        cleaned
    }
}

/// `$XDG_CACHE_HOME/rmut` (or `~/.cache/rmut`), shared by the IMAP
/// and mbox mirrors.
pub(crate) fn cache_base() -> PathBuf {
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
    base.join("rmut")
}

/// Where a folder's cache maildir lives:
/// `$XDG_CACHE_HOME/rmut/imap/<account>/<mailbox>` (percent-encoded).
pub fn cache_dir(account: &str, mailbox: &str) -> PathBuf {
    cache_base()
        .join("imap")
        .join(sanitize(account))
        .join(sanitize(mailbox))
}

/// Filesystem-safe single path component.
pub(crate) fn sanitize(name: &str) -> String {
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

/// Log the client in the way the account's `auth` asks for: LOGIN
/// with the password, or SASL AUTHENTICATE with an OAuth token.
fn login(client: &mut Client, account: &Account, secret: &str) -> Result<()> {
    match account.auth_kind()? {
        AuthKind::Password => client.login(&account.user, secret),
        kind => {
            let host = account.imap_host.as_deref().unwrap_or_default();
            let initial = kind.initial_response(&account.user, secret, host, account.imap_port);
            client.authenticate(kind.sasl_name(), &crate::smtp::b64(initial.as_bytes()))
        }
    }
}

/// A fresh, logged-in session.
fn connect_client(account: &Account, secret: &str) -> Result<Client> {
    let host = account
        .imap_host
        .as_deref()
        .with_context(|| format!("account {} has no imap_host", account.name))?;
    let mut client = Client::connect(host, account.imap_port, account.imap_tls)?;
    login(&mut client, account, secret)?;
    Ok(client)
}

impl Remote {
    /// Connect, log in, select, and bring the cache maildir up to date.
    pub fn open(account: &Account, mailbox: &str, password: &str) -> Result<Remote> {
        let client = connect_client(account, password)?;
        let mut remote = Remote {
            spec: String::new(),
            account: account.clone(),
            mailbox: String::new(),
            cache: PathBuf::new(),
            client,
            secret: password.to_string(),
            uidvalidity: 0,
            last_uid: 0,
        };
        remote.point_at(mailbox)?;
        Ok(remote)
    }

    /// Reuse this session for another folder of the same account — a
    /// SELECT on the live connection instead of a fresh connect+login
    /// round. On failure the caller falls back to a full open.
    pub fn switch(&mut self, mailbox: &str) -> Result<()> {
        self.point_at(mailbox)
    }

    /// Point the session at `mailbox`: SELECT, cache setup with the
    /// UIDVALIDITY check, and the initial reconcile.
    fn point_at(&mut self, mailbox: &str) -> Result<()> {
        let mailbox = clean_mailbox(mailbox);
        let select = self.client.select(&mailbox)?;
        let cache = cache_dir(&self.account.name, &mailbox);
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
        self.spec = format!("imap:{}/{mailbox}", self.account.name);
        self.mailbox = mailbox;
        self.cache = cache;
        self.uidvalidity = select.uidvalidity;
        self.last_uid = 0;
        self.refresh()?;
        Ok(())
    }

    /// One transparent reconnect after a dropped connection: fresh
    /// session, same mailbox. A changed UIDVALIDITY means the cache is
    /// stale — that needs a real reopen, not a silent retry.
    fn reconnect(&mut self) -> Result<()> {
        let mut client = connect_client(&self.account, &self.secret)?;
        let select = client.select(&self.mailbox)?;
        ensure!(
            select.uidvalidity == self.uidvalidity,
            "UIDVALIDITY changed — reopen the mailbox"
        );
        self.client = client;
        Ok(())
    }

    /// Run an IMAP operation, reconnecting and retrying once when the
    /// connection died under us (laptop sleep, server timeout). Every
    /// operation this wraps is idempotent.
    fn retry<T>(&mut self, mut op: impl FnMut(&mut Client, &Path) -> Result<T>) -> Result<T> {
        match op(&mut self.client, &self.cache) {
            Err(err) if net::is_connection_error(&err) => {
                self.reconnect()
                    .with_context(|| format!("reconnect after: {err:#}"))?;
                op(&mut self.client, &self.cache)
            }
            other => other,
        }
    }

    /// Reconcile the cache with the server: pull flag changes, drop
    /// expunged messages, download headers of new ones. Returns how
    /// many new messages arrived.
    pub fn refresh(&mut self) -> Result<usize> {
        let (arrived, max_uid) = self.retry(Self::reconcile)?;
        self.last_uid = max_uid;
        Ok(arrived)
    }

    fn reconcile(client: &mut Client, cache: &Path) -> Result<(usize, u32)> {
        let mut by_uid: HashMap<u32, MailFile> = HashMap::new();
        for file in maildir::scan(cache)? {
            if let Some(uid) = uid_of(&file.path) {
                by_uid.insert(uid, file);
            }
        }
        let metas = client.uid_fetch_flags("1:*")?;
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
            for fetched in client.uid_fetch_headers(&set)? {
                write_partial(cache, &fetched)?;
            }
        }
        let max_uid = metas.iter().map(|m| m.uid).max().unwrap_or(0);
        Ok((new_uids.len(), max_uid))
    }

    /// Mirror only arrivals: everything above the last mirrored UID.
    fn fetch_new(&mut self) -> Result<usize> {
        let last = self.last_uid;
        let (arrived, max_uid) = self.retry(|client, cache| {
            let mut arrived = 0usize;
            let mut max_uid = last;
            for fetched in client.uid_fetch_headers(&format!("{}:*", last + 1))? {
                // "N:*" always returns at least the last message,
                // even when nothing is newer.
                if fetched.uid > last {
                    write_partial(cache, &fetched)?;
                    arrived += 1;
                    max_uid = max_uid.max(fetched.uid);
                }
            }
            Ok((arrived, max_uid))
        })?;
        self.last_uid = max_uid;
        Ok(arrived)
    }

    /// Replace a header-only cache file with the full message.
    pub fn fetch_body(&mut self, path: &Path) -> Result<()> {
        let uid = uid_of(path).context("not a cached IMAP message")?;
        let body = self.retry(|client, _| client.uid_fetch_full(uid))?;
        fs::write(path, &body).with_context(|| format!("writing {}", path.display()))
    }

    /// Push a local flag change (from `$` sync) to the server.
    pub fn push_flags(&mut self, path: &Path, flags: Flags) -> Result<()> {
        let uid = uid_of(path).context("not a cached IMAP message")?;
        self.retry(|client, _| client.uid_store_flags(uid, flags))
    }

    /// UID COPY the cached messages into another folder of this
    /// account (the $trash step before a purge).
    pub fn copy_to_folder(&mut self, paths: &[PathBuf], mailbox: &str) -> Result<()> {
        let uids: Vec<String> = paths
            .iter()
            .filter_map(|p| uid_of(p))
            .map(|u| u.to_string())
            .collect();
        ensure!(uids.len() == paths.len(), "unrecognized cache filename");
        let folder = clean_mailbox(mailbox);
        let set = uids.join(",");
        self.retry(|client, _| client.uid_copy(&set, &folder))
    }

    /// Mark the given cached messages \Deleted and expunge them.
    pub fn delete(&mut self, paths: &[PathBuf]) -> Result<()> {
        let uids: Vec<String> = paths
            .iter()
            .filter_map(|p| uid_of(p))
            .map(|u| u.to_string())
            .collect();
        ensure!(uids.len() == paths.len(), "unrecognized cache filename");
        let set = uids.join(",");
        self.retry(|client, _| {
            client.uid_delete(&set)?;
            client.expunge()
        })
    }

    /// Selectable folders with their UNSEEN counts, for the folder
    /// browser. The open folder's count comes from the local cache
    /// (STATUS must not target the selected mailbox); a failing STATUS
    /// just shows as 0.
    pub fn folders(&mut self) -> Result<Vec<(String, usize)>> {
        let open = self.mailbox.clone();
        self.retry(move |client, cache| {
            let names: Vec<String> = client
                .list()?
                .into_iter()
                .filter(|f| !f.no_select)
                .map(|f| f.name)
                .collect();
            let mut out = Vec::with_capacity(names.len());
            for name in names {
                let unseen = if name == open {
                    maildir::new_count(cache)
                } else {
                    client.status_unseen(&name).unwrap_or(0) as usize
                };
                out.push((name, unseen));
            }
            Ok(out)
        })
    }

    /// Unseen count of one folder of this account, for the sidebar:
    /// the open folder from its cache, others via STATUS (0 on error).
    pub fn unseen(&mut self, mailbox: &str) -> usize {
        let folder = clean_mailbox(mailbox);
        if folder == self.mailbox {
            maildir::new_count(&self.cache)
        } else {
            self.retry(|client, _| client.status_unseen(&folder))
                .unwrap_or(0) as usize
        }
    }

    /// Fcc: file the sent message into the account's Sent folder.
    /// Returns the folder name for the status line.
    /// APPEND a message into another folder of this account (`s` save).
    pub fn append_to(&mut self, mailbox: &str, flags: Flags, body: &[u8]) -> Result<String> {
        let folder = clean_mailbox(mailbox);
        self.retry(|client, _| client.append(&folder, flags, body))?;
        Ok(folder)
    }

    pub fn append_sent(&mut self, body: &[u8]) -> Result<String> {
        let folder = clean_mailbox(&self.account.sent_folder);
        let flags = Flags {
            seen: true,
            ..Default::default()
        };
        self.retry(|client, _| client.append(&folder, flags, body))?;
        Ok(folder)
    }

    /// Poll the server. Arrivals alone are fetched incrementally from
    /// the last known UID; anything else triggers a full reconcile.
    pub fn check_new(&mut self) -> Result<usize> {
        match self.retry(|client, _| client.noop_changes())? {
            Changes::None => Ok(0),
            Changes::NewOnly => self.fetch_new(),
            Changes::Full => self.refresh(),
        }
    }
}

/// Header-only cache file for a message we haven't viewed yet.
fn write_partial(cache: &Path, fetched: &Fetched) -> Result<()> {
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
    let path = cache.join(sub).join(name);
    fs::write(&path, &content).with_context(|| format!("writing {}", path.display()))
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.client.logout();
    }
}

/// Handle to a background IDLE watcher. Dropping it sets the stop
/// flag; the thread notices within the socket's 60 s read timeout and
/// logs out.
pub struct IdleWatch {
    changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl IdleWatch {
    /// True once since the server last announced changes.
    pub fn take_changed(&self) -> bool {
        self.changed.swap(false, Ordering::Relaxed)
    }
}

impl Drop for IdleWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Watch `mailbox` with IDLE on a dedicated connection, setting the
/// handle's flag whenever the server announces changes. Best-effort:
/// when the server lacks IDLE (or anything fails) the thread just
/// ends and the caller's NOOP polling carries on as before.
pub fn idle_watch(account: &Account, mailbox: &str, password: &str) -> IdleWatch {
    let changed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let (account, mailbox, password) = (account.clone(), mailbox.to_string(), password.to_string());
    let (thread_changed, thread_stop) = (Arc::clone(&changed), Arc::clone(&stop));
    std::thread::spawn(move || {
        // Respawn dropped sessions (laptop sleep, server timeout) with
        // a pause between attempts; only "no IDLE support" gives up.
        while !thread_stop.load(Ordering::Relaxed) {
            if !idle_session(&account, &mailbox, &password, &thread_stop, &thread_changed) {
                return;
            }
            for _ in 0..60 {
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    });
    IdleWatch { changed, stop }
}

/// One IDLE session, ending when the connection dies or `stop` is
/// set. True = worth reconnecting later; false = give up for good.
fn idle_session(
    account: &Account,
    mailbox: &str,
    secret: &str,
    stop: &AtomicBool,
    changed: &AtomicBool,
) -> bool {
    let Ok(mut client) = connect_client(account, secret) else {
        return true; // maybe offline right now
    };
    match client.supports_idle() {
        Ok(true) => {}
        Ok(false) => return false,
        Err(_) => return true,
    }
    if client.select(mailbox).is_err() {
        return true;
    }
    while !stop.load(Ordering::Relaxed) {
        match client.idle(stop) {
            Ok(true) => changed.store(true, Ordering::Relaxed),
            Ok(false) => {}
            Err(_) => return true,
        }
    }
    client.logout();
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{self, Expect};

    #[test]
    fn clean_mailbox_fixes_separator_trouble() {
        assert_eq!(clean_mailbox("INBOX"), "INBOX");
        assert_eq!(clean_mailbox("Work/Reports"), "Work/Reports");
        assert_eq!(clean_mailbox("Work//Reports"), "Work/Reports");
        assert_eq!(clean_mailbox("/INBOX/"), "INBOX");
        assert_eq!(clean_mailbox("INBOX/"), "INBOX");
        assert_eq!(clean_mailbox("//"), "INBOX");
        assert_eq!(clean_mailbox(""), "INBOX");
        // Stale mutt-style URLs name the mailbox in their path.
        assert_eq!(
            clean_mailbox("imap://jane@mail.example.com:/INBOX"),
            "INBOX"
        );
        assert_eq!(clean_mailbox("imaps://host/Work/Reports/"), "Work/Reports");
        assert_eq!(clean_mailbox("imaps://host"), "INBOX");
    }

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
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
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
    fn switch_selects_on_the_same_connection() {
        // One scripted connection: a second connect+login would hang
        // the test server, so passing proves the session is reused.
        let mut script = open_script();
        script.push(Expect::new(
            "SELECT \"Archive\"",
            "* 1 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n".into(),
        ));
        script.push(Expect::new(
            "UID FETCH 1:* (UID FLAGS)",
            "* 1 FETCH (UID 3 FLAGS (\\Seen))\r\n".into(),
        ));
        script.push(Expect::new(
            "UID FETCH 3 (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
            fetch_reply(3, "\\Seen", "Subject: archived\r\n\r\n"),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            remote.switch("Archive").unwrap();
            assert_eq!(remote.spec, "imap:test/Archive");
            assert_eq!(remote.uidvalidity, 7);
            let files = maildir::scan(&remote.cache).unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(uid_of(&files[0].path), Some(3));
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
    fn reconnects_and_retries_after_a_dropped_connection() {
        let mut script = open_script();
        script.push(Expect::drop_conn("NOOP"));
        // The fresh connection: greeting, LOGIN, SELECT, retried NOOP.
        script.push(Expect::new("LOGIN", String::new()));
        script.push(Expect::new(
            "SELECT \"INBOX\"",
            "* 2 EXISTS\r\n* OK [UIDVALIDITY 42] ok\r\n".into(),
        ));
        script.push(Expect::new("NOOP", String::new()));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            assert_eq!(remote.check_new().unwrap(), 0);
        });
        handle.join().unwrap();
    }

    #[test]
    fn reconnect_refuses_a_changed_uidvalidity() {
        let mut script = open_script();
        script.push(Expect::drop_conn("NOOP"));
        script.push(Expect::new("LOGIN", String::new()));
        script.push(Expect::new(
            "SELECT \"INBOX\"",
            "* 2 EXISTS\r\n* OK [UIDVALIDITY 43] changed\r\n".into(),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let err = remote.check_new().unwrap_err();
            assert!(
                format!("{err:#}").contains("UIDVALIDITY changed"),
                "{err:#}"
            );
        });
        handle.join().unwrap();
    }

    #[test]
    fn arrivals_fetch_incrementally() {
        let mut script = open_script(); // mirrors UIDs 10 and 11
        script.push(Expect::new("NOOP", "* 3 EXISTS\r\n".into()));
        script.push(Expect::new(
            "UID FETCH 12:* (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
            fetch_reply(12, "", "Subject: third\r\n\r\n"),
        ));
        // Nothing newer: the N:* quirk returns the last message,
        // which must not be mirrored twice.
        script.push(Expect::new("NOOP", "* 3 EXISTS\r\n".into()));
        script.push(Expect::new(
            "UID FETCH 13:* (UID FLAGS RFC822.SIZE BODY.PEEK[HEADER])",
            fetch_reply(12, "", "Subject: third\r\n\r\n"),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            assert_eq!(remote.check_new().unwrap(), 1);
            assert_eq!(remote.check_new().unwrap(), 0);
            let files = maildir::scan(&remote.cache).unwrap();
            assert_eq!(files.len(), 3);
        });
        handle.join().unwrap();
    }

    #[test]
    fn open_authenticates_with_oauth() {
        let mut script = vec![
            Expect::untagged("AUTHENTICATE XOAUTH2", "+ \r\n".into()),
            // XOAUTH2 for user=jane token=tok, precomputed base64.
            Expect::new("dXNlcj1qYW5lAWF1dGg9QmVhcmVyIHRvawEB", String::new()),
        ];
        script.extend(open_script().into_iter().skip(1)); // no LOGIN
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let acct = Account {
                auth: Some("xoauth2".into()),
                ..account(port)
            };
            let remote = Remote::open(&acct, "INBOX", "tok").unwrap();
            assert_eq!(maildir::scan(&remote.cache).unwrap().len(), 2);
        });
        handle.join().unwrap();
    }

    #[test]
    fn folders_carry_unseen_counts() {
        let mut script = open_script();
        script.push(Expect::new(
            "LIST \"\" \"*\"",
            "* LIST () \"/\" \"INBOX\"\r\n* LIST () \"/\" \"Archive\"\r\n".into(),
        ));
        script.push(Expect::new(
            "STATUS \"Archive\" (UNSEEN)",
            "* STATUS \"Archive\" (UNSEEN 5)\r\n".into(),
        ));
        let (port, handle) = testserver::imap(script);
        let ((), _tmp) = with_cache_home(|| {
            let mut remote = Remote::open(&account(port), "INBOX", "pw").unwrap();
            let folders = remote.folders().unwrap();
            // INBOX (selected) counts its cache maildir: UID 11 is new.
            assert_eq!(folders, vec![("INBOX".into(), 1), ("Archive".into(), 5)]);
        });
        handle.join().unwrap();
    }

    #[test]
    fn idle_watch_flags_server_changes() {
        let script = vec![
            Expect::new("LOGIN", String::new()),
            Expect::new("CAPABILITY", "* CAPABILITY IMAP4rev1 IDLE\r\n".into()),
            Expect::new("SELECT \"INBOX\"", "* 1 EXISTS\r\n".into()),
            Expect::untagged("IDLE", "+ idling\r\n* 2 EXISTS\r\n".into()),
            Expect::new("DONE", String::new()),
            // The watcher re-idles; the script then runs out and the
            // dropped connection ends the thread.
            Expect::untagged("IDLE", "+ idling\r\n".into()),
        ];
        let (port, handle) = testserver::imap(script);
        let watch = idle_watch(&account(port), "INBOX", "pw");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !watch.take_changed() {
            assert!(
                std::time::Instant::now() < deadline,
                "idle watcher never flagged the change"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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
