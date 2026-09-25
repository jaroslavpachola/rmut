//! Markdown compose: the text/html half of a draft written in
//! markdown, the text/plain half being the draft as typed.
//!
//! Mail is not a web page, so three things differ from rendering a
//! README. Raw HTML in the draft is shown as the text it is, since
//! "a <b> c" in a mail is prose, not markup. A bare URL becomes a link,
//! the way a reader of the plain half would take it. And the
//! signature, everything below the "-- " line, keeps its lines as
//! typed instead of being run together into a paragraph (or read as
//! a heading underline).

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

use crate::links::link_spans;

/// The whole text/html document for a markdown body.
pub fn to_html(body: &str) -> String {
    let (text, signature) = split_signature(body);
    let mut out =
        String::from("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"></head><body>\n");
    out += &render(text);
    if let Some(sig) = signature {
        out += "<p class=\"signature\">-- ";
        for line in sig.lines() {
            out += "<br>\n";
            out += &escape(line);
        }
        out += "</p>\n";
    }
    out += "</body></html>\n";
    out
}

/// The body above the signature separator, and the signature below it.
fn split_signature(body: &str) -> (&str, Option<&str>) {
    if let Some(sig) = body.strip_prefix("-- \n") {
        return ("", Some(sig));
    }
    match body.rfind("\n-- \n") {
        Some(at) => (&body[..at + 1], Some(&body[at + 5..])),
        None => (body, None),
    }
}

/// The markdown itself, CommonMark plus tables, strikethrough and task
/// lists.
fn render(text: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut events = Vec::new();
    // Inside a link or code the text stays as it is.
    let mut plain = 0usize;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag @ (Tag::Link { .. } | Tag::CodeBlock(_) | Tag::Image { .. })) => {
                plain += 1;
                events.push(Event::Start(tag));
            }
            Event::End(end @ (TagEnd::Link | TagEnd::CodeBlock | TagEnd::Image)) => {
                plain = plain.saturating_sub(1);
                events.push(Event::End(end));
            }
            Event::Html(raw) | Event::InlineHtml(raw) => events.push(Event::Text(raw)),
            Event::Text(t) if plain == 0 && t.contains("http") => {
                for (piece, url) in link_spans(&t) {
                    events.push(match url {
                        Some(url) => Event::InlineHtml(CowStr::from(format!(
                            "<a href=\"{}\">{}</a>",
                            escape(&url),
                            escape(&piece)
                        ))),
                        None => Event::Text(CowStr::from(piece)),
                    });
                }
            }
            other => events.push(other),
        }
    }
    let mut out = String::new();
    html::push_html(&mut out, events.into_iter());
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders_and_mail_stays_mail() {
        let html = to_html(
            "Hi *Jane*,\n\n\
             the **plan**:\n\n\
             - one\n- two\n\n\
             > quoted from you\n\n\
             see https://example.com/x. and a <b> c\n\n\
             ```\nlet x = \"https://not.a.link\";\n```\n\
             -- \nAlex\nACME Ltd\n",
        );
        assert!(html.contains("<em>Jane</em>"), "{html}");
        assert!(html.contains("<strong>plan</strong>"), "{html}");
        assert!(html.contains("<li>one</li>"), "{html}");
        assert!(html.contains("<blockquote>"), "{html}");
        assert!(
            html.contains("<a href=\"https://example.com/x\">https://example.com/x</a>."),
            "a bare URL is a link, the full stop outside it: {html}"
        );
        assert!(html.contains("a &lt;b&gt; c"), "raw html is text: {html}");
        assert!(
            html.contains("let x = \"https://not.a.link\";")
                && !html.contains("href=\"https://not"),
            "code stays code: {html}"
        );
        assert!(
            html.contains("<p class=\"signature\">-- <br>\nAlex<br>\nACME Ltd</p>"),
            "the signature keeps its lines: {html}"
        );
        assert!(!html.contains("<h2>"), "the -- line is no heading: {html}");
    }

    #[test]
    fn a_link_written_as_markdown_is_not_linked_twice() {
        let html = to_html("[the docs](https://example.com/docs)\n");
        assert_eq!(html.matches("<a ").count(), 1, "{html}");
    }
}
