//! text/html into readable text, rmut's own few hundred lines: mutt
//! shows html-only mail as raw source unless a mailcap filter is
//! configured, and machines without lynx/w3m deserve better. This is
//! the fallback the pager uses by default (`[pager] html = "raw"`
//! restores mutt's literal behavior); a `[filters]`/mailcap
//! auto_view entry still wins, since it is consulted first.
//!
//! The shape of the output, not fidelity, is the goal: block
//! elements break lines, `<blockquote>` becomes `> ` prefixes so the
//! quote machinery works on html mail, links keep their targets as
//! `text <url>` so the window can click them, and everything inside
//! `<style>`/`<script>`/`<head>` disappears. Unknown tags are
//! ignored rather than feared; broken html must never panic.

/// The rendered text, one trailing newline, never more than one
/// blank line in a row.
pub fn to_text(html: &str) -> String {
    let mut out = Render::default();
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            None => {
                out.text(rest);
                break;
            }
            Some(i) => {
                out.text(&rest[..i]);
                rest = &rest[i..];
                match tag_end(rest) {
                    Some(end) => {
                        out.tag(&rest[..end]);
                        rest = &rest[end..];
                    }
                    None => {
                        // A stray '<' with no closing '>': literal.
                        out.text("<");
                        rest = &rest[1..];
                    }
                }
            }
        }
    }
    out.finish()
}

/// Where the tag starting at `s` (which begins with '<') ends, past
/// its '>': quotes protect '>', comments end at `-->`.
fn tag_end(s: &str) -> Option<usize> {
    if let Some(rest) = s.strip_prefix("<!--") {
        return rest.find("-->").map(|i| 4 + i + 3);
    }
    let bytes = s.as_bytes();
    let mut quote: Option<u8> = None;
    for (i, &b) in bytes.iter().enumerate().skip(1) {
        match quote {
            Some(q) => {
                if b == q {
                    quote = None;
                }
            }
            None => match b {
                b'"' | b'\'' => quote = Some(b),
                b'>' => return Some(i + 1),
                _ => {}
            },
        }
    }
    None
}

#[derive(Default)]
struct Render {
    out: String,
    /// `<blockquote>` depth: each line starts with this many `> `.
    quote: usize,
    /// Open lists: an `<ol>` counts its items, a `<ul>` does not.
    lists: Vec<Option<usize>>,
    /// `<pre>` depth: whitespace kept verbatim inside.
    pre: usize,
    /// Inside `<style>`/`<script>`/`<head>`...: text is dropped
    /// until this tag closes (the innermost such tag).
    skip: Vec<String>,
    /// Open `<a href>` targets, said after their anchor text.
    links: Vec<(String, usize)>,
    /// A collapsed space is owed before the next word.
    space: bool,
    /// A blank line is owed before the next text (paragraph break).
    blank: bool,
}

/// Elements whose content is machinery, not prose.
const SKIPPED: &[&str] = &[
    "style", "script", "head", "title", "template", "svg", "noscript",
];

/// Elements that break the line without a blank (a row, a cell run).
const LINE_BREAKERS: &[&str] = &[
    "div", "tr", "section", "article", "aside", "main", "figure", "nav", "form", "header", "footer",
];

/// Elements that earn a blank line around them.
const PARA_BREAKERS: &[&str] = &[
    "p",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "ul",
    "ol",
    "pre",
    "blockquote",
];

impl Render {
    fn text(&mut self, s: &str) {
        if !self.skip.is_empty() {
            return;
        }
        if self.pre > 0 {
            let decoded = decode_entities(s);
            for (i, line) in decoded.split('\n').enumerate() {
                if i > 0 {
                    self.newline();
                }
                self.raw(line);
            }
            return;
        }
        if s.starts_with(char::is_whitespace) {
            self.space = true;
        }
        for piece in s.split_whitespace() {
            if self.space && !self.at_line_start() {
                self.raw(" ");
            }
            let word = decode_entities(piece);
            self.raw(&word);
            self.space = true;
        }
        // Trailing whitespace in the run keeps the pending space for
        // the next run ("a <b>b</b>" has a space, "a<b>b</b>" not).
        if !s.is_empty() {
            self.space = s.ends_with(char::is_whitespace) || self.space;
            if !s.trim().is_empty() && !s.ends_with(char::is_whitespace) {
                self.space = false;
            }
        }
    }

    fn tag(&mut self, tag: &str) {
        if tag.starts_with("<!") || tag.starts_with("<?") {
            return;
        }
        let inner = tag.trim_start_matches('<').trim_end_matches('>');
        let closing = inner.starts_with('/');
        let inner = inner.trim_start_matches('/');
        let name: String = inner
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_lowercase();
        if name.is_empty() {
            return;
        }
        // Inside a skipped element, only its own closing tag matters.
        if let Some(top) = self.skip.last() {
            if closing && *top == name {
                self.skip.pop();
            }
            return;
        }
        if SKIPPED.contains(&name.as_str()) {
            if !closing && !inner.ends_with('/') {
                self.skip.push(name);
            }
            return;
        }
        match (name.as_str(), closing) {
            ("br", _) => self.newline(),
            ("hr", _) => {
                self.want_blank();
                self.flush_break();
                self.raw("--");
                self.want_blank();
            }
            ("blockquote", false) => {
                self.want_blank();
                self.quote += 1;
            }
            ("blockquote", true) => {
                self.quote = self.quote.saturating_sub(1);
                self.want_blank();
            }
            ("ul", false) => {
                self.want_blank();
                self.lists.push(None);
            }
            ("ol", false) => {
                self.want_blank();
                self.lists.push(Some(0));
            }
            ("ul", true) | ("ol", true) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.want_blank();
                }
            }
            ("li", false) => {
                self.fresh_line();
                let depth = self.lists.len().saturating_sub(1);
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{n}. ")
                    }
                    _ => "- ".to_string(),
                };
                self.raw(&"  ".repeat(depth));
                self.raw(&marker);
                self.space = false;
            }
            ("pre", false) => {
                self.want_blank();
                self.flush_break();
                self.pre += 1;
            }
            ("pre", true) => {
                self.pre = self.pre.saturating_sub(1);
                self.want_blank();
            }
            ("a", false) => {
                let href = attr(inner, "href").unwrap_or_default();
                self.links.push((href, self.out.len()));
            }
            ("a", true) => {
                if let Some((href, start)) = self.links.pop() {
                    let text = &self.out[start.min(self.out.len())..];
                    if worth_showing(&href, text) {
                        let href = href.clone();
                        self.text(&format!(" <{href}>"));
                    }
                }
            }
            ("img", _) => {
                if let Some(alt) = attr(inner, "alt")
                    && !alt.trim().is_empty()
                {
                    let alt = alt.clone();
                    self.text(&format!("[{alt}]"));
                }
            }
            ("td", false) | ("th", false) => {
                if !self.at_line_start() {
                    self.raw("  ");
                    self.space = false;
                }
            }
            (n, _) if PARA_BREAKERS.contains(&n) => self.want_blank(),
            (n, _) if LINE_BREAKERS.contains(&n) => self.fresh_line(),
            _ => {}
        }
    }

    /// Text goes through here so line starts get their quote prefix
    /// and an owed paragraph break lands first.
    fn raw(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        self.flush_break();
        if self.at_line_start() && self.quote > 0 {
            let prefix = "> ".repeat(self.quote);
            self.out.push_str(&prefix);
        }
        self.out.push_str(s);
    }

    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n')
    }

    /// A hard line break (`<br>`): breaks even at a line start, so
    /// two of them make a blank line.
    fn newline(&mut self) {
        self.flush_break();
        self.out.push('\n');
        self.space = false;
    }

    /// Break to a fresh line without stacking: a `</tr><tr>` pair or
    /// a list item wants one break, however many tags said so.
    fn fresh_line(&mut self) {
        self.flush_break();
        if !self.at_line_start() {
            self.out.push('\n');
        }
        self.space = false;
    }

    fn want_blank(&mut self) {
        if !self.out.is_empty() {
            self.blank = true;
        }
        self.space = false;
    }

    /// An owed paragraph break becomes at most one blank line.
    fn flush_break(&mut self) {
        if std::mem::take(&mut self.blank) {
            while !self.out.is_empty() && !self.out.ends_with("\n\n") {
                self.out.push('\n');
            }
        }
    }

    fn finish(mut self) -> String {
        let mut text: String = self
            .out
            .split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        while text.ends_with('\n') {
            text.pop();
        }
        let trimmed = text.trim_start_matches('\n').to_string();
        self.out = trimmed;
        if self.out.is_empty() {
            return String::new();
        }
        self.out.push('\n');
        self.out
    }
}

/// A link target worth saying after its anchor text: not empty, not
/// a fragment or script, and not already the text itself.
fn worth_showing(href: &str, text: &str) -> bool {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.to_lowercase().starts_with("javascript:") {
        return false;
    }
    let text = text.trim();
    !text.contains(href.trim_end_matches('/'))
}

/// The value of `name=` in a tag's innards, quotes stripped.
fn attr(inner: &str, name: &str) -> Option<String> {
    let lower = inner.to_lowercase();
    let mut from = 0;
    loop {
        let i = lower[from..].find(name)? + from;
        // A whole attribute name, not the tail of another.
        let clean_start = i == 0
            || !lower.as_bytes()[i - 1].is_ascii_alphanumeric() && lower.as_bytes()[i - 1] != b'-';
        let after = &inner[i + name.len()..];
        let after_trim = after.trim_start();
        if clean_start && after_trim.starts_with('=') {
            let value = after_trim[1..].trim_start();
            let value = match value.as_bytes().first() {
                Some(b'"') => value[1..].split('"').next().unwrap_or(""),
                Some(b'\'') => value[1..].split('\'').next().unwrap_or(""),
                _ => value.split_whitespace().next().unwrap_or(""),
            };
            return Some(decode_entities(value));
        }
        from = i + name.len();
        if from >= lower.len() {
            return None;
        }
    }
}

/// `&amp;` and friends, plus numeric `&#123;` / `&#x1f;`. An entity
/// this table does not know stays literal.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let semi = rest[..rest.len().min(32)].find(';');
        let Some(semi) = semi else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..semi];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "copy" => Some('\u{a9}'),
            "reg" => Some('\u{ae}'),
            "trade" => Some('\u{2122}'),
            "mdash" => Some('\u{2014}'),
            "ndash" => Some('\u{2013}'),
            "hellip" => Some('\u{2026}'),
            "lsquo" => Some('\u{2018}'),
            "rsquo" => Some('\u{2019}'),
            "ldquo" => Some('\u{201c}'),
            "rdquo" => Some('\u{201d}'),
            "bull" => Some('\u{2022}'),
            "middot" => Some('\u{b7}'),
            "laquo" => Some('\u{ab}'),
            "raquo" => Some('\u{bb}'),
            "deg" => Some('\u{b0}'),
            "times" => Some('\u{d7}'),
            "euro" => Some('\u{20ac}'),
            "pound" => Some('\u{a3}'),
            "shy" | "zwnj" | "zwj" | "lrm" | "rlm" => Some('\u{0}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some('\u{0}') => rest = &rest[semi + 1..],
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_and_headings_break_lines() {
        let text = to_text("<html><body><h1>Title</h1><p>one two</p>\n<p>three</p></body></html>");
        assert_eq!(text, "Title\n\none two\n\nthree\n");
    }

    #[test]
    fn style_script_and_head_disappear() {
        let text = to_text(
            "<head><title>t</title><style>p { color: red }</style></head>\
             <body><script>alert(1)</script><p>kept</p></body>",
        );
        assert_eq!(text, "kept\n");
    }

    #[test]
    fn entities_decode_and_unknown_stay() {
        let text = to_text("<p>Hello &amp; goodbye &lt;3 &#65; &#x42; &unknown; &rsquo;</p>");
        assert_eq!(text, "Hello & goodbye <3 A B &unknown; \u{2019}\n");
    }

    #[test]
    fn links_keep_their_targets() {
        let text = to_text("<p>see <a href=\"https://example.com/x\">the docs</a> now</p>");
        assert_eq!(text, "see the docs <https://example.com/x> now\n");
        // The text already saying the target says it once.
        let text = to_text("<a href=\"https://example.com\">https://example.com</a>");
        assert_eq!(text, "https://example.com\n");
        // Fragments and scripts are not links worth words.
        let text = to_text("<a href=\"#top\">up</a> <a href=\"javascript:x()\">no</a>");
        assert_eq!(text, "up no\n");
    }

    #[test]
    fn blockquotes_become_quote_prefixes() {
        let text = to_text(
            "<p>said:</p><blockquote>first<br>second\
             <blockquote>deeper</blockquote></blockquote><p>after</p>",
        );
        assert_eq!(text, "said:\n\n> first\n> second\n\n> > deeper\n\nafter\n");
    }

    #[test]
    fn lists_get_markers_and_numbers() {
        let text = to_text("<ul><li>one</li><li>two<ol><li>a</li><li>b</li></ol></li></ul>");
        assert_eq!(text, "- one\n- two\n\n  1. a\n  2. b\n");
    }

    #[test]
    fn pre_keeps_its_whitespace() {
        let text = to_text("<p>code:</p><pre>  indented\n    more</pre>");
        assert_eq!(text, "code:\n\n  indented\n    more\n");
    }

    #[test]
    fn tables_space_their_cells() {
        let text =
            to_text("<table><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>");
        assert_eq!(text, "a  b\n1  2\n");
    }

    #[test]
    fn images_say_their_alt_text() {
        let text = to_text("<p><img src=\"cid:x\" alt=\"a chart\"> and <img src=\"y\"></p>");
        assert_eq!(text, "[a chart] and\n");
    }

    #[test]
    fn broken_html_never_panics() {
        for bad in [
            "<",
            "<p",
            "<a href=\"unclosed>text",
            "</closes><nothing",
            "<p>&#xffffffff; &#; &",
            "<blockquote></blockquote></blockquote>",
            "<!-- unterminated",
            "text < 3 and > 2",
        ] {
            let _ = to_text(bad);
        }
        assert_eq!(to_text("a < b"), "a < b\n");
    }
}
