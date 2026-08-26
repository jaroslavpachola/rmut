//! Mutt-style search/limit patterns. Terms: `~f` from, `~s` subject,
//! `~b` body, `~t` to, `~c` cc, `~C` to-or-cc, `~e` sender, `~h` any
//! header, `~i` Message-ID, `~x` References, `~d` date, `~r` received
//! date, `~m` index range, `~z` size range, `~=` duplicate,
//! `~N` new, `~F` flagged, `~D` deleted, `~U` unread, `~T` tagged,
//! `~l` addressed to a known mailing list,
//! `~p` addressed to me, `~P` sent by me, `~A` every message;
//! a bare word matches subject or from (mutt's
//! $simple_search). Adjacent terms AND, `|` ORs, `!` negates, `()`
//! groups; string arguments are case-insensitive regexes (quote them
//! to include spaces), `~d`/`~r` take `DD/MM/YYYY` ranges or `<`/`>`/`=`
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
    /// `~h`: any header line, as `Name: value` text (read from disk).
    Header(Matcher),
    /// `~i`: the Message-ID.
    MessageId(Matcher),
    /// `~x`: any References / In-Reply-To id.
    References(Matcher),
    /// `~(P)`: some message in the same thread matches P.
    Thread(Box<Pattern>),
    /// `~<(P)`: the immediate parent matches P.
    Parent(Box<Pattern>),
    /// `~>(P)`: an immediate child matches P.
    Child(Box<Pattern>),
    /// `~v`: the message heads a collapsed thread.
    Collapsed,
    /// `~$`: no parent and no children, in a threaded index.
    Unreferenced,
    /// `~d`: epoch-second bounds, min inclusive, max exclusive.
    Date {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// `~r`: the same bounds against the file's delivery time.
    Received {
        min: Option<i64>,
        max: Option<i64>,
    },
    /// `~m`: index-number range, inclusive, over the numbering on
    /// screen; `.` is the selected message and `$` the last one.
    Number {
        min: Option<Bound>,
        max: Option<Bound>,
    },
    /// `~z`: size in bytes, min inclusive, max inclusive.
    Size {
        min: Option<u64>,
        max: Option<u64>,
    },
    /// `~=`: the Message-ID occurs more than once in the mailbox.
    Duplicate,
    New,
    Flagged,
    Deleted,
    Unread,
    Tagged,
    /// `~p`: addressed to one of my addresses.
    ToMe,
    /// `~P`: sent by me (From is one of my addresses).
    FromMe,
    /// `~l`: addressed to a known mailing list.
    ToList,
    /// Bare word: matches subject or from.
    Default(Matcher),
}

/// One end of a `~m` range: a literal number, or mutt's `.` (the
/// selected message) and `$` (the last), resolved at match time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Num(usize),
    Current,
    Last,
}

impl Bound {
    fn resolve(self, pos: &Position) -> usize {
        match self {
            Bound::Num(n) => n,
            Bound::Current => pos.current,
            Bound::Last => pos.last,
        }
    }
}

/// Which addresses count as mine: the exact ones the identity layers
/// name (bare, lowercase) plus mutt's `alternates`, regexes over the
/// bare address. Everything that asks "is this me?" goes through here:
/// `~p` and `~P`, the `+`/`T`/`C`/`F` index marks, reverse_name, the
/// group-reply dedup, and Mail-Followup-To.
#[derive(Default, Clone, Copy)]
pub struct Me<'a> {
    /// My bare lowercase addresses.
    pub addresses: &'a [String],
    /// mutt's `alternates`: regexes for my other addresses.
    pub alternates: &'a [Matcher],
}

impl<'a> Me<'a> {
    pub fn new(addresses: &'a [String], alternates: &'a [Matcher]) -> Me<'a> {
        Me {
            addresses,
            alternates,
        }
    }

    /// Only the exact addresses, no alternates (tests, and callers
    /// that have no config in reach).
    pub fn addresses(addresses: &'a [String]) -> Me<'a> {
        Me {
            addresses,
            alternates: &[],
        }
    }

    pub fn is_me(&self, addr: &str) -> bool {
        let bare = addr.trim().to_lowercase();
        self.addresses.contains(&bare) || self.alternates.iter().any(|m| m.is_match(&bare))
    }

    /// True when any of these bare addresses is mine.
    pub fn any<'b>(&self, addresses: impl IntoIterator<Item = &'b String>) -> bool {
        addresses.into_iter().any(|a| self.is_me(a))
    }

    /// True when the message's From names one of my addresses (`~P`).
    pub fn wrote(&self, from_header: &str) -> bool {
        crate::compose::addresses(from_header)
            .iter()
            .any(|a| self.is_me(a))
    }

    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty() && self.alternates.is_empty()
    }
}

/// Everything matching needs besides the message: my own addresses
/// (`~p`, `~P`), the known mailing lists (`~l`), and the message's
/// place in the list (`~m`, `~=`).
#[derive(Default, Clone, Copy)]
pub struct Scope<'a> {
    pub me: Me<'a>,
    /// Address patterns naming mailing lists, subscribed or not.
    pub lists: &'a [Matcher],
    pub position: Position,
    /// The message's place in its thread, for `~(`, `~<`, `~>`, `~v`
    /// and `~$`. None outside a threaded index, where `~(P)` means P
    /// of the message itself and the rest are false.
    pub thread: Option<ThreadView<'a>>,
}

/// One message's neighbours in the thread, by index into `envs`.
#[derive(Clone, Copy)]
pub struct ThreadView<'a> {
    pub members: &'a [usize],
    pub parent: Option<usize>,
    pub children: &'a [usize],
    pub collapsed: bool,
    pub envs: &'a dyn EnvSource,
}

/// Where a thread term finds the other messages: a session's list,
/// or a plain slice in a test.
pub trait EnvSource {
    fn envelope(&self, i: usize) -> Option<&Envelope>;
}

impl EnvSource for [Envelope] {
    fn envelope(&self, i: usize) -> Option<&Envelope> {
        self.get(i)
    }
}

impl EnvSource for Vec<Envelope> {
    fn envelope(&self, i: usize) -> Option<&Envelope> {
        self.get(i)
    }
}

impl<'a> Scope<'a> {
    /// The common case: my addresses, nothing else known.
    pub fn me(me: &'a [String]) -> Scope<'a> {
        Scope {
            me: Me::addresses(me),
            ..Default::default()
        }
    }

    /// True when any of these addresses names a known list.
    pub fn any_list(&self, addresses: &[String]) -> bool {
        addresses
            .iter()
            .any(|a| self.lists.iter().any(|m| m.is_match(a)))
    }
}

/// What a message needs from the list around it: its index number for
/// `~m` and whether its Message-ID repeats for `~=`. Matching without
/// a list (a single color rule, say) leaves this at its default, where
/// both terms are false.
#[derive(Debug, Clone, Copy, Default)]
pub struct Position {
    /// 1-based number as shown in the index; 0 when not on screen.
    pub number: usize,
    /// The selected message's number, for `.`.
    pub current: usize,
    /// The last message's number, for `$`.
    pub last: usize,
    pub duplicate: bool,
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
                    // `~(`: the paren is the operator's own; the
                    // group it opens is parsed as a term of its own.
                    if op == '(' {
                        out.push(Tok::LParen);
                    }
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
        Some(Tok::Op(op @ ('(' | '<' | '>'))) => {
            match toks.next() {
                Some(Tok::LParen) => {}
                _ => return Err(format!("~{op} needs a (pattern)")),
            }
            let inner = Box::new(parse_or(toks, now)?);
            match toks.next() {
                Some(Tok::RParen) => {}
                _ => return Err("missing )".into()),
            }
            Ok(match op {
                '(' => Pattern::Thread(inner),
                '<' => Pattern::Parent(inner),
                _ => Pattern::Child(inner),
            })
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
                'h' => Pattern::Header(Matcher::new(&arg()?)),
                'i' => Pattern::MessageId(Matcher::new(&arg()?)),
                'x' => Pattern::References(Matcher::new(&arg()?)),
                'd' => date_term(&arg()?, now)?,
                'r' => match date_term(&arg()?, now)? {
                    Pattern::Date { min, max } => Pattern::Received { min, max },
                    other => other,
                },
                'm' => number_term(&arg()?)?,
                'z' => size_term(&arg()?)?,
                '=' => Pattern::Duplicate,
                'N' => Pattern::New,
                'F' => Pattern::Flagged,
                'D' => Pattern::Deleted,
                'U' => Pattern::Unread,
                'T' => Pattern::Tagged,
                'A' => Pattern::All(Vec::new()),
                'p' => Pattern::ToMe,
                'P' => Pattern::FromMe,
                'l' => Pattern::ToList,
                'v' => Pattern::Collapsed,
                '$' => Pattern::Unreferenced,
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

// ---- ~m index ranges and ~z sizes ----

/// `~m 10-20`, `~m 5`, `~m 5-`, `~m -20`, and mutt's `.` (selected)
/// and `$` (last) at either end.
fn number_term(spec: &str) -> Result<Pattern, String> {
    let end = |text: &str| -> Result<Bound, String> {
        match text {
            "." => Ok(Bound::Current),
            "$" => Ok(Bound::Last),
            n => n
                .parse()
                .map(Bound::Num)
                .map_err(|_| format!("~m wants a number, . or $, got {n:?}")),
        }
    };
    match spec.split_once('-') {
        Some((min, max)) => Ok(Pattern::Number {
            min: (!min.is_empty()).then(|| end(min)).transpose()?,
            max: (!max.is_empty()).then(|| end(max)).transpose()?,
        }),
        None => {
            let one = end(spec)?;
            Ok(Pattern::Number {
                min: Some(one),
                max: Some(one),
            })
        }
    }
}

/// `~z >100K`, `~z <2M`, `~z 10K-1M`, `~z 500`. K/M/G suffixes are
/// mutt's (powers of 1024), case-insensitive.
fn size_term(spec: &str) -> Result<Pattern, String> {
    if let Some(rest) = spec.strip_prefix('>') {
        return Ok(Pattern::Size {
            min: Some(bytes(rest)?),
            max: None,
        });
    }
    if let Some(rest) = spec.strip_prefix('<') {
        return Ok(Pattern::Size {
            min: None,
            max: Some(bytes(rest)?),
        });
    }
    match spec.split_once('-') {
        Some((min, max)) => Ok(Pattern::Size {
            min: (!min.is_empty()).then(|| bytes(min)).transpose()?,
            max: (!max.is_empty()).then(|| bytes(max)).transpose()?,
        }),
        None => {
            let exact = bytes(spec)?;
            Ok(Pattern::Size {
                min: Some(exact),
                max: Some(exact),
            })
        }
    }
}

fn bytes(spec: &str) -> Result<u64, String> {
    let spec = spec.trim();
    let err = || format!("~z wants a size like 100K, got {spec:?}");
    let (digits, scale) = match spec.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&spec[..spec.len() - 1], 1024),
        Some('M') => (&spec[..spec.len() - 1], 1024 * 1024),
        Some('G') => (&spec[..spec.len() - 1], 1024 * 1024 * 1024),
        _ => (spec, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .map(|n| n * scale)
        .map_err(|_| err())
}

// ---- matching ----

/// Answers `~b` for a message without the local file read: Some
/// when a server-side search already knows, None to fall back.
pub type BodyOracle<'a> = &'a dyn Fn(&Envelope, &Matcher) -> Option<bool>;

/// Per-message context: disk reads happen at most once.
struct Ctx<'a> {
    env: &'a Envelope,
    scope: Scope<'a>,
    body: Option<String>,
    sender: Option<String>,
    headers: Option<String>,
    received: Option<i64>,
    oracle: Option<BodyOracle<'a>>,
}

/// AND of the top-level patterns. `me` are my own bare lowercase
/// addresses, for `~p`.
pub fn matches(patterns: &[Pattern], env: &Envelope, me: &[String]) -> bool {
    matches_in(patterns, env, Scope::me(me), None)
}

/// Like `matches`, with `~b` optionally answered by `oracle`
/// (server-side IMAP search) instead of reading the file.
pub fn matches_via(
    patterns: &[Pattern],
    env: &Envelope,
    me: &[String],
    oracle: Option<BodyOracle>,
) -> bool {
    matches_in(patterns, env, Scope::me(me), oracle)
}

/// The full entry point: `scope` answers the terms a lone message
/// cannot (`~p`, `~l`, `~m`, `~=`).
pub fn matches_in<'a>(
    patterns: &[Pattern],
    env: &'a Envelope,
    scope: Scope<'a>,
    oracle: Option<BodyOracle<'a>>,
) -> bool {
    let mut ctx = Ctx {
        env,
        scope,
        body: None,
        sender: None,
        headers: None,
        received: None,
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
            Pattern::Not(term)
            | Pattern::Thread(term)
            | Pattern::Parent(term)
            | Pattern::Child(term) => {
                walk(term, out);
            }
            Pattern::Body(m) if !out.contains(&m.raw) => out.push(m.raw.clone()),
            _ => {}
        }
    }
    patterns.iter().for_each(|p| walk(p, &mut out));
    out
}

/// A thread term's inner pattern over another message of the thread:
/// the same me and lists, but no position and no thread of its own,
/// so `~m`, `~=` and a nested thread term read as the lone-message
/// case.
fn eval_peer(inner: &Pattern, view: ThreadView, i: usize, ctx: &Ctx) -> bool {
    let Some(env) = view.envs.envelope(i) else {
        return false;
    };
    let mut peer = Ctx {
        env,
        scope: Scope {
            me: ctx.scope.me,
            lists: ctx.scope.lists,
            position: Position::default(),
            thread: None,
        },
        body: None,
        sender: None,
        headers: None,
        received: None,
        oracle: ctx.oracle,
    };
    eval(inner, &mut peer)
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
        Pattern::Header(m) => {
            let headers = ctx
                .headers
                .get_or_insert_with(|| message::header_text(&env.file.path).unwrap_or_default());
            m.is_match(headers)
        }
        Pattern::MessageId(m) => env.msg_id.as_deref().is_some_and(|id| m.is_match(id)),
        Pattern::References(m) => env.references.iter().any(|id| m.is_match(id)),
        Pattern::Date { min, max } => {
            min.is_none_or(|min| env.date >= min) && max.is_none_or(|max| env.date < max)
        }
        Pattern::Received { min, max } => {
            // Delivery time, which for a maildir is the file's mtime;
            // the Date header stands in when the file is unreadable.
            let at = *ctx.received.get_or_insert_with(|| {
                std::fs::metadata(&env.file.path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(env.date)
            });
            min.is_none_or(|min| at >= min) && max.is_none_or(|max| at < max)
        }
        Pattern::Number { min, max } => {
            let n = ctx.scope.position.number;
            n > 0
                && min.is_none_or(|b| n >= b.resolve(&ctx.scope.position))
                && max.is_none_or(|b| n <= b.resolve(&ctx.scope.position))
        }
        Pattern::Size { min, max } => {
            let size = env.file.size;
            min.is_none_or(|min| size >= min) && max.is_none_or(|max| size <= max)
        }
        Pattern::Duplicate => ctx.scope.position.duplicate,
        Pattern::Thread(inner) => match ctx.scope.thread {
            Some(t) => t.members.iter().any(|&i| eval_peer(inner, t, i, ctx)),
            None => eval(inner, ctx),
        },
        Pattern::Parent(inner) => match ctx.scope.thread {
            Some(t) => t.parent.is_some_and(|i| eval_peer(inner, t, i, ctx)),
            None => false,
        },
        Pattern::Child(inner) => match ctx.scope.thread {
            Some(t) => t.children.iter().any(|&i| eval_peer(inner, t, i, ctx)),
            None => false,
        },
        Pattern::Collapsed => ctx.scope.thread.is_some_and(|t| t.collapsed),
        Pattern::Unreferenced => ctx
            .scope
            .thread
            .is_some_and(|t| t.parent.is_none() && t.children.is_empty()),
        Pattern::New => env.file.is_new,
        Pattern::Flagged => env.file.flags.flagged,
        Pattern::Deleted => env.file.flags.deleted,
        Pattern::Unread => !env.file.flags.seen,
        Pattern::Tagged => env.tagged,
        Pattern::ToMe => ctx.scope.me.any(env.to.iter().chain(&env.cc)),
        Pattern::FromMe => ctx.scope.me.wrote(&env.from_full),
        Pattern::ToList => env
            .to
            .iter()
            .chain(&env.cc)
            .any(|a| ctx.scope.lists.iter().any(|m| m.is_match(a))),
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
    fn patterns_v3_parse() {
        assert_eq!(
            ok("~i msg1"),
            vec![Pattern::MessageId(Matcher::new("msg1"))]
        );
        assert_eq!(
            ok("~x parent"),
            vec![Pattern::References(Matcher::new("parent"))]
        );
        assert_eq!(
            ok("~h x-spam"),
            vec![Pattern::Header(Matcher::new("x-spam"))]
        );
        assert_eq!(ok("~="), vec![Pattern::Duplicate]);
        assert_eq!(
            ok("~m 10-20"),
            vec![Pattern::Number {
                min: Some(Bound::Num(10)),
                max: Some(Bound::Num(20)),
            }]
        );
        assert_eq!(
            ok("~m .-$"),
            vec![Pattern::Number {
                min: Some(Bound::Current),
                max: Some(Bound::Last),
            }]
        );
        assert_eq!(
            ok("~m 7"),
            vec![Pattern::Number {
                min: Some(Bound::Num(7)),
                max: Some(Bound::Num(7)),
            }]
        );
        assert_eq!(
            ok("~z >100K"),
            vec![Pattern::Size {
                min: Some(102400),
                max: None,
            }]
        );
        assert_eq!(
            ok("~z 1K-2M"),
            vec![Pattern::Size {
                min: Some(1024),
                max: Some(2 * 1024 * 1024),
            }]
        );
        // ~r shares the ~d spec vocabulary but keeps its own variant.
        assert!(matches!(
            ok("~r <1w").as_slice(),
            [Pattern::Received { .. }]
        ));
        assert!(parse("~m nonsense").is_err());
        assert!(parse("~z 10X").is_err());
    }

    #[test]
    fn patterns_v3_match() {
        let mut e = env("jane@x", "lunch", false, Flags::default());
        e.msg_id = Some("msg1@example.com".into());
        e.references = vec!["parent@example.com".into()];
        e.file.size = 150 * 1024;
        assert!(matches(&ok("~i msg1"), &e, &[]));
        assert!(!matches(&ok("~i other"), &e, &[]));
        assert!(matches(&ok("~x parent"), &e, &[]));
        assert!(matches(&ok("~z >100K"), &e, &[]));
        assert!(!matches(&ok("~z >1M"), &e, &[]));
        assert!(matches(&ok("~z 100K-200K"), &e, &[]));
        // ~m and ~= are false without a list around the message.
        assert!(!matches(&ok("~m 1"), &e, &[]));
        assert!(!matches(&ok("~="), &e, &[]));
        let pos = Position {
            number: 3,
            current: 5,
            last: 9,
            duplicate: true,
        };
        let scope = Scope {
            position: pos,
            ..Default::default()
        };
        let at = |p: &str| matches_in(&ok(p), &e, scope, None);
        assert!(at("~m 3"));
        assert!(at("~m 1-3"));
        assert!(at("~m -3"));
        assert!(!at("~m 4-"));
        assert!(at("~m 1-."));
        assert!(!at("~m .-$"));
        assert!(at("~="));
        assert!(!matches_in(&ok("~m 3"), &e, Scope::default(), None));
        // ~l needs the configured list patterns to say anything.
        e.to = vec!["dev@lists.example.com".into()];
        assert!(!matches(&ok("~l"), &e, &[]));
        let lists = [Matcher::new("@lists\\.example\\.com")];
        let scope = Scope {
            lists: &lists,
            ..Default::default()
        };
        assert!(matches_in(&ok("~l"), &e, scope, None));
        assert!(scope.any_list(&["dev@lists.example.com".to_string()]));
    }

    #[test]
    fn thread_terms_look_at_the_neighbours() {
        // A thread of three: root (from jane), reply (from bob),
        // reply to the reply (from jane again). Views are built the
        // way a session builds them, over a slice.
        let envs = vec![
            env("jane@example.com", "root", false, Flags::default()),
            env("bob@example.com", "reply", false, Flags::default()),
            env("jane@example.com", "again", false, Flags::default()),
        ];
        let members = [0usize, 1, 2];
        let children_of_root = [1usize];
        let children_of_reply = [2usize];
        let view = |i: usize| ThreadView {
            members: &members,
            parent: match i {
                0 => None,
                1 => Some(0),
                _ => Some(1),
            },
            children: match i {
                0 => &children_of_root[..],
                1 => &children_of_reply[..],
                _ => &[],
            },
            collapsed: i == 0,
            envs: &envs,
        };
        let at = |pat: &str, i: usize| {
            let scope = Scope {
                thread: Some(view(i)),
                ..Default::default()
            };
            matches_in(&ok(pat), &envs[i], scope, None)
        };
        // ~(P): anyone in the thread.
        assert!(at("~(~f bob)", 0));
        assert!(at("~(~f bob)", 2));
        assert!(!at("~(~f alice)", 1));
        // ~<(P): the parent; ~>(P): a child.
        assert!(at("~<(~f jane)", 1));
        assert!(!at("~<(~f jane)", 2));
        assert!(!at("~<(~A)", 0));
        assert!(at("~>(~f bob)", 0));
        assert!(!at("~>(~f jane)", 0), "a grandchild is not a child");
        // ~v and ~$, and the terms compose like any other.
        assert!(at("~v", 0));
        assert!(!at("~v", 1));
        assert!(!at("~$", 0), "it has children");
        assert!(at("!~$ ~(~s again)", 2));
        // Without a thread view, ~(P) is P of the message and the
        // rest are false.
        let alone = |pat: &str, i: usize| matches_in(&ok(pat), &envs[i], Scope::default(), None);
        assert!(alone("~(~f bob)", 1));
        assert!(!alone("~(~f bob)", 0));
        assert!(!alone("~<(~A)", 1));
        assert!(!alone("~$", 0));
        // Spelling: the paren belongs to the operator.
        assert!(parse("~(").is_err());
        assert!(parse("~<~f x").is_err());
        assert!(parse("~(~f x").is_err());
    }

    #[test]
    fn parse_reports_errors() {
        assert!(parse("~Q").is_err());
        assert!(parse("~f").is_err());
        assert!(parse("~x").is_err()); // still needs an argument
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
    fn alternates_widen_who_counts_as_me() {
        let mut e = env("Jane Doe <jane@example.com>", "s", false, Flags::default());
        e.to = vec!["j.doe@old.example.com".into()];
        // The exact list does not have it; an alternate regex does.
        assert!(!matches(&ok("~p"), &e, &["me@example.com".to_string()]));
        let alternates = vec![Matcher::new("@old\\.example\\.com$")];
        let scope = Scope {
            me: Me::new(&[], &alternates),
            ..Default::default()
        };
        assert!(matches_in(&ok("~p"), &e, scope, None));
        // ~P reads the From header, so an alternate there is me too.
        assert!(!matches_in(&ok("~P"), &e, scope, None));
        let mine = vec!["jane@example.com".to_string()];
        let scope = Scope {
            me: Me::addresses(&mine),
            ..Default::default()
        };
        assert!(matches_in(&ok("~P"), &e, scope, None));
        assert!(!matches_in(&ok("~p"), &e, scope, None));
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
