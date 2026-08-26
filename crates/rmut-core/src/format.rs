//! Mutt-like index format strings: `%[-][min][.max]X` where X is
//! `C` number, `Z` status/flag/mark chars, `d` date, `F` from, `L`
//! its list-aware variant ("To <list>" for List-Id mail), `c` size,
//! `l` body lines, `y` X-Label, `s` subject, `%` a literal percent. `-`
//! left-aligns, `min` pads, `.max` truncates (all in characters).
//! Mutt conditionals work too: `%?X?then&else?` renders `then` when
//! field X is set and non-zero.

/// Mutt's default index_format, with its inline `%{%b %e}` date as
/// `%-6d` (the date_format default is "%b %e").
pub const DEFAULT_FORMAT: &str = "%4C %Z %-6d %-15.15L (%?l?%4l&%4c?) %s";

/// Renders exactly rmut's classic status line; override with
/// `[ui] status_format`. Status specifiers: %f mailbox, %m messages,
/// %M shown-when-limited, %n new, %u unread, %d deleted, %F flagged,
/// %t tagged, %s sort, %V limit pattern, %r mailbox mark (* pending
/// changes, % read-only), %P index position, %v version; `%>X` fills
/// the rest of the width with X, right-aligning what follows.
pub const DEFAULT_STATUS_FORMAT: &str =
    "---rmut%r: %f [Msgs:%?M?%M/?%m New:%n%?d? Del:%d?] (sort:%s)%?V? (limit:%V)?";

pub struct IndexFields<'a> {
    pub number: usize,
    pub status: char,
    pub flag: char,
    /// Third %Z slot: '*' tagged, or how the mail addresses me
    /// ('+' only me in To, 'T' me among others, 'C' me in Cc).
    pub mark: char,
    pub date: &'a str,
    pub from: &'a str,
    pub size: &'a str,
    /// Body line count; None (header-only cache file) makes `%l`
    /// empty, so `%?l?…&…?` shows its else branch.
    pub lines: Option<usize>,
    /// Mailing-list name (List-Id); `%L` shows "To <name>" instead of
    /// the author when set.
    pub list: Option<&'a str>,
    /// Messages hidden under this collapsed thread root (`%M`).
    pub hidden: Option<usize>,
    /// mutt's X-Label, for `%y`.
    pub label: Option<&'a str>,
    pub subject: &'a str,
}

/// The raw text of one specifier, for both rendering and the
/// conditional set/unset test.
fn value_of(spec: char, f: &IndexFields) -> String {
    match spec {
        'C' => f.number.to_string(),
        'Z' => format!("{}{}{}", f.status, f.flag, f.mark),
        'd' => f.date.to_string(),
        'F' => f.from.to_string(),
        'L' => match f.list {
            Some(list) => format!("To {list}"),
            None => f.from.to_string(),
        },
        'c' => f.size.to_string(),
        'l' => f.lines.map(|n| n.to_string()).unwrap_or_default(),
        'M' => f.hidden.map(|n| n.to_string()).unwrap_or_default(),
        's' => f.subject.to_string(),
        'y' => f.label.unwrap_or_default().to_string(),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    }
}

pub fn render(fmt: &str, f: &IndexFields) -> String {
    render_with(fmt, &|spec| value_of(spec, f))
}

/// `render_with` plus mutt's `%>X`: everything after it is pushed to
/// the right edge of `width`, the gap filled with X (the status line
/// uses this; only the first `%>` counts).
pub fn render_status(fmt: &str, width: usize, value_of: &dyn Fn(char) -> String) -> String {
    let Some((left_fmt, rest)) = fmt.split_once("%>") else {
        return render_with(fmt, value_of);
    };
    let mut rest = rest.chars();
    let fill = rest.next().unwrap_or(' ');
    let left = render_with(left_fmt, value_of);
    let right = render_with(rest.as_str(), value_of);
    let used = left.chars().count() + right.chars().count();
    let gap = fill.to_string().repeat(width.saturating_sub(used));
    format!("{left}{gap}{right}")
}

/// The `%[-][min][.max]X` + `%?X?then&else?` machinery over any
/// specifier set; index lines and the status line share it. Unknown
/// specifiers should come back as `"%x"` to stay visible.
pub fn render_with(fmt: &str, value_of: &dyn Fn(char) -> String) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // %?X?then&else?, mutt's conditional on field X.
        if chars.peek() == Some(&'?') {
            chars.next();
            let Some(spec) = chars.next() else { break };
            if chars.peek() == Some(&'?') {
                chars.next();
            }
            let mut then_part = String::new();
            let mut else_part = String::new();
            let mut cur = &mut then_part;
            for c in chars.by_ref() {
                match c {
                    '&' => cur = &mut else_part,
                    '?' => break,
                    c => cur.push(c),
                }
            }
            let value = value_of(spec);
            let chosen = if value.trim().is_empty() || value.trim() == "0" {
                &else_part
            } else {
                &then_part
            };
            out.push_str(&render_with(chosen, value_of));
            continue;
        }
        let mut left = false;
        if chars.peek() == Some(&'-') {
            left = true;
            chars.next();
        }
        let mut min = 0usize;
        while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
            min = min * 10 + d as usize;
            chars.next();
        }
        let mut max = usize::MAX;
        if chars.peek() == Some(&'.') {
            chars.next();
            max = 0;
            while let Some(d) = chars.peek().and_then(|c| c.to_digit(10)) {
                max = max * 10 + d as usize;
                chars.next();
            }
        }
        let Some(spec) = chars.next() else { break };
        let value = value_of(spec);
        let mut v: Vec<char> = value.chars().collect();
        if v.len() > max {
            v.truncate(max);
        }
        let pad = min.saturating_sub(v.len());
        if left {
            out.extend(v);
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            out.extend(std::iter::repeat_n(' ', pad));
            out.extend(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_right_align_fills_the_width() {
        let v = |c: char| match c {
            'a' => "A".to_string(),
            'b' => "BB".to_string(),
            _ => String::new(),
        };
        assert_eq!(render_status("%a%>-%b", 8, &v), "A-----BB");
        // Space fill, mutt's usual "%> ".
        assert_eq!(render_status("%a%> %b", 6, &v), "A   BB");
        // Without %> it renders plainly, no padding.
        assert_eq!(render_status("%a %b", 8, &v), "A BB");
        // Too narrow: the gap just collapses.
        assert_eq!(render_status("%a%>-%b", 2, &v), "ABB");
    }

    fn fields() -> IndexFields<'static> {
        IndexFields {
            number: 7,
            status: 'N',
            flag: '!',
            mark: 'T',
            date: "Jul 06",
            from: "Jane Doe",
            size: "1.2K",
            lines: None,
            list: None,
            label: None,
            hidden: None,
            subject: "Lunch",
        }
    }

    #[test]
    fn default_format_matches_layout() {
        assert_eq!(
            render(DEFAULT_FORMAT, &fields()),
            "   7 N!T Jul 06 Jane Doe        (1.2K) Lunch"
        );
    }

    #[test]
    fn conditionals_and_list_alias() {
        let f = fields();
        // Unknown line count (header-only cache): the else branch wins.
        assert_eq!(render("(%?l?%4l&%5c?)", &f), "( 1.2K)");
        assert_eq!(render("%?s?have&none?", &f), "have");
        assert_eq!(render("%?l?lines?", &f), "");
        assert_eq!(render("%-10.10L|", &f), "Jane Doe  |");
        // A known count renders and satisfies the conditional.
        let counted = IndexFields {
            lines: Some(42),
            ..fields()
        };
        assert_eq!(render("(%?l?%4l&%5c?)", &counted), "(  42)");
        assert_eq!(render("%l", &counted), "42");
        // A zero-line body is "unset" for the conditional, like mutt.
        let empty = IndexFields {
            lines: Some(0),
            ..fields()
        };
        assert_eq!(render("%?l?%l&-?", &empty), "-");
        // %L prefers the mailing list over the author.
        let listed = IndexFields {
            list: Some("dev"),
            ..fields()
        };
        assert_eq!(render("%-10.10L|", &listed), "To dev    |");
        assert_eq!(render("%F", &listed), "Jane Doe");
    }

    #[test]
    fn width_precision_and_alignment() {
        let f = fields();
        assert_eq!(render("%-4.4F", &f), "Jane");
        assert_eq!(render("%10s", &f), "     Lunch");
        assert_eq!(render("%.3s", &f), "Lun");
        assert_eq!(render("100%% %C", &f), "100% 7");
    }

    #[test]
    fn unknown_specifier_is_kept_visibly() {
        assert_eq!(render("%q", &fields()), "%q");
    }
}
