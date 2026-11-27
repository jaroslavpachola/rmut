//! What the copy actions put on the clipboard. The clipboard itself is
//! the front end's: OSC 52 in the terminal, the window's own in egui.

use rmut_core::message::{MessageView, Part};
use rmut_session::Session;
use std::path::Path;

/// copy-body: the body as the pager shows it (decoded, HTML rendered),
/// without the pager's own notices.
pub fn body(view: &MessageView) -> Result<String, String> {
    nonempty(crate::pager::body_text(view))
}

/// copy-body from the index: the selected message, loaded as the
/// pager would load it.
pub fn selected_body(session: &mut Session) -> Result<String, String> {
    let path = session.selected_path().ok_or("no message selected")?;
    let view = session
        .load_view(&path)
        .map_err(|err| format!("cannot open message: {err:#}"))?;
    body(&view)
}

/// `y` on the attachment menu: a text part decoded to text in its
/// charset, an HTML one rendered. Anything else is no text to paste.
pub fn part(
    session: &mut Session,
    path: &Path,
    index: usize,
    part: &Part,
) -> Result<String, String> {
    if !part.is_text {
        return Err(format!("{} is not text (s saves it)", part.mimetype));
    }
    let text = session
        .part_text(path, index)
        .map_err(|err| format!("cannot decode part: {err:#}"))?;
    nonempty(match part.mimetype.eq_ignore_ascii_case("text/html") {
        true => rmut_core::html::to_text(&text),
        false => text,
    })
}

/// The note after a copy: how much went, never the text itself.
pub fn note(text: &str) -> String {
    match text.lines().count() {
        1 => format!("copied 1 line, {} chars", text.chars().count()),
        n => format!("copied {n} lines"),
    }
}

/// The text as it goes out: Unix line ends (a decoded part keeps the
/// message's CRLF), and an empty one refused.
fn nonempty(text: String) -> Result<String, String> {
    let text = match text.contains('\r') {
        true => text.replace("\r\n", "\n"),
        false => text,
    };
    match text.trim().is_empty() {
        true => Err("nothing to copy".into()),
        false => Ok(text),
    }
}

#[cfg(test)]
mod tests {
    use super::{body, note};
    use rmut_core::message::MessageView;

    #[test]
    fn a_body_of_notices_only_is_nothing_to_copy() {
        let view = MessageView {
            brief: vec![],
            all: vec![],
            body: "[-- Attachment #1: plan.pdf --]\n".into(),
        };
        assert_eq!(body(&view), Err("nothing to copy".into()));
    }

    #[test]
    fn the_note_counts_rather_than_quotes() {
        assert_eq!(note("Hi Jane\n"), "copied 1 line, 8 chars");
        assert_eq!(note("one\ntwo\nthree\n"), "copied 3 lines");
    }
}
