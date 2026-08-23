//! RFC 6068 `mailto:` URLs, for `rmut mailto:...` as a desktop mail
//! handler. Only the headers a compose window can honor are read
//! (to, cc, bcc, subject, body); anything else is ignored rather than
//! smuggled into the draft, since a URL is untrusted input.

/// The pieces of a `mailto:` URL that reach a draft.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Mailto {
    pub to: String,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    pub subject: String,
    pub body: String,
}

/// Parse a `mailto:` URL. Not one is `None`; a malformed query simply
/// contributes nothing, which is what a mail handler should do rather
/// than refusing to open.
pub fn parse(url: &str) -> Option<Mailto> {
    let rest = url
        .strip_prefix("mailto:")
        .or_else(|| url.strip_prefix("MAILTO:"))?;
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    let mut out = Mailto {
        to: decode(path).trim().to_string(),
        ..Default::default()
    };
    let mut extra_to: Vec<String> = Vec::new();
    for pair in query.into_iter().flat_map(|q| q.split('&')) {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let value = decode(value);
        // Header names are case-insensitive in a mailto URL.
        match decode(name).to_ascii_lowercase().as_str() {
            "to" if !value.trim().is_empty() => extra_to.push(value),
            "cc" => out.cc = non_empty(value),
            "bcc" => out.bcc = non_empty(value),
            "subject" => out.subject = one_line(&value),
            "body" => out.body = value,
            _ => {}
        }
    }
    for more in extra_to {
        if out.to.is_empty() {
            out.to = more;
        } else {
            out.to = format!("{}, {more}", out.to);
        }
    }
    Some(out)
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

/// A header value cannot carry newlines: folding them away keeps a
/// crafted URL from injecting extra headers into the draft.
fn one_line(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

/// Percent-decoding, with `+` for a space as browsers write it.
/// Invalid escapes stay literal instead of being dropped.
fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    None => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_address() {
        let m = parse("mailto:jane@example.com").unwrap();
        assert_eq!(m.to, "jane@example.com");
        assert_eq!(m.subject, "");
        assert_eq!(m.body, "");
    }

    #[test]
    fn query_headers_and_escapes() {
        let m = parse(
            "mailto:jane@example.com?subject=Lunch%20on%20Friday&cc=petr@example.com\
             &body=Hi%20Jane%2C%0Aare%20you%20free%3F",
        )
        .unwrap();
        assert_eq!(m.to, "jane@example.com");
        assert_eq!(m.subject, "Lunch on Friday");
        assert_eq!(m.cc.as_deref(), Some("petr@example.com"));
        assert_eq!(m.body, "Hi Jane,\nare you free?");
        assert_eq!(m.bcc, None);
    }

    #[test]
    fn several_recipients_join() {
        let m = parse("mailto:a@x,b@x?to=c@x").unwrap();
        assert_eq!(m.to, "a@x,b@x, c@x");
        let m = parse("mailto:?to=only@x").unwrap();
        assert_eq!(m.to, "only@x");
    }

    #[test]
    fn plus_is_a_space_and_bad_escapes_stay() {
        let m = parse("mailto:a@x?subject=one+two%zz%2").unwrap();
        assert_eq!(m.subject, "one two%zz%2");
    }

    #[test]
    fn a_crafted_subject_cannot_add_headers() {
        let m = parse("mailto:a@x?subject=hi%0ABcc:%20evil@x").unwrap();
        assert!(!m.subject.contains('\n'), "{:?}", m.subject);
        assert_eq!(m.subject, "hi Bcc: evil@x");
        assert_eq!(m.bcc, None);
    }

    #[test]
    fn not_a_mailto() {
        assert_eq!(parse("https://example.com"), None);
        assert_eq!(parse("/home/jarda/Maildir"), None);
    }
}
