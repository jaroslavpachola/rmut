//! The pager's rows and the index's scroll math: what a message looks
//! like as lines of text at a width, and where the cursor lands. No
//! toolkit; both front ends draw from this.

use rmut_core::message::MessageView;

pub fn humanize_size(bytes: u64) -> String {
    match bytes {
        0..=999 => format!("{bytes}"),
        1000..=10_239 => format!("{:.1}K", bytes as f64 / 1024.0),
        10_240..=1_048_575 => format!("{}K", bytes / 1024),
        1_048_576..=10_485_759 => format!("{:.1}M", bytes as f64 / 1_048_576.0),
        _ => format!("{}M", bytes / 1_048_576),
    }
}

// ---- pager ----

/// How one pager display row gets colored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RowKind {
    Header,
    /// `[-- ... --]` notices (PGP verdicts, missing parts).
    Marker,
    /// Quoted body text, 1-based nesting depth.
    Quoted(usize),
    Text,
}

pub struct Row {
    pub text: String,
    pub kind: RowKind,
}

/// Quote depth of a body line under $quote_regexp: the number of
/// quote characters in the prefix match, 0 for unquoted text.
pub fn quote_depth(line: &str, re: &regex_lite::Regex) -> usize {
    match re.find(line) {
        Some(m) if m.start() == 0 => m
            .as_str()
            .chars()
            .filter(|c| !c.is_whitespace())
            .count()
            .max(1),
        _ => 0,
    }
}

/// The pager display: header block, separator, wrapped body (with
/// mutt's `+` continuation markers), each row classified for
/// coloring. The scroll math, the body search, and the drawing all
/// share this; T (hide_quoted) drops quoted rows here, so every
/// consumer agrees on what a line number means.
/// What the config says about drawing a message: which lines count as
/// quoted, whether a wrapped line is marked, and whether it breaks at
/// a word.
pub struct PagerStyle<'a> {
    pub quote_re: &'a regex_lite::Regex,
    /// mutt's $markers.
    pub markers: bool,
    /// mutt's $smart_wrap.
    pub smart_wrap: bool,
}

impl<'a> PagerStyle<'a> {
    pub fn of(config: &'a rmut_core::config::Config, quote_re: &'a regex_lite::Regex) -> Self {
        PagerStyle {
            quote_re,
            markers: config.pager.markers.unwrap_or(true),
            smart_wrap: config.pager.smart_wrap.unwrap_or(true),
        }
    }
}

pub fn pager_rows(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    style: &PagerStyle,
    hide_quoted: bool,
) -> Vec<Row> {
    let quote_re = style.quote_re;
    let headers = if full_headers { &view.all } else { &view.brief };
    let mut rows: Vec<Row> = headers
        .iter()
        .map(|(name, value)| Row {
            text: format!("{name}: {value}"),
            kind: RowKind::Header,
        })
        .collect();
    rows.push(Row {
        text: String::new(),
        kind: RowKind::Text,
    });
    for line in view.body.lines() {
        // Marker lines like the PGP verdict get the header treatment.
        let marker = line.starts_with("[-- ") && line.ends_with(" --]");
        let depth = if marker {
            0
        } else {
            quote_depth(line, quote_re)
        };
        if hide_quoted && depth > 0 {
            continue;
        }
        let kind = if marker {
            RowKind::Marker
        } else if depth > 0 {
            RowKind::Quoted(depth)
        } else {
            RowKind::Text
        };
        for (i, wrapped) in wrap_line_with(line, width.saturating_sub(1), style.smart_wrap)
            .into_iter()
            .enumerate()
        {
            // mutt's $markers: a wrapped line says it is one.
            let text = match i > 0 && style.markers {
                true => format!("+{wrapped}"),
                false => wrapped,
            };
            rows.push(Row { text, kind });
        }
    }
    rows
}

/// The pager's display as plain text: what the body search runs over.
pub fn pager_text_lines(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    style: &PagerStyle,
    hide_quoted: bool,
) -> Vec<String> {
    pager_rows(view, width, full_headers, style, hide_quoted)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

/// Total pager lines at the given width.
pub fn pager_line_count(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    style: &PagerStyle,
    hide_quoted: bool,
) -> usize {
    pager_rows(view, width, full_headers, style, hide_quoted).len()
}

/// Word-wrap one body line to `width` columns (hard break when a single
/// word is longer than the line). Tabs are expanded first.
/// The same, with mutt's $smart_wrap: without it a long line breaks
/// at the column rather than at the last space before it.
pub fn wrap_line_with(line: &str, width: usize, smart: bool) -> Vec<String> {
    let width = width.max(4);
    let expanded = line.replace('\t', "    ");
    let chars: Vec<char> = expanded.chars().collect();
    if chars.len() <= width {
        return vec![expanded];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            out.push(chars[start..].iter().collect());
            break;
        }
        let window_end = start + width;
        let brk = match smart {
            true => (start + 1..window_end)
                .rev()
                .find(|&i| chars[i] == ' ')
                .unwrap_or(window_end),
            false => window_end,
        };
        out.push(chars[start..brk].iter().collect());
        start = if chars.get(brk) == Some(&' ') {
            brk + 1
        } else {
            brk
        };
    }
    out
}

/// mutt's $menu_scroll, $menu_context and $menu_move_off, as the
/// index's recentering reads them.
#[derive(Clone, Copy)]
pub struct Menu {
    pub scroll: bool,
    pub context: usize,
    pub move_off: bool,
}

/// Where the index's first row goes so the cursor stays on screen:
/// mutt's menu_check_recenter, line for line. `top` is the row now at
/// the top, `sel` the cursor, `rows` the screen, `max` the entries.
/// With `scroll` the view moves just far enough (keeping `context`
/// lines beyond the cursor); without it a whole page turns. Unless
/// `move_off`, the last entry never scrolls up past the bottom.
pub fn recenter(top: usize, sel: usize, rows: usize, max: usize, menu: Menu) -> usize {
    let (mut top, sel, rows, max) = (top as i64, sel as i64, rows as i64, max as i64);
    let c = (menu.context as i64).min(rows / 2);
    if !menu.move_off && max <= rows {
        top = 0;
    } else if menu.scroll || rows <= 0 || c < menu.context as i64 {
        if sel < top + c {
            top = sel - c;
        } else if sel >= top + rows - c {
            top = sel - rows + c + 1;
        }
    } else if sel < top + c {
        top -= (rows - c) * ((top + rows - 1 - sel) / (rows - c)) - c;
    } else if sel >= top + rows - c {
        top += (rows - c) * ((sel - top) / (rows - c)) - c;
    }
    if !menu.move_off {
        top = top.min(max - rows);
    }
    top.max(0) as usize
}

/// One pager line cut around its URLs: `(text, None)` runs and
/// `(url, Some(url))` spans, in order. Trailing sentence punctuation
/// stays outside the link, the way a URL at the end of a sentence
/// reads.
pub fn link_spans(line: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("http") {
        let candidate = &rest[at..];
        let scheme_ok = candidate.starts_with("http://") || candidate.starts_with("https://");
        if !scheme_ok {
            let cut = at + 4;
            let (head, tail) = rest.split_at(cut);
            out.push((head.to_string(), None));
            rest = tail;
            continue;
        }
        if at > 0 {
            out.push((rest[..at].to_string(), None));
        }
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ')' | ']'))
            .unwrap_or(candidate.len());
        let mut url = &candidate[..end];
        while let Some(stripped) = url.strip_suffix(['.', ',', ';', ':', '!', '?']) {
            url = stripped;
        }
        out.push((url.to_string(), Some(url.to_string())));
        rest = &candidate[url.len()..];
    }
    if !rest.is_empty() || out.is_empty() {
        out.push((rest.to_string(), None));
    }
    out
}

/// The pager's text search: the next line matching `m` from `from`,
/// wrapping around, and whether it wrapped. Both front ends step
/// their pagers with this.
pub fn search_lines(
    lines: &[String],
    m: &rmut_core::pattern::Matcher,
    from: usize,
    forward: bool,
) -> Option<(usize, bool)> {
    rmut_session::wrap_order(lines.len(), from, forward)
        .into_iter()
        .find(|&(idx, _)| m.is_match(&lines[idx]))
}

#[cfg(test)]
mod tests {
    use super::{
        Menu, PagerStyle, RowKind, humanize_size, pager_rows, quote_depth, recenter, wrap_line_with,
    };
    use rmut_core::message::MessageView;
    use rmut_session::default_quote_re;

    #[test]
    fn quote_depth_counts_prefix_marks() {
        let re = default_quote_re();
        assert_eq!(quote_depth("plain text", &re), 0);
        assert_eq!(quote_depth("> quoted", &re), 1);
        assert_eq!(quote_depth("> > deeper", &re), 2);
        assert_eq!(quote_depth(">>tight", &re), 2);
        assert_eq!(quote_depth("  | indented pipe", &re), 1);
        // A > later in the line is not a quote.
        assert_eq!(quote_depth("2 > 1", &re), 0);
    }

    #[test]
    fn rows_classify_and_hide_quoted() {
        let view = MessageView {
            brief: vec![("From".into(), "jane@example.com".into())],
            all: vec![("From".into(), "jane@example.com".into())],
            body: "top\n> one\n> > two\n[-- marker --]\ntail".into(),
        };
        let re = default_quote_re();
        let style = PagerStyle {
            quote_re: &re,
            markers: true,
            smart_wrap: true,
        };
        let rows = pager_rows(&view, 80, false, &style, false);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::Header,
                RowKind::Text, // separator
                RowKind::Text,
                RowKind::Quoted(1),
                RowKind::Quoted(2),
                RowKind::Marker,
                RowKind::Text,
            ]
        );
        // T drops the quoted rows for every consumer at once.
        let hidden = pager_rows(&view, 80, false, &style, true);
        assert_eq!(hidden.len(), rows.len() - 2);
        assert!(hidden.iter().all(|r| !matches!(r.kind, RowKind::Quoted(_))));
    }

    #[test]
    fn wrap_short_line_untouched() {
        assert_eq!(wrap_line_with("hello", 10, true), vec!["hello"]);
        assert_eq!(wrap_line_with("", 10, true), vec![""]);
    }

    #[test]
    fn wrap_breaks_at_word_boundary() {
        assert_eq!(
            wrap_line_with("the quick brown fox", 10, true),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn without_smart_wrap_a_line_breaks_at_the_column() {
        // mutt's $smart_wrap off: the break lands on the width, not
        // on the last space before it.
        assert_eq!(
            wrap_line_with("alpha beta gamma", 10, false),
            vec!["alpha beta", "gamma"]
        );
        assert_eq!(
            wrap_line_with("alpha beta gamma", 10, true),
            vec!["alpha", "beta gamma"]
        );
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(
            wrap_line_with("abcdefghij", 4, true),
            vec!["abcd", "efgh", "ij"]
        );
    }

    #[test]
    fn humanize_size_ranges() {
        assert_eq!(humanize_size(0), "0");
        assert_eq!(humanize_size(999), "999");
        assert_eq!(humanize_size(2048), "2.0K");
        assert_eq!(humanize_size(204800), "200K");
        assert_eq!(humanize_size(2 * 1024 * 1024), "2.0M");
    }

    #[test]
    fn recenter_scrolls_a_line_or_turns_a_page() {
        let scroll = Menu {
            scroll: true,
            context: 0,
            move_off: true,
        };
        let page = Menu {
            scroll: false,
            context: 0,
            move_off: true,
        };
        // Moving down off a 10-row screen: scrolling shows one more
        // line, paging turns the whole page (mutt's default).
        assert_eq!(recenter(0, 10, 10, 100, scroll), 1);
        assert_eq!(recenter(0, 10, 10, 100, page), 10);
        // Moving up off the top is symmetrical.
        assert_eq!(recenter(20, 19, 10, 100, scroll), 19);
        assert_eq!(recenter(20, 19, 10, 100, page), 10);
        // On screen already: nothing moves.
        assert_eq!(recenter(20, 25, 10, 100, scroll), 20);
        assert_eq!(recenter(20, 25, 10, 100, page), 20);
    }

    #[test]
    fn recenter_keeps_context_lines() {
        let m = Menu {
            scroll: true,
            context: 3,
            move_off: true,
        };
        // The cursor stays three rows clear of the bottom edge.
        assert_eq!(recenter(0, 7, 10, 100, m), 1);
        // And of the top edge.
        assert_eq!(recenter(20, 22, 10, 100, m), 19);
        // Context is capped at half the screen (a 4-row screen: 2).
        let big = Menu {
            scroll: true,
            context: 9,
            move_off: true,
        };
        assert_eq!(recenter(0, 2, 4, 100, big), 1);
    }

    #[test]
    fn recenter_move_off_pins_the_bottom() {
        let stuck = Menu {
            scroll: true,
            context: 0,
            move_off: false,
        };
        // Fewer entries than rows: the top is always the top.
        assert_eq!(recenter(3, 4, 10, 5, stuck), 0);
        // The last page stays full: top never passes max - rows.
        assert_eq!(recenter(95, 99, 10, 100, stuck), 90);
        // With move_off (the default) it may.
        let free = Menu {
            scroll: true,
            context: 0,
            move_off: true,
        };
        assert_eq!(recenter(95, 99, 10, 100, free), 95);
    }

    #[test]
    fn search_lines_steps_and_wraps() {
        use super::search_lines;
        use rmut_core::pattern::Matcher;
        let lines: Vec<String> = ["alpha", "the needle", "beta", "a Needle too"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = Matcher::new("needle");
        // Forward from the top: the next hit, no wrap; case-insensitive.
        assert_eq!(search_lines(&lines, &m, 0, true), Some((1, false)));
        assert_eq!(search_lines(&lines, &m, 1, true), Some((3, false)));
        // Past the last hit it wraps to the first.
        assert_eq!(search_lines(&lines, &m, 3, true), Some((1, true)));
        // Backwards, with and without the wrap.
        assert_eq!(search_lines(&lines, &m, 3, false), Some((1, false)));
        assert_eq!(search_lines(&lines, &m, 1, false), Some((3, true)));
        // No match, and the empty pager.
        assert_eq!(search_lines(&lines, &Matcher::new("zzz"), 0, true), None);
        assert_eq!(search_lines(&[], &m, 0, true), None);
        // A regex argument works like the patterns do.
        let re = Matcher::new("^bet.");
        assert_eq!(search_lines(&lines, &re, 0, true), Some((2, false)));
    }

    #[test]
    fn urls_cut_out_of_a_line() {
        use super::link_spans;
        let spans = link_spans("see https://example.com/x, then more");
        assert_eq!(
            spans,
            vec![
                ("see ".into(), None),
                (
                    "https://example.com/x".into(),
                    Some("https://example.com/x".into())
                ),
                (", then more".into(), None),
            ]
        );
        assert_eq!(
            link_spans("no links here"),
            vec![("no links here".into(), None)]
        );
        let wrapped = link_spans("(https://a.example) and <https://b.example>.");
        assert_eq!(wrapped[1].1.as_deref(), Some("https://a.example"));
        assert_eq!(wrapped[3].1.as_deref(), Some("https://b.example"));
        assert_eq!(
            link_spans("httpx is not a link"),
            vec![("http".into(), None), ("x is not a link".into(), None),]
        );
    }
}
