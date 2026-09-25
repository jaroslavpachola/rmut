//! URLs in running text: where the pager draws its links, what the
//! link list offers, and what markdown compose turns into anchors all
//! agree on what a URL is.

/// One line cut around its URLs: `(text, None)` runs and
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_cut_out_of_a_line() {
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
