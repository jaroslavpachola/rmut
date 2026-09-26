//! Private temporary files: a part for a mailcap `%s` command, a
//! signature for gpg, a draft for the editor. They live in the shared
//! temp dir, so each is created fresh (O_EXCL: never through a link or
//! over a file someone else put there) and readable by us alone.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};

/// `bytes` in a new file `rmut-<what>-<unique><suffix>` under the temp
/// dir, mode 0600. The caller removes it.
pub fn write(what: &str, suffix: &str, bytes: &[u8]) -> Result<PathBuf> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    for _ in 0..100 {
        // The clock makes the name hard to guess ahead of time; a taken
        // name is skipped rather than reused either way.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let path = std::env::temp_dir().join(format!(
            "rmut-{what}-{}-{}-{nanos:08x}{suffix}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        };
        if let Err(e) = file.write_all(bytes) {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("writing {}", path.display()));
        }
        return Ok(path);
    }
    bail!(
        "no free temporary file name in {}",
        std::env::temp_dir().display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn private_and_fresh_each_time() {
        let a = write("scratch-test", ".txt", b"one").unwrap();
        let b = write("scratch-test", ".txt", b"two").unwrap();
        assert_ne!(a, b);
        assert!(a.to_str().unwrap().ends_with(".txt"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        let mode = std::fs::metadata(&a).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }

    #[test]
    fn never_writes_through_a_planted_link() {
        // create_new refuses a name that exists, a dangling symlink
        // included, so a link at the path cannot redirect the write.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("victim");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&link)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);
        assert!(!target.exists());
    }
}
