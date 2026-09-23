//! Header cache for maildirs: opening a mailbox used to read and
//! parse every message; now the parsed envelopes are persisted (under
//! `$XDG_CACHE_HOME/rmut/headers/`) keyed by the file's invariant base
//! name plus its byte length, so only new or changed files are parsed.
//! Flag renames keep the key; a partial IMAP cache file gaining its
//! body changes length and re-parses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::maildir::{self, MailFile};
use crate::message::{self, Envelope};

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    from: String,
    /// Absent in pre-1.24 caches; those entries just match `~f`
    /// against the short form until the file is re-parsed.
    #[serde(default)]
    from_full: String,
    subject: String,
    date: i64,
    msg_id: Option<String>,
    references: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
    lines: Option<usize>,
    list: Option<String>,
    /// Absent in pre-1.64 caches; those entries have no label until
    /// the file is re-parsed.
    #[serde(default)]
    label: Option<String>,
    /// Absent in pre-1.83 caches; those entries lose the break-thread
    /// marker until the file is re-parsed, which a rewrite forces
    /// anyway (the key carries the length).
    #[serde(default)]
    broken: bool,
}

/// Which header decoding wrote the cache. The key only notices a file
/// that changed, so a fix to how an unchanged file decodes would never
/// reach an envelope already cached; a cache from another decoder is
/// dropped whole and rebuilt instead. 1: mutt's RFC 2047 reading
/// (`rfc2047`), which pre-1 caches, written through mailparse, lack.
const DECODER: u32 = 1;

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    #[serde(default)]
    decoder: u32,
    /// The mirrored maildir, so `sweep` can drop caches of vanished
    /// mailboxes (absent in pre-1.30 files, which age out on
    /// their next rewrite).
    #[serde(default)]
    dir: String,
    entries: Vec<Entry>,
}

impl Entry {
    fn from_envelope(env: &Envelope, key: String) -> Entry {
        Entry {
            key,
            from: env.from.clone(),
            from_full: env.from_full.clone(),
            subject: env.subject.clone(),
            date: env.date,
            msg_id: env.msg_id.clone(),
            references: env.references.clone(),
            to: env.to.clone(),
            cc: env.cc.clone(),
            lines: env.lines,
            list: env.list.clone(),
            label: env.label.clone(),
            broken: env.broken,
        }
    }

    fn to_envelope(&self, file: MailFile) -> Envelope {
        // The display fields go through one_line on the way out as
        // well as in: a cache written before rmut cleaned tabs out of
        // headers would otherwise keep serving them, and no length
        // changed to make it re-parse.
        Envelope {
            file,
            from: message::one_line(&self.from),
            from_full: message::one_line(&self.from_full),
            subject: message::one_line(&self.subject),
            date: self.date,
            msg_id: self.msg_id.clone(),
            references: self.references.clone(),
            tagged: false,
            to: self.to.clone(),
            cc: self.cc.clone(),
            lines: self.lines,
            list: self.list.as_deref().map(message::one_line),
            label: self.label.as_deref().map(message::one_line),
            broken: self.broken,
        }
    }
}

/// Cache identity of one message file: the base name (immutable in
/// maildir, flags live after ":2,") plus the current byte length.
fn key_of(file: &MailFile) -> Option<String> {
    let name = file.path.file_name()?.to_str()?;
    let base = name.split(":2,").next()?;
    let len = std::fs::metadata(&file.path).ok()?.len();
    Some(format!("{base}\u{1}{len}"))
}

fn cache_path(dir: &Path) -> PathBuf {
    let abs = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .display()
        .to_string();
    crate::remote::cache_base()
        .join("headers")
        .join(format!("{}.toml", crate::remote::sanitize(&abs)))
}

/// Envelopes for every message in the maildir, parsing only the files
/// the cache does not know; the refreshed cache is written back when
/// anything changed. Returns (envelopes, unreadable count).
pub fn load_envelopes(dir: &Path) -> Result<(Vec<Envelope>, usize)> {
    load_envelopes_at(dir, &cache_path(dir))
}

fn load_envelopes_at(dir: &Path, cache_file: &Path) -> Result<(Vec<Envelope>, usize)> {
    let mut cached: HashMap<String, Entry> = std::fs::read_to_string(cache_file)
        .ok()
        .and_then(|text| toml::from_str::<CacheFile>(&text).ok())
        .filter(|c| c.decoder == DECODER)
        .map(|c| c.entries.into_iter().map(|e| (e.key.clone(), e)).collect())
        .unwrap_or_default();
    let mut envelopes = Vec::new();
    let mut fresh: Vec<Entry> = Vec::new();
    let mut skipped = 0usize;
    let mut misses = 0usize;
    let stale_at_start = cached.len();
    for file in maildir::scan(dir)? {
        let key = key_of(&file);
        match key.as_ref().and_then(|k| cached.remove(k)) {
            Some(entry) => {
                envelopes.push(entry.to_envelope(file));
                fresh.push(entry);
            }
            None => match message::envelope(file) {
                Ok(env) => {
                    misses += 1;
                    if let Some(key) = key {
                        fresh.push(Entry::from_envelope(&env, key));
                    }
                    envelopes.push(env);
                }
                Err(_) => skipped += 1,
            },
        }
    }
    // Rewrite when something was parsed anew or entries went stale;
    // an unchanged mailbox costs no write.
    if misses > 0 || fresh.len() != stale_at_start || !cached.is_empty() {
        if let Some(parent) = cache_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let out = CacheFile {
            decoder: DECODER,
            dir: dir.display().to_string(),
            entries: fresh,
        };
        if let Ok(text) = toml::to_string(&out) {
            let _ = std::fs::write(cache_file, text);
        }
    }
    Ok((envelopes, skipped))
}

/// Drop header caches of maildirs that no longer exist. Rate-limited
/// by a marker file, so callers can just invoke it at startup.
pub fn sweep() {
    sweep_at(&crate::remote::cache_base().join("headers"))
}

fn sweep_at(headers: &Path) {
    let marker = headers.join(".last-sweep");
    let week = std::time::Duration::from_secs(7 * 24 * 3600);
    if std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < week)
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(headers) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let dir = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<CacheFile>(&text).ok())
            .map(|c| c.dir)
            .unwrap_or_default();
        if !dir.is_empty() && !Path::new(&dir).join("cur").is_dir() {
            let _ = std::fs::remove_file(&path);
        }
    }
    let _ = std::fs::write(&marker, "swept\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_drops_caches_of_vanished_maildirs() {
        let tmp = tempfile::tempdir().unwrap();
        let headers = tmp.path().join("headers");
        std::fs::create_dir_all(&headers).unwrap();
        let md = tmp.path().join("md");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(md.join(sub)).unwrap();
        }
        write_msg(&md.join("cur"), "1.host:2,S", "live");
        load_envelopes_at(&md, &headers.join("live.toml")).unwrap();
        let gone = format!(
            "dir = \"{}\"\nentries = []\n",
            tmp.path().join("gone").display()
        );
        std::fs::write(headers.join("gone.toml"), &gone).unwrap();
        sweep_at(&headers);
        assert!(headers.join("live.toml").exists());
        assert!(!headers.join("gone.toml").exists());
        // The marker rate-limits: a fresh sweep leaves things alone.
        std::fs::write(headers.join("gone.toml"), &gone).unwrap();
        sweep_at(&headers);
        assert!(headers.join("gone.toml").exists());
    }

    fn write_msg(dir: &Path, name: &str, subject: &str) {
        let content = format!(
            "From: jane@example.com\nSubject: {subject}\nDate: Thu, 9 Jul 2026 10:00:00 +0200\n\nbody\n"
        );
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn hits_skip_parsing_and_survive_flag_renames() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("md");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(md.join(sub)).unwrap();
        }
        write_msg(&md.join("cur"), "1.host:2,S", "original");
        let cache = tmp.path().join("headers.toml");
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        assert_eq!(envs[0].subject, "original");
        assert!(cache.exists());
        // Same length, different content: the cache answers, proving
        // the file was not re-parsed.
        write_msg(&md.join("cur"), "1.host:2,S", "gerbiled"); // same len
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        assert_eq!(envs[0].subject, "original");
        // A flag rename keeps the key.
        std::fs::rename(md.join("cur/1.host:2,S"), md.join("cur/1.host:2,FS")).unwrap();
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        assert_eq!(envs[0].subject, "original");
        assert!(envs[0].file.flags.flagged);
        // A length change re-parses.
        write_msg(&md.join("cur"), "1.host:2,FS", "changed for real");
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        assert_eq!(envs[0].subject, "changed for real");
    }

    #[test]
    fn a_cache_from_another_decoder_is_rebuilt() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("md");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(md.join(sub)).unwrap();
        }
        write_msg(&md.join("cur"), "1.host:2,S", "=?utf-8?Q?Nov=C3=BD_?=text");
        let cache = tmp.path().join("headers.toml");
        load_envelopes_at(&md, &cache).unwrap();
        // What an older rmut left: the same key, the word undecoded,
        // and no decoder field.
        let text = std::fs::read_to_string(&cache).unwrap();
        let old = text
            .replace(&format!("decoder = {DECODER}\n"), "")
            .replace("Nový text", "=?utf-8?Q?Nov=C3=BD_?=text");
        std::fs::write(&cache, old).unwrap();
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        assert_eq!(envs[0].subject, "Nový text");
        let text = std::fs::read_to_string(&cache).unwrap();
        assert!(text.contains(&format!("decoder = {DECODER}")), "{text}");
    }

    #[test]
    fn stale_entries_are_pruned_and_new_files_added() {
        let tmp = tempfile::tempdir().unwrap();
        let md = tmp.path().join("md");
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(md.join(sub)).unwrap();
        }
        write_msg(&md.join("cur"), "1.host:2,S", "one");
        write_msg(&md.join("cur"), "2.host:2,S", "two");
        let cache = tmp.path().join("headers.toml");
        load_envelopes_at(&md, &cache).unwrap();
        std::fs::remove_file(md.join("cur/2.host:2,S")).unwrap();
        write_msg(&md.join("new"), "3.host", "three");
        let (envs, _) = load_envelopes_at(&md, &cache).unwrap();
        let subjects: Vec<_> = envs.iter().map(|e| e.subject.as_str()).collect();
        assert_eq!(envs.len(), 2);
        assert!(subjects.contains(&"one") && subjects.contains(&"three"));
        // The pruned entry is gone from the cache file too.
        let text = std::fs::read_to_string(&cache).unwrap();
        assert!(!text.contains("two"), "{text}");
    }
}
