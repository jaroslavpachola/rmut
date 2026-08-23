//! Mutt-style search/limit patterns. Terms: `~f` from, `~s` subject,
//! `~b` body, `~t` to, `~c` cc, `~C` to-or-cc, `~e` sender, `~d` date,
//! `~N` new, `~F` flagged, `~D` deleted, `~U` unread, `~T` tagged,
//! `~p` addressed to me; a bare word matches subject or from (mutt's
//! $simple_search). Adjacent terms AND, `|` ORs, `!` negates, `()`
//! groups; string arguments are case-insensitive regexes (quote them
//! to include spaces), `~d` takes `DD/MM/YYYY` ranges or `<`/`>`/`=`
//! offsets like `<1w`.

use chrono::{Datelike, Local, TimeZone};

use crate::message::{self, Envelope};

/// A string argument: tried as a case-insensitive regex; an invalid
/// regex degrades to a case-insensitive substring match.
#[derive(Debug, Clone)]
pub struct Matcher {
    raw: String,
    re: Option<regex_lite::Regex>,
}

impl Matcher {
    pub fn new(raw: &str) -> Matcher {
        Matcher {
            raw: raw.to_string(),
            re: regex_lite::Regex::new(&format!("(?i){raw}")).ok(),
        }
    }

    /// The pattern text as written (for server-side IMAP search).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn is_match(&self, text: &str) -> bool {
        match &self.re {
            Some(re) => re.is_match(text),
            None => text.to_lowercase().contains(&self.raw.to_lowercase()),
        }
    }

    /// Byte ranges of every match in `text`, for highlighters. The
    /// substring fallback compares ASCII-case-insensitively (which
    /// keeps offsets valid, unlike full lowercasing).
    pub fn find_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        if let Some(re) = &self.re {
            return re.find_iter(text).map(|m| (m.start(), m.end())).collect();
        }
        let hay = text.to_ascii_lowercase();
        let needle = self.raw.to_ascii_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut from = 0;
        while let Some(pos) = hay[from..].find(&needle) {
            let start = from + pos;
            out.push((start, start + needle.len()));
            from = start + needle.len();
        }
        out
    }
}

impl PartialEq for Matcher {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl Eq for Matcher {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    All(Vec<Pattern>),
    Any(Vec<Pattern>),
    Not(Box<Pattern>),
    From(Matcher),
    Subject(Matcher),
    Body(Matcher),
    To(Matcher),
    Cc(Matcher),
    /// `~C`: any To or Cc address.
    Recipient(Matcher),
    /// `~e`: the Sender header (read from disk on demand).
    Sender(Matcher),
    /// `~d`: epoch-second bounds, min inclusive, max exclusive.
    Date {
        min: Option<i64>,
        max: Option<i64>,
    },
    New,
    Flagged,
    Deleted,
    Unread,
    Tagged,
    /// `~p`: addressed to one of my addresses.
    ToMe,
    /// Bare word: matches subject or from.
    Default(Matcher),
}

// ---- parsing ----

enum Tok {
    Not,
    Or,
    LParen,
    RParen,
    Op(char),
    Word(String),
}

fn lex(input: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '!' => {
                chars.next();
                out.push(Tok::Not);
            }
            '|' => {
                chars.next();
                out.push(Tok::Or);
            }
            '(' => {
                chars.next();
                out.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                out.push(Tok::RParen);
            }
            '~' => {
                chars.next();
                if let Some(op) = chars.next() {
                    out.push(Tok::Op(op));
                }
            }
            '"' | '\'' => {
                let quote = c;
                chars.next();
                let mut word = String::new();
                for c in chars.by_ref() {
                    if c == quote {
                        break;
                    }
                    word.push(c);
                }
                out.push(Tok::Word(word));
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | '|') {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                out.push(Tok::Word(word));
            }
        }
    }
    out
}

/// Top-level terms (implicitly ANDed, like mutt).
pub fn parse(input: &str) -> Result<Vec<Pattern>, String> {
    parse_at(input, Local::now().timestamp())
}

/// `now` anchors relative `~d` offsets (injectable for tests).
fn parse_at(input: &str, now: i64) -> Result<Vec<Pattern>, String> {
    let mut toks = lex(input).into_iter().peekable();
    let top = parse_or(&mut toks, now)?;
    if toks.next().is_some() {
        return Err("unbalanced )".into());
    }
    Ok(match top {
        Pattern::All(terms) => terms,
        one => vec![one],
    })
}

type Toks = std::iter::Peekable<std::vec::IntoIter<Tok>>;

fn parse_or(toks: &mut Toks, now: i64) -> Result<Pattern, String> {
    let mut terms = vec![parse_and(toks, now)?];
    while matches!(toks.peek(), Some(Tok::Or)) {
        toks.next();
        terms.push(parse_and(toks, now)?);
    }
    Ok(if terms.len() == 1 {
        terms.pop().expect("one term")
    } else {
        Pattern::Any(terms)
    })
}

fn parse_and(toks: &mut Toks, now: i64) -> Result<Pattern, String> {
    let mut terms = Vec::new();
    while !matches!(toks.peek(), None | Some(Tok::Or) | Some(Tok::RParen)) {
        terms.push(parse_unary(toks, now)?);
    }
    Ok(if terms.len() == 1 {
        terms.pop().expect("one term")
    } else {
        Pattern::All(terms)
    })
}

fn parse_unary(toks: &mut Toks, now: i64) -> Result<Pattern, String> {
    match toks.next() {
        Some(Tok::Not) => Ok(Pattern::Not(Box::new(parse_unary(toks, now)?))),
        Some(Tok::LParen) => {
            let inner = parse_or(toks, now)?;
            match toks.next() {
                Some(Tok::RParen) => Ok(inner),
                _ => Err("missing )".into()),
            }
        }
        Some(Tok::Op(op)) => {
            let mut arg = || match toks.next() {
                Some(Tok::Word(w)) => Ok(w),
                _ => Err(format!("~{op} needs an argument")),
            };
            Ok(match op {
                'f' => Pattern::From(Matcher::new(&arg()?)),
                's' => Pattern::Subject(Matcher::new(&arg()?)),
                'b' => Pattern::Body(Matcher::new(&arg()?)),
                't' => Pattern::To(Matcher::new(&arg()?)),
                'c' => Pattern::Cc(Matcher::new(&arg()?)),
                'C' => Pattern::Recipient(Matcher::new(&arg()?)),
                'e' => Pattern::Sender(Matcher::new(&arg()?)),
                'd' => date_term(&arg()?, now)?,
                'N' => Pattern::New,
                'F' => Pattern::Flagged,
                'D' => Pattern::Deleted,
                'U' => Pattern::Unread,
                'T' => Pattern::Tagged,
                'p' => Pattern::ToMe,
                other => return Err(format!("unknown pattern ~{other}")),
            })
        }
        Some(Tok::Word(w)) => Ok(Pattern::Default(Matcher::new(&w))),
        _ => Err("empty pattern group".into()),
    }
}

// ---- ~d date specs ----

/// `<1w` newer than, `>2d` older than, `=3d` exactly that day, or
/// absolute `DD/MM/YYYY` (a single day, or a `-` range with either
/// end open).
fn date_term(spec: &str, now: i64) -> Result<Pattern, String> {
    if let Some(rest) = spec.strip_prefix('<') {
        return Ok(Pattern::Date {
            min: Some(now - offset_secs(rest)?),
            max: None,
        });
    }
    if let Some(rest) = spec.strip_prefix('>') {
        return Ok(Pattern::Date {
            min: None,
            max: Some(now - offset_secs(rest)?),
        });
    }
    if let Some(rest) = spec.strip_prefix('=') {
        let then = now - offset_secs(rest)?;
        let date = Local
            .timestamp_opt(then, 0)
            .earliest()
            .ok_or("bad offset")?
            .date_naive();
        let (min, max) = day_bounds_of(date)?;
        return Ok(Pattern::Date {
            min: Some(min),
            max: Some(max),
        });
    }
    match spec.split_once('-') {
        Some((min, max)) => {
            let min = if min.is_empty() {
                None
            } else {
                Some(day_bounds(min, now)?.0)
            };
            let max = if max.is_empty() {
                None
            } else {
                Some(day_bounds(max, now)?.1)
            };
            Ok(Pattern::Date { min, max })
        }
        None => {
            let (min, max) = day_bounds(spec, now)?;
            Ok(Pattern::Date {
                min: Some(min),
                max: Some(max),
            })
        }
    }
}

/// "2w" → seconds; units y(ears) m(onths) w(eeks) d(ays) H(ours)
/// M(inutes), months and years approximated like mutt.
fn offset_secs(s: &str) -> Result<i64, String> {
    let err = || format!("bad date offset {s:?} (want e.g. 1w, 2d, 3H)");
    let unit = s.chars().last().ok_or_else(err)?;
    let n: i64 = s[..s.len() - unit.len_utf8()].parse().map_err(|_| err())?;
    let secs = match unit {
        'y' => 365 * 86400,
        'm' => 30 * 86400,
        'w' => 7 * 86400,
        'd' => 86400,
        'H' => 3600,
        'M' => 60,
        _ => return Err(err()),
    };
    Ok(n * secs)
}

/// "DD[/MM[/YYYY]]" → that local day as [start, end) epoch seconds;
/// missing month/year come from `now`, 2-digit years mean 20xx.
fn day_bounds(s: &str, now: i64) -> Result<(i64, i64), String> {
    let err = || format!("bad date {s:?} (want DD/MM/YYYY)");
    let today = Local
        .timestamp_opt(now, 0)
        .earliest()
        .ok_or_else(err)?
        .date_naive();
    let mut parts = s.split('/');
    let day: u32 = parts.next().unwrap_or("").parse().map_err(|_| err())?;
    let month: u32 = match parts.next() {
        Some(m) => m.parse().map_err(|_| err())?,
        None => today.month(),
    };
    let year: i32 = match parts.next() {
        Some(y) => {
            let y: i32 = y.parse().map_err(|_| err())?;
            if y < 100 { 2000 + y } else { y }
        }
        None => today.year(),
    };
    if parts.next().is_some() {
        return Err(err());
    }
    let date = chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(err)?;
    day_bounds_of(date)
}

fn day_bounds_of(date: chrono::NaiveDate) -> Result<(i64, i64), String> {
    let midnight = date.and_hms_opt(0, 0, 0).ok_or("bad date")?;
    let start = Local
        .from_local_datetime(&midnight)
        .earliest()
        .ok_or("bad date")?
        .timestamp();
    Ok((start, start + 86400))
}

// ---- matching ----

/// Answers `~b` for a message without the local file read: Some
/// when a server-side search already knows, None to fall back.
pub type BodyOracle<'a> = &'a dyn Fn(&Envelope, &Matcher) -> Option<bool>;

/// Per-message context: disk reads happen at most once.
struct Ctx<'a> {
    env: &'a Envelope,
    me: &'a [String],
    body: Option<String>,
    sender: Option<String>,
    oracle: Option<BodyOracle<'a>>,
}

/// AND of the top-level patterns. `me` are my own bare lowercase
/// addresses, for `~p`.
pub fn matches(patterns: &[Pattern], env: &Envelope, me: &[String]) -> bool {
    matches_via(patterns, env, me, None)
}

/// Like `matches`, with `~b` optionally answered by `oracle`
/// (server-side IMAP search) instead of reading the file.
pub fn matches_via(
    patterns: &[Pattern],
    env: &Envelope,
    me: &[String],
    oracle: Option<BodyOracle>,
) -> bool {
    let mut ctx = Ctx {
        env,
        me,
        body: None,
        sender: None,
        oracle,
    };
    patterns.iter().all(|p| eval(p, &mut ctx))
}

/// Every `~b` argument in the pattern, for pre-resolving on a server.
pub fn body_terms(patterns: &[Pattern]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &Pattern, out: &mut Vec<String>) {
        match p {
            Pattern::All(terms) | Pattern::Any(terms) => {
                terms.iter().for_each(|t| walk(t, out));
            }
            Pattern::Not(term) => walk(term, out),
            Pattern::Body(m) if !out.contains(&m.raw) => out.push(m.raw.clone()),
            _ => {}
        }
    }
    patterns.iter().for_each(|p| walk(p, &mut out));
    out
}

fn eval(p: &Pattern, ctx: &mut Ctx) -> bool {
    let env = ctx.env;
    match p {
        Pattern::All(terms) => terms.iter().all(|t| eval(t, ctx)),
        Pattern::Any(terms) => terms.iter().any(|t| eval(t, ctx)),
        Pattern::Not(term) => !eval(term, ctx),
        Pattern::From(m) => m.is_match(&env.from) || m.is_match(&env.from_full),
        Pattern::Subject(m) => m.is_match(&env.subject),
        Pattern::Default(m) => {
            m.is_match(&env.subject) || m.is_match(&env.from) || m.is_match(&env.from_full)
        }
        Pattern::To(m) => env.to.iter().any(|a| m.is_match(a)),
        Pattern::Cc(m) => env.cc.iter().any(|a| m.is_match(a)),
        Pattern::Recipient(m) => env.to.iter().chain(&env.cc).any(|a| m.is_match(a)),
        Pattern::Sender(m) => {
            let sender = ctx.sender.get_or_insert_with(|| {
                message::first_header(&env.file.path, "Sender").unwrap_or_default()
            });
            !sender.is_empty() && m.is_match(sender)
        }
        Pattern::Date { min, max } => {
            min.is_none_or(|min| env.date >= min) && max.is_none_or(|max| env.date < max)
        }
        Pattern::New => env.file.is_new,
        Pattern::Flagged => env.file.flags.flagged,
        Pattern::Deleted => env.file.flags.deleted,
        Pattern::Unread => !env.file.flags.seen,
        Pattern::Tagged => env.tagged,
        Pattern::ToMe => env.to.iter().chain(&env.cc).any(|a| ctx.me.contains(a)),
        Pattern::Body(m) => {
            if let Some(answer) = ctx.oracle.and_then(|oracle| oracle(env, m)) {
                return answer;
            }
            let body = ctx
                .body
                .get_or_insert_with(|| message::body_text(&env.file.path).unwrap_or_default());
            m.is_match(body)
        }
    }
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
            from: crate::message::short_from(from),
            from_full: from.into(),
            subject: subject.into(),
            date: 0,
            msg_id: None,
            references: vec![],
            tagged: false,
            to: vec![],
            cc: vec![],
            lines: Some(0),
            list: None,
        }
    }

    fn ok(input: &str) -> Vec<Pattern> {
        parse(input).unwrap_or_else(|e| panic!("parse {input:?}: {e}"))
    }

    #[test]
    fn parse_mixed_terms() {
        assert_eq!(
            ok("~f jane ~N lunch"),
            vec![
                Pattern::From(Matcher::new("jane")),
                Pattern::New,
                Pattern::Default(Matcher::new("lunch")),
            ]
        );
        assert_eq!(ok(""), vec![]);
        // Quotes keep spaces together.
        assert_eq!(
            ok("~s \"pizza friday\""),
            vec![Pattern::Subject(Matcher::new("pizza friday"))]
        );
    }

    #[test]
    fn parse_reports_errors() {
        assert!(parse("~x").is_err());
        assert!(parse("~f").is_err());
        assert!(parse("(~N").is_err());
        assert!(parse("~N)").is_err());
        assert!(parse("~d nonsense").is_err());
    }

    #[test]
    fn matches_is_case_insensitive_and_anded() {
        let e = env("Jane Doe", "Lunch on Friday", true, Flags::default());
        assert!(matches(&ok("~f JANE"), &e, &[]));
        assert!(matches(&ok("~f jane ~s lunch"), &e, &[]));
        assert!(!matches(&ok("~f jane ~s dinner"), &e, &[]));
        assert!(matches(&ok("friday"), &e, &[]));
        assert!(matches(&ok("doe"), &e, &[])); // Default also matches from
        assert!(matches(&ok("~N"), &e, &[]));
        assert!(!matches(&ok("~F"), &e, &[]));
        assert!(matches(&[], &e, &[])); // empty pattern matches everything
    }

    #[test]
    fn from_matches_the_whole_header() {
        // Like mutt: ~f (and a bare word) match the address too, not
        // just the displayed name.
        let e = env(
            "Jane Doe <jane@example.com>",
            "Lunch",
            false,
            Flags::default(),
        );
        assert!(matches(&ok("~f jane@example.com"), &e, &[]));
        assert!(matches(&ok("~f example"), &e, &[]));
        assert!(matches(&ok("~f \"jane doe\""), &e, &[]));
        assert!(matches(&ok("example.com"), &e, &[]));
        assert!(!matches(&ok("~f petr@example.com"), &e, &[]));
        // A pre-1.24 header cache entry has no full header stored;
        // the short form still matches.
        let mut old = env(
            "Jane Doe <jane@example.com>",
            "Lunch",
            false,
            Flags::default(),
        );
        old.from_full = String::new();
        assert!(matches(&ok("~f doe"), &old, &[]));
        assert!(!matches(&ok("~f jane@example.com"), &old, &[]));
    }

    #[test]
    fn not_or_and_grouping() {
        let e = env("Jane", "Lunch", true, Flags::default());
        assert!(matches(&ok("!~F"), &e, &[]));
        assert!(!matches(&ok("!~N"), &e, &[]));
        assert!(matches(&ok("~f jane | ~f petr"), &e, &[]));
        assert!(matches(&ok("~f petr | ~f jane"), &e, &[]));
        assert!(!matches(&ok("~f petr | ~f alice"), &e, &[]));
        // AND binds tighter than OR.
        assert!(matches(&ok("~f petr ~s x | ~f jane ~s lunch"), &e, &[]));
        assert!(!matches(&ok("~f petr (~s x | ~s lunch)"), &e, &[]));
        assert!(matches(&ok("~f jane (~s x | ~s lunch)"), &e, &[]));
        assert!(matches(&ok("!(~f petr | ~f alice)"), &e, &[]));
    }

    #[test]
    fn body_terms_collect_and_the_oracle_answers() {
        let pats = ok("~b invoice ~s x !(~b a | ~b invoice)");
        assert_eq!(body_terms(&pats), vec!["invoice", "a"]);
        // The oracle's verdict replaces the (missing) local body read.
        let e = env("Jane", "report", false, Flags::default());
        let pats = ok("~b invoice");
        assert!(!matches(&pats, &e, &[]));
        let yes = |_: &Envelope, m: &Matcher| Some(m.raw() == "invoice");
        assert!(matches_via(&pats, &e, &[], Some(&yes)));
        // None falls back to the local read (empty body: no match).
        let dunno = |_: &Envelope, _: &Matcher| None;
        assert!(!matches_via(&pats, &e, &[], Some(&dunno)));
    }

    #[test]
    fn string_arguments_are_regexes() {
        let e = env("Jane", "Re: budget", false, Flags::default());
        assert!(matches(&ok("~s ^re:"), &e, &[]));
        assert!(!matches(&ok("~s ^budget"), &e, &[]));
        assert!(matches(&ok("~s bud.et"), &e, &[]));
        assert!(matches(&ok("~s \"re:.*budget\""), &e, &[]));
        // An invalid regex still works as a plain substring.
        let e2 = env("Jane", "cost [draft]", false, Flags::default());
        assert!(matches(&ok("~s \"[draft\""), &e2, &[]));
    }

    #[test]
    fn recipients_and_to_me() {
        let mut e = env("Jane", "s", false, Flags::default());
        e.to = vec!["team@example.com".into(), "me@example.com".into()];
        e.cc = vec!["boss@example.com".into()];
        assert!(matches(&ok("~t team@"), &e, &[]));
        assert!(!matches(&ok("~t boss@"), &e, &[]));
        assert!(matches(&ok("~c boss@"), &e, &[]));
        assert!(matches(&ok("~C boss@"), &e, &[]));
        assert!(matches(&ok("~C team@"), &e, &[]));
        let me = vec!["me@example.com".to_string()];
        assert!(matches(&ok("~p"), &e, &me));
        assert!(!matches(&ok("~p"), &e, &["other@example.com".to_string()]));
        assert!(!matches(&ok("~p"), &e, &[]));
    }

    #[test]
    fn sender_header_is_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("msg");
        std::fs::write(
            &path,
            "From: jane@example.com\nSender: list-bot@example.com\nSubject: s\n\nbody\n",
        )
        .unwrap();
        let mut e = env("jane", "s", false, Flags::default());
        e.file.path = path;
        assert!(matches(&ok("~e list-bot"), &e, &[]));
        assert!(!matches(&ok("~e jane"), &e, &[]));
        // No Sender header at all: ~e never matches.
        assert!(!matches(
            &ok("~e .*"),
            &env("j", "s", false, Flags::default()),
            &[]
        ));
    }

    #[test]
    fn date_offsets_and_ranges() {
        let now = 1_800_000_000i64; // fixed "now" for the offsets
        let mut e = env("Jane", "s", false, Flags::default());
        e.date = now - 3 * 86400; // three days ago
        let m = |input: &str, e: &Envelope| matches(&parse_at(input, now).unwrap(), e, &[]);
        assert!(m("~d <1w", &e));
        assert!(!m("~d <2d", &e));
        assert!(m("~d >2d", &e));
        assert!(!m("~d >1w", &e));
        assert!(m("~d =3d", &e));
        assert!(!m("~d =2d", &e));
        assert!(m("!~d <2d", &e));
        // Absolute days, resolved in the local timezone.
        let day = chrono::Local.timestamp_opt(e.date, 0).unwrap();
        let spec = day.format("%d/%m/%Y").to_string();
        assert!(m(&format!("~d {spec}"), &e));
        assert!(m(&format!("~d {spec}-"), &e));
        assert!(m(&format!("~d -{spec}"), &e));
        let before = day - chrono::Duration::days(2);
        assert!(!m(&format!("~d -{}", before.format("%d/%m/%Y")), &e));
        assert!(m(&format!("~d {}-", before.format("%d/%m/%Y")), &e));
        // Two-digit years and short forms parse.
        assert!(parse_at("~d 1/1/26-31/12/26", now).is_ok());
        assert!(parse_at("~d 15", now).is_ok());
    }

    #[test]
    fn tagged_pattern_reads_the_runtime_mark() {
        let mut e = env("Jane", "s", false, Flags::default());
        assert!(!matches(&ok("~T"), &e, &[]));
        e.tagged = true;
        assert!(matches(&ok("~T"), &e, &[]));
    }
}
