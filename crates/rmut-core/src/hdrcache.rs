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
    subject: String,
    date: i64,
    msg_id: Option<String>,
    references: Vec<String>,
    to: Vec<String>,
    cc: Vec<String>,
    lines: Option<usize>,
    list: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    entries: Vec<Entry>,
}

impl Entry {
    fn from_envelope(env: &Envelope, key: String) -> Entry {
        Entry {
            key,
            from: env.from.clone(),
            subject: env.subject.clone(),
            date: env.date,
            msg_id: env.msg_id.clone(),
            references: env.references.clone(),
            to: env.to.clone(),
            cc: env.cc.clone(),
            lines: env.lines,
            list: env.list.clone(),
        }
    }

    fn to_envelope(&self, file: MailFile) -> Envelope {
        Envelope {
            file,
            from: self.from.clone(),
            subject: self.subject.clone(),
            date: self.date,
            msg_id: self.msg_id.clone(),
            references: self.references.clone(),
            tagged: false,
            to: self.to.clone(),
            cc: self.cc.clone(),
            lines: self.lines,
            list: self.list.clone(),
        }
    }
}

/// Cache identity of one message file: the base name (immutable in
/// maildir — flags live after ":2,") plus the current byte length.
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
        if let Ok(text) = toml::to_string(&CacheFile { entries: fresh }) {
            let _ = std::fs::write(cache_file, text);
        }
    }
    Ok((envelopes, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

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
