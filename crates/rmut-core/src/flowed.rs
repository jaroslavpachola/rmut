//! RFC 3676 `text/plain; format=flowed`.
//!
//! Incoming: [`unflow`] puts a paragraph the sender split across
//! several flowed lines back on one logical line, keeping the quote
//! depth, so the pager's own wrapper lays it out at the display width
//! instead of leaving the sender's line breaks in place.
//!
//! Outgoing: [`space_stuff`] is the sender's half of the format, the
//! leading space RFC 3676 puts on a line that would otherwise look
//! like a quote or a `From ` line.

/// A line ending in a space continues the paragraph, except the
/// signature separator, which RFC 3676 §4.3 keeps fixed.
const SIG_SEPARATOR: &str = "-- ";

/// Leading `>` run (the quote depth) and the rest of the line. RFC
/// 3676 quote marks sit at the very start with nothing between them.
fn split_quote(line: &str) -> (usize, &str) {
    let depth = line.bytes().take_while(|&b| b == b'>').count();
    (depth, &line[depth..])
}

/// Reflow a flowed body into one line per paragraph. `delsp` is the
/// DelSp parameter: with it the space that marked a line as flowed is
/// dropped on joining, without it (the default) it is part of the
/// text. Quote depth survives as `>` marks, so the pager still colours
/// quoted text and can still fold it.
pub fn unflow(text: &str, delsp: bool) -> String {
    let mut out = String::new();
    // The paragraph being collected: its quote depth and text so far.
    let mut open: Option<(usize, String)> = None;
    for raw in text.lines() {
        let (depth, rest) = split_quote(raw);
        // Space-stuffing is the sender's, not part of the text.
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        let flowed = rest.ends_with(' ') && rest != SIG_SEPARATOR;
        let content = match flowed && delsp {
            true => &rest[..rest.len() - 1],
            false => rest,
        };
        match &mut open {
            // Same depth: the paragraph goes on.
            Some((d, buf)) if *d == depth => buf.push_str(content),
            // A depth change ends the paragraph, whatever the spaces.
            Some(_) => {
                flush(&mut out, open.take());
                open = Some((depth, content.to_string()));
            }
            None => open = Some((depth, content.to_string())),
        }
        if !flowed {
            flush(&mut out, open.take());
        }
    }
    flush(&mut out, open.take());
    out
}

fn flush(out: &mut String, paragraph: Option<(usize, String)>) {
    let Some((depth, text)) = paragraph else {
        return;
    };
    for _ in 0..depth {
        out.push('>');
    }
    if depth > 0 && !text.is_empty() {
        out.push(' ');
    }
    out.push_str(&text);
    out.push('\n');
}

/// RFC 3676 space-stuffing: a line starting with a space, a `>` or
/// `From ` gets one more space in front, so a reader can tell the
/// sender's text from the format's own marks. Undone by [`unflow`] at
/// the other end.
pub fn space_stuff(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('>') || line.starts_with("From ") {
            out.push(' ');
        }
        out.push_str(line);
        out.push('\n');
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraphs_join_and_fixed_lines_stay() {
        let text = "One flowed \nparagraph here.\n\nA fixed line.\n";
        assert_eq!(
            unflow(text, false),
            "One flowed paragraph here.\n\nA fixed line.\n"
        );
    }

    #[test]
    fn delsp_drops_the_marking_space() {
        assert_eq!(unflow("half \nway\n", true), "halfway\n");
        assert_eq!(unflow("half \nway\n", false), "half way\n");
    }

    #[test]
    fn quote_depth_bounds_a_paragraph() {
        let text = "> quoted and \n> continued\n>> deeper \n>> line\nmine\n";
        assert_eq!(
            unflow(text, false),
            "> quoted and continued\n>> deeper line\nmine\n"
        );
    }

    #[test]
    fn stuffing_is_undone_and_the_signature_stays_fixed() {
        // A stuffed line: the leading space is the format's, not text.
        assert_eq!(unflow("  indented\n", false), " indented\n");
        assert_eq!(unflow(" >not a quote\n", false), ">not a quote\n");
        // "-- " ends the paragraph despite the trailing space.
        assert_eq!(unflow("text\n-- \nJane\n", false), "text\n-- \nJane\n");
    }

    #[test]
    fn stuffing_marks_the_lines_that_need_it() {
        assert_eq!(
            space_stuff(" lead\n>quote\nFrom here\nplain\n"),
            "  lead\n >quote\n From here\nplain\n"
        );
        assert_eq!(space_stuff("no newline"), "no newline");
    }
}
