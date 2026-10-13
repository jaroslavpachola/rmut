//! Picking a file at a prompt, the way mutt's file prompts do it
//! (enter.c, complete.c, browser.c): Tab completes the path to the
//! longest prefix its directory agrees on, a Tab that changes nothing
//! (or one on an empty line) opens the file browser there, and the
//! browser descends into directories, tags files when the prompt
//! takes several, and hands back what was picked. Nothing in here
//! draws; both front ends show a [`Browser`] their own way.

use std::path::{Path, PathBuf};

use rmut_session::expand_tilde;

/// The path as typed, read for real: `~` and `~/` are the home
/// directory, a relative path is relative to where rmut runs.
fn expand(typed: &str) -> PathBuf {
    match typed {
        "~" => expand_tilde("~/"),
        _ => expand_tilde(typed),
    }
}

/// What a Tab at a file prompt does with `head`, the text before the
/// cursor, given `last`, the line as the previous Tab left it.
#[derive(Debug, PartialEq, Eq)]
pub enum Tab {
    /// The completed head, and how many names it could still be (1
    /// when it is whole: a file, or a directory with its `/`).
    Fill(String, usize),
    /// Nothing to add (an empty line, or a Tab on a line the last Tab
    /// left as it was): browse `dir`, the cursor on the first entry
    /// starting with `prefix`.
    Browse {
        dir: Option<PathBuf>,
        prefix: String,
    },
    /// No name in that directory starts that way.
    NoMatch,
}

/// mutt's enter.c: "see if the path has changed from the last time";
/// when it has not, the browser opens where the path points.
pub fn tab(head: &str, last: Option<&str>) -> Tab {
    if head.is_empty() || last == Some(head) {
        let (dir, prefix) = browse_start(head);
        return Tab::Browse { dir, prefix };
    }
    match complete(head) {
        Some((line, n)) => Tab::Fill(line, n),
        None => Tab::NoMatch,
    }
}

/// Split a typed path into the directory it names and the start of a
/// name in it; None for the directory when nothing was typed (the
/// browser then opens where it last was).
pub fn browse_start(typed: &str) -> (Option<PathBuf>, String) {
    if typed.is_empty() {
        return (None, String::new());
    }
    if typed == "~" {
        return (Some(expand(typed)), String::new());
    }
    match typed.rfind('/') {
        Some(0) => (Some(PathBuf::from("/")), typed[1..].to_string()),
        Some(i) => (Some(expand(&typed[..i + 1])), typed[i + 1..].to_string()),
        None => (Some(PathBuf::from(".")), typed.to_string()),
    }
}

/// mutt_complete (complete.c): the typed path with its last name
/// grown to the longest prefix the matching names share, a `/` after
/// it when it is the one directory that matches, and the number of
/// names that matched. Hidden names match only once a `.` is typed,
/// as $mask hides them in the browser. The directory part stays as it
/// was typed, `~` and all. None when nothing matches.
pub fn complete(typed: &str) -> Option<(String, usize)> {
    let (dir_typed, part) = match typed {
        "~" => ("~/", ""),
        _ => match typed.rfind('/') {
            Some(i) => (&typed[..i + 1], &typed[i + 1..]),
            None => ("", typed),
        },
    };
    let dir = if dir_typed.is_empty() {
        PathBuf::from(".")
    } else {
        expand(dir_typed)
    };
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let hidden = name.starts_with('.') && !part.starts_with('.');
            (name.starts_with(part) && !hidden).then(|| (name, is_dir(&e.path())))
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    let n = names.len();
    let grown = match names.as_slice() {
        [(name, true)] => format!("{name}/"),
        [(name, false)] => name.clone(),
        _ => common_prefix(names.iter().map(|(name, _)| name.as_str())),
    };
    Some((format!("{dir_typed}{grown}"), n))
}

/// The longest prefix all the names share, on char boundaries.
fn common_prefix<'a>(mut names: impl Iterator<Item = &'a str>) -> String {
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut len = first.len();
    for name in names {
        len = first
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((i, a), _)| i + a.len_utf8())
            .min(len);
    }
    first[..len].to_string()
}

/// A directory, or a link to one (mutt's link_is_dir).
fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

/// One row of the browser.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The name in the directory; `..` for the parent.
    pub name: String,
    pub is_dir: bool,
    /// Bytes; 0 for directories, as mutt shows none for them.
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub tagged: bool,
}

impl Entry {
    /// The browser row: the tag, size, date and name, a `/` marking a
    /// directory - mutt's $folder_format reduced to what is read.
    pub fn row(&self) -> String {
        let tag = if self.tagged { '*' } else { ' ' };
        let size = match self.is_dir {
            true => String::new(),
            false => rmut_core::message::pretty_size(self.size as usize),
        };
        let date = self
            .modified
            .map(|t| {
                chrono::DateTime::<chrono::Local>::from(t)
                    .format("%b %d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let slash = if self.is_dir && self.name != ".." {
            "/"
        } else {
            ""
        };
        format!("{tag} {size:>6}  {date:<12}  {}{slash}", self.name)
    }
}

/// What a key in the browser came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    /// Still browsing (a directory was entered, a tag toggled).
    Nothing,
    /// These files, in the order they are listed.
    Files(Vec<PathBuf>),
}

/// mutt's file browser over one directory at a time: everything but
/// hidden names ($mask's default `!^\.[^.]`), `..` on top, sorted by
/// name (mutt's $sort_browser alpha).
#[derive(Clone, Debug)]
pub struct Browser {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub sel: usize,
    /// The prompt takes several files: `t` tags, and leaving with
    /// tags picks them (mutt's MUTT_SEL_MULTI).
    pub multiple: bool,
}

impl Browser {
    /// Open at `dir`, or at `last` (where the previous browse ended)
    /// or the current directory when there is none, the cursor on the
    /// first name starting with `prefix`. A directory that is gone
    /// gives way to its parent, as mutt's examine_directory does for
    /// a deleted last directory.
    pub fn open(
        dir: Option<PathBuf>,
        last: Option<&Path>,
        prefix: &str,
        multiple: bool,
    ) -> Result<Browser, String> {
        let mut dir = dir
            .or_else(|| last.map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        if dir.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            dir = cwd.join(dir);
        }
        let mut dir = normalize(&dir);
        while !dir.exists() && dir.pop() {}
        let mut browser = Browser {
            dir,
            entries: Vec::new(),
            sel: 0,
            multiple,
        };
        browser.list()?;
        if !prefix.is_empty() {
            browser.sel = browser
                .entries
                .iter()
                .position(|e| e.name.starts_with(prefix))
                .unwrap_or(0);
        }
        Ok(browser)
    }

    /// Read the directory into the entries; tags do not survive it.
    fn list(&mut self) -> Result<(), String> {
        let read = std::fs::read_dir(&self.dir)
            .map_err(|err| format!("cannot browse {}: {err}", self.dir.display()))?;
        let mut entries: Vec<Entry> = read
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let path = e.path();
                // Through a link: its target says what it is.
                let meta = std::fs::metadata(&path).ok()?;
                (meta.is_dir() || meta.is_file()).then(|| Entry {
                    name,
                    is_dir: meta.is_dir(),
                    size: if meta.is_dir() { 0 } else { meta.len() },
                    modified: meta.modified().ok(),
                    tagged: false,
                })
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if self.dir.parent().is_some() {
            entries.insert(
                0,
                Entry {
                    name: "..".into(),
                    is_dir: true,
                    size: 0,
                    modified: None,
                    tagged: false,
                },
            );
        }
        self.entries = entries;
        self.sel = 0;
        Ok(())
    }

    /// The full path of the entry at `i`.
    pub fn path(&self, i: usize) -> Option<PathBuf> {
        let entry = self.entries.get(i)?;
        Some(match entry.name.as_str() {
            ".." => self.dir.parent()?.to_path_buf(),
            name => self.dir.join(name),
        })
    }

    /// Enter on the selected entry (mutt's select-entry): a directory
    /// is entered; a file is picked, and when files are tagged the
    /// tagged ones are what is picked.
    pub fn select(&mut self) -> Result<Picked, String> {
        let Some(entry) = self.entries.get(self.sel) else {
            return Err("no files here".into());
        };
        if entry.is_dir {
            let from = self.dir.clone();
            let to = self.path(self.sel).unwrap_or_else(|| from.clone());
            return self.chdir(to, Some(&from)).map(|()| Picked::Nothing);
        }
        let tagged = self.tagged();
        Ok(Picked::Files(match tagged.is_empty() {
            true => self.path(self.sel).into_iter().collect(),
            false => tagged,
        }))
    }

    /// Leaving (mutt's OP_EXIT): the tagged files when there are any,
    /// nothing otherwise.
    pub fn exit(&self) -> Picked {
        Picked::Files(self.tagged())
    }

    fn tagged(&self) -> Vec<PathBuf> {
        (0..self.entries.len())
            .filter(|&i| self.entries[i].tagged)
            .filter_map(|i| self.path(i))
            .collect()
    }

    /// `t`: tag or untag the selected file, then move down, as mutt's
    /// tag-entry does with $resolve. Directories cannot be attached.
    pub fn toggle_tag(&mut self) -> Result<(), String> {
        if !self.multiple {
            return Err("only one file can be picked here".into());
        }
        let Some(entry) = self.entries.get_mut(self.sel) else {
            return Ok(());
        };
        if entry.is_dir {
            return Err("Can't attach a directory!".into());
        }
        entry.tagged = !entry.tagged;
        self.sel = (self.sel + 1).min(self.entries.len().saturating_sub(1));
        Ok(())
    }

    /// Browse another directory (`c`, or entering one), the cursor on
    /// the directory just left when going up. A directory that cannot
    /// be read leaves the browser where it was.
    pub fn chdir(&mut self, to: PathBuf, from: Option<&Path>) -> Result<(), String> {
        let to = if to.is_relative() {
            self.dir.join(to)
        } else {
            to
        };
        let old = std::mem::replace(&mut self.dir, normalize(&to));
        if let Err(err) = self.list() {
            self.dir = old;
            let _ = self.list();
            return Err(err);
        }
        if let Some(from) = from
            && let Some(i) = self
                .entries
                .iter()
                .position(|e| e.name != ".." && self.dir.join(&e.name) == from)
        {
            self.sel = i;
        }
        Ok(())
    }

    pub fn tagged_count(&self) -> usize {
        self.entries.iter().filter(|e| e.tagged).count()
    }

    /// The browser's title: the directory, with `~` for home.
    pub fn title(&self) -> String {
        tilde(&self.dir)
    }
}

/// A path as it is best typed and read: under the home directory it
/// starts with `~`, which every file prompt expands again.
pub fn tilde(path: &Path) -> String {
    let shown = path.display().to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    match shown.strip_prefix(&home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => shown,
    }
}

/// `a/./b/../c` as `a/c`, without asking the filesystem (a link's
/// `..` is where it was entered from, as in a shell).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for d in ["Desktop", "Documents", "Downloads", "only"] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("Documents/report.pdf"), b"%PDF").unwrap();
        std::fs::write(dir.path().join("Documents/report.txt"), b"text").unwrap();
        std::fs::write(dir.path().join("only/one.png"), b"png").unwrap();
        std::fs::write(dir.path().join(".hidden"), b"").unwrap();
        dir
    }

    #[test]
    fn tab_grows_to_the_common_prefix_and_ends_a_directory_with_a_slash() {
        let t = tree();
        let root = format!("{}/", t.path().display());
        // Three names start with D: only the D is shared.
        assert_eq!(complete(&format!("{root}D")), Some((format!("{root}D"), 3)));
        assert_eq!(
            complete(&format!("{root}Do")),
            Some((format!("{root}Do"), 2))
        );
        assert_eq!(
            complete(&format!("{root}Doc")),
            Some((format!("{root}Documents/"), 1))
        );
        assert_eq!(
            complete(&format!("{root}Documents/r")),
            Some((format!("{root}Documents/report."), 2))
        );
        assert_eq!(
            complete(&format!("{root}only/")),
            Some((format!("{root}only/one.png"), 1))
        );
        assert_eq!(complete(&format!("{root}x")), None);
        // Hidden names wait for the dot.
        assert_eq!(
            complete(&format!("{root}.h")),
            Some((format!("{root}.hidden"), 1))
        );
    }

    #[test]
    fn a_tab_that_changes_nothing_browses() {
        let t = tree();
        let head = format!("{}/D", t.path().display());
        let Tab::Fill(line, 3) = tab(&head, None) else {
            panic!("expected a fill");
        };
        assert_eq!(
            tab(&line, Some(&line)),
            Tab::Browse {
                dir: Some(PathBuf::from(format!("{}/", t.path().display()))),
                prefix: "D".into()
            }
        );
        assert_eq!(
            tab("", None),
            Tab::Browse {
                dir: None,
                prefix: String::new()
            }
        );
    }

    #[test]
    fn the_browser_descends_tags_and_picks() {
        let t = tree();
        let mut b = Browser::open(Some(t.path().to_path_buf()), None, "Doc", true).unwrap();
        let names: Vec<&str> = b.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "Desktop", "Documents", "Downloads", "only"]);
        assert_eq!(b.entries[b.sel].name, "Documents");
        assert_eq!(b.select(), Ok(Picked::Nothing));
        assert_eq!(b.dir, t.path().join("Documents"));
        // A directory cannot be tagged; files can.
        assert!(b.toggle_tag().is_err());
        b.sel = 1;
        b.toggle_tag().unwrap();
        b.toggle_tag().unwrap();
        assert_eq!(b.tagged_count(), 2);
        assert_eq!(
            b.exit(),
            Picked::Files(vec![
                t.path().join("Documents/report.pdf"),
                t.path().join("Documents/report.txt")
            ])
        );
        // Up again: the cursor lands on the directory left.
        b.sel = 0;
        assert_eq!(b.select(), Ok(Picked::Nothing));
        assert_eq!(b.entries[b.sel].name, "Documents");
        // Enter on a file with nothing tagged picks that file.
        b.chdir(t.path().join("only"), None).unwrap();
        b.sel = 1;
        assert_eq!(
            b.select(),
            Ok(Picked::Files(vec![t.path().join("only/one.png")]))
        );
        assert_eq!(b.exit(), Picked::Files(vec![]));
    }

    #[test]
    fn a_gone_directory_gives_way_to_its_parent() {
        let t = tree();
        let b = Browser::open(None, Some(&t.path().join("gone/deeper")), "", false).unwrap();
        assert_eq!(b.dir, t.path());
        let mut one = Browser::open(Some(t.path().join("only")), None, "", false).unwrap();
        one.sel = 1;
        assert!(one.toggle_tag().is_err(), "a one-file prompt does not tag");
    }
}
