//! The line prompt's editing, history and completion: what happens
//! to the text when a key arrives, with no opinion about what the
//! text is for. A front end owns a [`LineEdit`] per open prompt and
//! a [`History`] for the session; the prompt's meaning stays with it.

use std::collections::HashMap;
use std::path::Path;

use crate::key::{KeyCode, KeyEvent, KeyModifiers};

/// Byte offset of the `cursor`-th char (the length when past the end).
pub fn byte_at(buf: &str, cursor: usize) -> usize {
    buf.char_indices()
        .nth(cursor)
        .map(|(i, _)| i)
        .unwrap_or(buf.len())
}

/// Where the word before `cursor` starts (whitespace skipped first).
fn word_start(buf: &str, cursor: usize) -> usize {
    let chars: Vec<char> = buf.chars().collect();
    let mut c = cursor.min(chars.len());
    while c > 0 && chars[c - 1].is_whitespace() {
        c -= 1;
    }
    while c > 0 && !chars[c - 1].is_whitespace() {
        c -= 1;
    }
    c
}

/// Where the word at or after `cursor` ends (whitespace skipped first).
fn word_end(buf: &str, cursor: usize) -> usize {
    let chars: Vec<char> = buf.chars().collect();
    let mut c = cursor.min(chars.len());
    while c < chars.len() && chars[c].is_whitespace() {
        c += 1;
    }
    while c < chars.len() && !chars[c].is_whitespace() {
        c += 1;
    }
    c
}

/// What a key did to the line, or what it asks the front end for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    /// The line changed, or the cursor moved. Nothing to do but draw.
    Edited,
    /// Esc, or mutt's Ctrl+G.
    Cancel,
    /// Enter: the line is the answer.
    Submit,
    /// Tab: complete, if the prompt knows how.
    Complete,
    /// Up / Down: step the history (`true` is older).
    History(bool),
    /// Not an editing key.
    Ignored,
}

/// The text of a line prompt and the cursor in it, as a char index.
#[derive(Clone, Debug, Default)]
pub struct LineEdit {
    pub buf: String,
    pub cursor: usize,
    /// Index into the history while browsing with Up/Down.
    hist_pos: Option<usize>,
    /// The line being typed, restored when browsing steps back past
    /// the newest history entry.
    stash: String,
}

impl LineEdit {
    /// The cursor at the end of the prefill.
    pub fn new(prefill: String) -> LineEdit {
        let cursor = prefill.chars().count();
        LineEdit {
            buf: prefill,
            cursor,
            hist_pos: None,
            stash: String::new(),
        }
    }

    /// Replace the line, cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.buf = text.to_string();
        self.cursor = self.buf.chars().count();
    }

    /// The line editor, mutt/readline style.
    pub fn key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let buf = &mut self.buf;
        let cursor = &mut self.cursor;
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('g') if ctrl => return Edit::Cancel,
            // mutt's backward-word, forward-word and kill-eow. Words
            // end at whitespace, as Ctrl+W's do.
            KeyCode::Char('b') if alt => *cursor = word_start(buf, *cursor),
            KeyCode::Char('f') if alt => *cursor = word_end(buf, *cursor),
            KeyCode::Char('d') if alt => {
                let end = word_end(buf, *cursor);
                let (from, to) = (byte_at(buf, *cursor), byte_at(buf, end));
                buf.replace_range(from..to, "");
            }
            KeyCode::Enter => return Edit::Submit,
            KeyCode::Tab => return Edit::Complete,
            KeyCode::Up => return Edit::History(true),
            KeyCode::Down => return Edit::History(false),
            KeyCode::Left => *cursor = cursor.saturating_sub(1),
            KeyCode::Right => *cursor = (*cursor + 1).min(buf.chars().count()),
            KeyCode::Home => *cursor = 0,
            KeyCode::End => *cursor = buf.chars().count(),
            KeyCode::Char('a') if ctrl => *cursor = 0,
            KeyCode::Char('e') if ctrl => *cursor = buf.chars().count(),
            KeyCode::Backspace => {
                if *cursor > 0 {
                    buf.remove(byte_at(buf, *cursor - 1));
                    *cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if *cursor < buf.chars().count() {
                    buf.remove(byte_at(buf, *cursor));
                }
            }
            KeyCode::Char('d') if ctrl => {
                if *cursor < buf.chars().count() {
                    buf.remove(byte_at(buf, *cursor));
                }
            }
            KeyCode::Char('u') if ctrl => {
                // Kill to the start of the line.
                let i = byte_at(buf, *cursor);
                buf.replace_range(..i, "");
                *cursor = 0;
            }
            KeyCode::Char('k') if ctrl => {
                let i = byte_at(buf, *cursor);
                buf.truncate(i);
            }
            KeyCode::Char('w') if ctrl => {
                // Kill the word before the cursor.
                let c = word_start(buf, *cursor);
                let (start, end) = (byte_at(buf, c), byte_at(buf, *cursor));
                buf.replace_range(start..end, "");
                *cursor = c;
            }
            // Any other Alt+letter is a key, not text to type.
            KeyCode::Char(c) if !ctrl && !alt => {
                buf.insert(byte_at(buf, *cursor), c);
                *cursor += 1;
            }
            _ => return Edit::Ignored,
        }
        Edit::Edited
    }

    /// Up/Down at a line prompt: recall the bucket's history (newest
    /// first); stepping back past the newest restores the line that
    /// was being typed.
    pub fn history_step(&mut self, bucket: &[String], older: bool) {
        if bucket.is_empty() {
            return;
        }
        let next = match (self.hist_pos, older) {
            (None, true) => Some(0),
            (None, false) => return,
            (Some(p), true) => Some((p + 1).min(bucket.len() - 1)),
            (Some(0), false) => None,
            (Some(p), false) => Some(p - 1),
        };
        match next {
            Some(p) => {
                if self.hist_pos.is_none() {
                    self.stash = self.buf.clone();
                }
                self.buf = bucket[p].clone();
            }
            None => self.buf = self.stash.clone(),
        }
        self.hist_pos = next;
        self.cursor = self.buf.chars().count();
    }
}

/// The history buckets, mutt-style: one shared list per input class,
/// named so a persisted file maps back onto them.
pub const KNOWN_BUCKETS: &[&str] = &[
    "mailbox", "pattern", "address", "command", "other", "file", "notmuch",
];

/// Prompt history per bucket, newest first.
#[derive(Default, Debug)]
pub struct History {
    buckets: HashMap<&'static str, Vec<String>>,
}

impl History {
    pub fn get(&self, bucket: &str) -> &[String] {
        self.buckets.get(bucket).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Remember an answer: newest first, no duplicates, 100 deep.
    pub fn push(&mut self, bucket: &'static str, entry: &str) {
        let entry = entry.trim();
        if entry.is_empty() {
            return;
        }
        let list = self.buckets.entry(bucket).or_default();
        list.retain(|e| e != entry);
        list.insert(0, entry.to_string());
        list.truncate(100);
    }

    /// Load persisted history: `bucket\tentry` lines, newest first
    /// within each bucket, as [`History::save`] wrote them. Unknown
    /// buckets are dropped.
    pub fn load(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        for line in text.lines() {
            if let Some((bucket, entry)) = line.split_once('\t')
                && !entry.is_empty()
                && let Some(known) = KNOWN_BUCKETS.iter().find(|b| **b == bucket)
            {
                let list = self.buckets.entry(known).or_default();
                // As push keeps it: no duplicates, 100 deep, even when
                // the file was edited by hand.
                if list.len() < 100 && !list.iter().any(|e| e == entry) {
                    list.push(entry.to_string());
                }
            }
        }
    }

    /// Write the history back, at most `cap` entries per bucket, the
    /// buckets in a fixed order. The file is replaced whole and kept
    /// private: it holds addresses and searches.
    pub fn save(&self, path: &Path, cap: usize) {
        let mut out = String::new();
        for bucket in KNOWN_BUCKETS {
            let Some(entries) = self.buckets.get(bucket) else {
                continue;
            };
            for entry in entries.iter().take(cap) {
                // A tab or newline in an entry would corrupt the file;
                // both are vanishingly rare in a prompt, and dropped.
                if !entry.contains(['\t', '\n']) {
                    out += &format!("{bucket}\t{entry}\n");
                }
            }
        }
        let _ = rmut_core::scratch::save_private(path, out.as_bytes());
    }
}

/// Tab-completion state: candidates for the token at `start`, `tail`
/// the text after the cursor that the completion leaves alone, and
/// `expect` the whole line after the last insertion (an edit in
/// between restarts the match).
#[derive(Clone, Debug)]
pub struct Complete {
    start: usize,
    tail: String,
    candidates: Vec<String>,
    index: usize,
    expect: String,
}

impl Complete {
    /// Where the token to complete begins, and the token: what stands
    /// before the cursor, after the last comma before it at an
    /// address prompt (mutt's address list) or from the start of the
    /// line elsewhere, leading space skipped.
    pub fn token(buf: &str, cursor: usize, address_list: bool) -> (usize, String) {
        let head = &buf[..byte_at(buf, cursor)];
        let after_comma = if address_list {
            head.rfind(',').map(|i| i + 1).unwrap_or(0)
        } else {
            0
        };
        let start =
            after_comma + head[after_comma..].len() - head[after_comma..].trim_start().len();
        (start, head[start..].trim().to_string())
    }

    /// The line with this candidate in, and the cursor just after it.
    fn fill(&self, buf: &str) -> (String, usize) {
        let head = format!("{}{}", &buf[..self.start], self.candidates[self.index]);
        let cursor = head.chars().count();
        (head + &self.tail, cursor)
    }

    /// Another Tab on an unchanged line: the next candidate (the line
    /// and the cursor), and the "match n/m" note. None when the line
    /// was edited since, or there is only one candidate.
    pub fn cycle(&mut self, buf: &str) -> Option<((String, usize), String)> {
        if self.expect != buf || self.candidates.len() < 2 {
            return None;
        }
        self.index = (self.index + 1) % self.candidates.len();
        let next = self.fill(buf);
        self.expect = next.0.clone();
        let note = format!("match {}/{}", self.index + 1, self.candidates.len());
        Some((next, note))
    }

    /// A fresh match for the token [`Complete::token`] found before
    /// `cursor`: the line with the first candidate in and the cursor
    /// after it, the note when there are more, and the state for the
    /// next Tab. What follows the cursor stays. `candidates` is not
    /// empty.
    pub fn first(
        buf: &str,
        cursor: usize,
        start: usize,
        candidates: Vec<String>,
    ) -> (Complete, (String, usize), Option<String>) {
        let mut state = Complete {
            start,
            tail: buf[byte_at(buf, cursor)..].to_string(),
            candidates,
            index: 0,
            expect: String::new(),
        };
        let next = state.fill(buf);
        state.expect = next.0.clone();
        let note = (state.candidates.len() > 1)
            .then(|| format!("match 1/{} (Tab cycles)", state.candidates.len()));
        (state, next, note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(edit: &mut LineEdit, code: KeyCode) -> Edit {
        edit.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(edit: &mut LineEdit, c: char) -> Edit {
        edit.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    #[test]
    fn ctrl_g_cancels_and_alt_moves_by_words() {
        let alt =
            |e: &mut LineEdit, c: char| e.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        let mut e = LineEdit::new("one two  three".into());
        assert_eq!(ctrl(&mut e, 'g'), Edit::Cancel);
        alt(&mut e, 'b');
        assert_eq!(e.cursor, 9);
        alt(&mut e, 'b');
        assert_eq!(e.cursor, 4);
        alt(&mut e, 'f');
        assert_eq!(e.cursor, 7);
        alt(&mut e, 'd');
        assert_eq!(e.buf, "one two");
        e.cursor = 0;
        alt(&mut e, 'd');
        assert_eq!(e.buf, " two");
        // Another Alt+letter types nothing.
        assert_eq!(alt(&mut e, 'x'), Edit::Ignored);
        assert_eq!(e.buf, " two");
    }

    #[test]
    fn editing_keys_move_insert_and_kill() {
        let mut e = LineEdit::new("ab".into());
        assert_eq!(press(&mut e, KeyCode::Char('c')), Edit::Edited);
        assert_eq!(e.buf, "abc");
        press(&mut e, KeyCode::Left);
        press(&mut e, KeyCode::Char('x'));
        assert_eq!(e.buf, "abxc");
        press(&mut e, KeyCode::Backspace);
        assert_eq!(e.buf, "abc");
        ctrl(&mut e, 'a');
        press(&mut e, KeyCode::Delete);
        assert_eq!(e.buf, "bc");
        ctrl(&mut e, 'e');
        ctrl(&mut e, 'u');
        assert_eq!(e.buf, "");
        e.set("two words here");
        ctrl(&mut e, 'w');
        assert_eq!(e.buf, "two words ");
        press(&mut e, KeyCode::Home);
        ctrl(&mut e, 'k');
        assert_eq!(e.buf, "");
        assert_eq!(press(&mut e, KeyCode::Enter), Edit::Submit);
        assert_eq!(press(&mut e, KeyCode::Esc), Edit::Cancel);
        assert_eq!(press(&mut e, KeyCode::Tab), Edit::Complete);
        assert_eq!(press(&mut e, KeyCode::Up), Edit::History(true));
        assert_eq!(press(&mut e, KeyCode::PageUp), Edit::Ignored);
    }

    #[test]
    fn cursor_is_in_chars_not_bytes() {
        let mut e = LineEdit::new("héllo".into());
        press(&mut e, KeyCode::Left);
        press(&mut e, KeyCode::Left);
        press(&mut e, KeyCode::Backspace);
        assert_eq!(e.buf, "hélo");
        assert_eq!(byte_at("héllo", 2), 3);
    }

    #[test]
    fn history_steps_and_restores_the_stash() {
        let bucket = vec!["newest".to_string(), "older".to_string()];
        let mut e = LineEdit::new("typing".into());
        e.history_step(&bucket, true);
        assert_eq!(e.buf, "newest");
        e.history_step(&bucket, true);
        assert_eq!(e.buf, "older");
        e.history_step(&bucket, true);
        assert_eq!(e.buf, "older", "stays at the oldest");
        e.history_step(&bucket, false);
        e.history_step(&bucket, false);
        assert_eq!(e.buf, "typing", "back past the newest restores the line");
        e.history_step(&[], true);
        assert_eq!(e.buf, "typing");
    }

    #[test]
    fn history_dedupes_and_round_trips_through_a_file() {
        let mut h = History::default();
        h.push("pattern", "~f jane");
        h.push("pattern", "~N");
        h.push("pattern", "~f jane");
        h.push("pattern", "  ");
        assert_eq!(h.get("pattern"), ["~f jane", "~N"]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("history");
        h.push("command", "set beep");
        h.save(&path, 1);
        let mut back = History::default();
        back.load(&path);
        assert_eq!(back.get("pattern"), ["~f jane"], "capped at 1");
        assert_eq!(back.get("command"), ["set beep"]);
        assert!(back.get("bogus").is_empty());
    }

    #[test]
    fn completion_tokens_and_cycling() {
        assert_eq!(Complete::token("jane, bo", 8, true), (6, "bo".into()));
        assert_eq!(Complete::token("  =arch", 7, false), (2, "=arch".into()));
        let (mut c, line, note) = Complete::first(
            "jane, bo",
            8,
            6,
            vec!["bob@example.com".into(), "bonnie@example.com".into()],
        );
        assert_eq!(line, ("jane, bob@example.com".to_string(), 21));
        assert_eq!(note.as_deref(), Some("match 1/2 (Tab cycles)"));
        let (line, note) = c.cycle(&line.0).unwrap();
        assert_eq!(line.0, "jane, bonnie@example.com");
        assert_eq!(note, "match 2/2");
        assert!(c.cycle("edited since").is_none());
        let (mut one, line, note) = Complete::first("x", 1, 0, vec!["xy".into()]);
        assert_eq!((line.0.as_str(), note), ("xy", None));
        assert!(one.cycle("xy").is_none());
    }

    #[test]
    fn completion_works_on_the_token_before_the_cursor() {
        // The cursor back on the first address of a list: that one
        // completes, and the rest of the line stays as it was.
        let buf = "ja, carol@example.com";
        let (start, word) = Complete::token(buf, 2, true);
        assert_eq!((start, word.as_str()), (0, "ja"));
        let (_, (line, cursor), _) =
            Complete::first(buf, 2, start, vec!["jane@example.com".into()]);
        assert_eq!(line, "jane@example.com, carol@example.com");
        assert_eq!(cursor, 16);
    }
}
