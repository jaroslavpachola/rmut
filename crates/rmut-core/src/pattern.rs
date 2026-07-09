//! Mutt-ish search/limit patterns: `~f x` (from), `~s x` (subject),
//! `~b x` (body), `~N` (new), `~F` (flagged), `~D` (deleted), `~U`
//! (unread), `~T` (tagged); a bare word matches subject or from.
//! Multiple terms AND.

use crate::message::{self, Envelope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    From(String),
    Subject(String),
    Body(String),
    New,
    Flagged,
    Deleted,
    Unread,
    Tagged,
    /// Bare word: matches subject or from (mutt's $simple_search).
    Default(String),
}

pub fn parse(input: &str) -> Vec<Pattern> {
    let mut patterns = Vec::new();
    let mut tokens = input.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "~N" => patterns.push(Pattern::New),
            "~F" => patterns.push(Pattern::Flagged),
            "~D" => patterns.push(Pattern::Deleted),
            "~U" => patterns.push(Pattern::Unread),
            "~T" => patterns.push(Pattern::Tagged),
            "~f" | "~s" | "~b" => {
                let arg = tokens.next().unwrap_or("").to_string();
                patterns.push(match token {
                    "~f" => Pattern::From(arg),
                    "~s" => Pattern::Subject(arg),
                    _ => Pattern::Body(arg),
                });
            }
            word => patterns.push(Pattern::Default(word.to_string())),
        }
    }
    patterns
}

/// AND of all patterns; the body is read from disk at most once.
pub fn matches(patterns: &[Pattern], env: &Envelope) -> bool {
    let mut body: Option<String> = None;
    patterns.iter().all(|p| match p {
        Pattern::From(s) => contains(&env.from, s),
        Pattern::Subject(s) => contains(&env.subject, s),
        Pattern::Default(s) => contains(&env.subject, s) || contains(&env.from, s),
        Pattern::New => env.file.is_new,
        Pattern::Flagged => env.file.flags.flagged,
        Pattern::Deleted => env.file.flags.deleted,
        Pattern::Unread => !env.file.flags.seen,
        Pattern::Tagged => env.tagged,
        Pattern::Body(s) => {
            let body =
                body.get_or_insert_with(|| message::body_text(&env.file.path).unwrap_or_default());
            contains(body, s)
        }
    })
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maildir::{Flags, MailFile};

    fn env(from: &str, subject: &str, is_new: bool, flags: Flags) -> Envelope {
        Envelope {
            file: MailFile {
                path: "/nonexistent".into(),
                is_new,
                flags,
                size: 0,
            },
            from: from.into(),
            subject: subject.into(),
            date: 0,
            msg_id: None,
            references: vec![],
            tagged: false,
        }
    }

    #[test]
    fn parse_mixed_terms() {
        assert_eq!(
            parse("~f jane ~N lunch"),
            vec![
                Pattern::From("jane".into()),
                Pattern::New,
                Pattern::Default("lunch".into()),
            ]
        );
        assert_eq!(parse(""), vec![]);
    }

    #[test]
    fn matches_is_case_insensitive_and_anded() {
        let e = env("Jane Doe", "Lunch on Friday", true, Flags::default());
        assert!(matches(&parse("~f JANE"), &e));
        assert!(matches(&parse("~f jane ~s lunch"), &e));
        assert!(!matches(&parse("~f jane ~s dinner"), &e));
        assert!(matches(&parse("friday"), &e));
        assert!(matches(&parse("doe"), &e)); // Default also matches from
        assert!(matches(&parse("~N"), &e));
        assert!(!matches(&parse("~F"), &e));
        assert!(matches(&[], &e)); // empty pattern matches everything
    }

    #[test]
    fn tagged_pattern_reads_the_runtime_mark() {
        let mut e = env("Jane", "s", false, Flags::default());
        assert!(!matches(&parse("~T"), &e));
        e.tagged = true;
        assert!(matches(&parse("~T"), &e));
    }
}
