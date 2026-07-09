//! Mutt-like index format strings: `%[-][min][.max]X` where X is
//! `C` number, `Z` status/flag/mark chars, `d` date, `F` from (`L` is
//! its list-aware alias), `c` size, `l` lines (unknown, always empty),
//! `s` subject, `%` a literal percent. `-` left-aligns, `min` pads,
//! `.max` truncates (all in characters). Mutt conditionals work too:
//! `%?X?then&else?` renders `then` when field X is set and non-zero.

pub const DEFAULT_FORMAT: &str = "%4C %Z %-6d %-20.20F %5c %s";

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
    pub subject: &'a str,
}

/// The raw text of one specifier, for both rendering and the
/// conditional set/unset test.
fn value_of(spec: char, f: &IndexFields) -> String {
    match spec {
        'C' => f.number.to_string(),
        'Z' => format!("{}{}{}", f.status, f.flag, f.mark),
        'd' => f.date.to_string(),
        'F' | 'L' => f.from.to_string(),
        'c' => f.size.to_string(),
        // Line counts are not tracked; conditionals on %l therefore
        // take their else branch (usually %c).
        'l' => String::new(),
        's' => f.subject.to_string(),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    }
}

pub fn render(fmt: &str, f: &IndexFields) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        // %?X?then&else? — mutt conditional on field X.
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
            let value = value_of(spec, f);
            let chosen = if value.trim().is_empty() || value.trim() == "0" {
                &else_part
            } else {
                &then_part
            };
            out.push_str(&render(chosen, f));
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
        let value = value_of(spec, f);
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

    fn fields() -> IndexFields<'static> {
        IndexFields {
            number: 7,
            status: 'N',
            flag: '!',
            mark: 'T',
            date: "Jul 06",
            from: "Jane Doe",
            size: "1.2K",
            subject: "Lunch",
        }
    }

    #[test]
    fn default_format_matches_layout() {
        assert_eq!(
            render(DEFAULT_FORMAT, &fields()),
            "   7 N!T Jul 06 Jane Doe              1.2K Lunch"
        );
    }

    #[test]
    fn conditionals_and_list_alias() {
        let f = fields();
        // %l is never known: the else branch (size) wins.
        assert_eq!(render("(%?l?%4l&%5c?)", &f), "( 1.2K)");
        assert_eq!(render("%?s?have&none?", &f), "have");
        assert_eq!(render("%?l?lines?", &f), "");
        assert_eq!(render("%-10.10L|", &f), "Jane Doe  |");
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
