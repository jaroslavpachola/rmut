//! Mutt-like index format strings: `%[-][min][.max]X` where X is
//! `C` number, `Z` status+flag chars, `d` date, `F` from, `c` size,
//! `s` subject, `%` a literal percent. `-` left-aligns, `min` pads,
//! `.max` truncates (all in characters).

pub const DEFAULT_FORMAT: &str = "%4C %Z %-6d %-20.20F %5c %s";

pub struct IndexFields<'a> {
    pub number: usize,
    pub status: char,
    pub flag: char,
    pub date: &'a str,
    pub from: &'a str,
    pub size: &'a str,
    pub subject: &'a str,
}

pub fn render(fmt: &str, f: &IndexFields) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
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
        let value = match spec {
            'C' => f.number.to_string(),
            'Z' => format!("{}{}", f.status, f.flag),
            'd' => f.date.to_string(),
            'F' => f.from.to_string(),
            'c' => f.size.to_string(),
            's' => f.subject.to_string(),
            '%' => "%".to_string(),
            other => format!("%{other}"),
        };
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
            "   7 N! Jul 06 Jane Doe              1.2K Lunch"
        );
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
