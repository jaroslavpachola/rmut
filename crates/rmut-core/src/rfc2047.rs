//! Header values decoded the way mutt decodes them: mutt's
//! `rfc2047_decode` rather than mailparse's `get_value`.
//!
//! The two differ where the mail is out of spec, which real mail often
//! is. mailparse follows RFC 2047 strictly and only takes `=?...?=` as
//! an encoded word when whitespace (or a quote, a bracket, a comma)
//! stands either side of it, so `=?utf-8?Q?Nov=C3=BD_?=text` stays raw;
//! mutt deliberately drops that rule (its `find_encoded_word`: "we
//! don't require the encoded word to be separated by
//! linear-white-space"). mailparse also converts each encoded word to
//! text on its own, so a UTF-8 character a sender's encoder split
//! across two words decodes to two U+FFFD; mutt gathers the bytes of
//! adjacent words in one charset and converts them together.

use std::borrow::Cow;

use mailparse::{MailHeader, MailHeaderMap};

/// The first `name` header, unfolded and decoded.
pub fn first<M: MailHeaderMap + ?Sized>(headers: &M, name: &str) -> Option<String> {
    headers.get_first_header(name).map(value)
}

/// Every `name` header, each unfolded and decoded.
pub fn all<M: MailHeaderMap + ?Sized>(headers: &M, name: &str) -> Vec<String> {
    headers
        .get_all_headers(name)
        .into_iter()
        .map(value)
        .collect()
}

/// One header's value, unfolded and decoded.
pub fn value(header: &MailHeader) -> String {
    decode(&unfold(header.get_value_raw()))
}

/// The raw value as text on one line, the way mutt's
/// `mutt_read_rfc822_line` reads it: each line's trailing whitespace
/// dropped, a continuation line's leading whitespace with it, the two
/// joined by one space. Bytes that are not UTF-8 are read as Latin-1,
/// as mailparse reads them.
fn unfold(raw: &[u8]) -> String {
    let text = match std::str::from_utf8(raw) {
        Ok(s) => Cow::Borrowed(s),
        Err(_) => Cow::Owned(raw.iter().map(|&b| char::from(b)).collect()),
    };
    let mut out = String::with_capacity(text.len());
    let wsp = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    for (n, line) in text.lines().enumerate() {
        if n > 0 {
            out.push(' ');
        }
        out.push_str(line.trim_start_matches(wsp).trim_end_matches(wsp));
    }
    out
}

/// mutt's `rfc2047_decode`, without `$ignore_linear_white_space` and
/// `$assumed_charset`, which rmut does not have. Text between encoded
/// words that is only whitespace is dropped; the decoded bytes of
/// adjacent words in one charset are converted together.
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut acc: Vec<u8> = Vec::new();
    let mut acc_charset: Option<String> = None;
    let mut found = false;
    let mut pos = 0;
    while let Some((begin, end)) = find_encoded_word(bytes, pos) {
        // Both ends sit on ASCII, so these slices are on char boundaries.
        let text = &s[pos..begin];
        if !text.is_empty() && (!found || !text.bytes().all(|b| b" \t\r\n".contains(&b))) {
            flush(&mut out, &mut acc, &mut acc_charset);
            out.push_str(text);
        }
        match decode_word(&s[begin..end]) {
            Some((charset, word)) => {
                let same = acc_charset
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(&charset));
                if !same {
                    flush(&mut out, &mut acc, &mut acc_charset);
                }
                acc.extend_from_slice(&word);
                acc_charset = Some(charset);
            }
            None => {
                flush(&mut out, &mut acc, &mut acc_charset);
                out.push_str(&s[begin..end]);
            }
        }
        found = true;
        pos = end;
    }
    flush(&mut out, &mut acc, &mut acc_charset);
    out.push_str(&s[pos..]);
    out
}

/// mutt's `convert_and_add_word`: the gathered bytes, converted from
/// their charset. A charset nothing knows leaves the bytes to be read
/// as UTF-8, which is what mutt's failed iconv leaves too.
fn flush(out: &mut String, acc: &mut Vec<u8>, charset: &mut Option<String>) {
    if let Some(label) = charset.take() {
        match charset::Charset::for_label_no_replacement(label.as_bytes()) {
            Some(cs) => out.push_str(&cs.decode_without_bom_handling(acc).0),
            None => out.push_str(&String::from_utf8_lossy(acc)),
        }
    }
    acc.clear();
}

/// mutt's `find_encoded_word`: the grammar of RFC 2047 section 2 with
/// B or Q for the encoding, a text that may hold spaces and question
/// marks, and nothing asked of what stands either side. Returns the
/// word's start and the index just past its `?=`.
fn find_encoded_word(s: &[u8], from: usize) -> Option<(usize, usize)> {
    const ESPECIALS: &[u8] = b"()<>@,;:\"/[]?.=";
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let mut q = from;
    while let Some(p) = find(s, q, b"=?") {
        q = p + 2;
        while (0x21..0x7f).contains(&at(q)) && !ESPECIALS.contains(&at(q)) {
            q += 1;
        }
        if at(q) != b'?' || !b"BbQq".contains(&at(q + 1)) || at(q + 2) != b'?' {
            continue;
        }
        q += 3;
        while (0x20..0x7f).contains(&at(q)) && !(at(q) == b'?' && at(q + 1) == b'=') {
            q += 1;
        }
        if at(q) != b'?' || at(q + 1) != b'=' {
            q -= 1;
            continue;
        }
        return Some((p, q + 2));
    }
    None
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| from + i)
}

/// mutt's `rfc2047_decode_word` on a word `find_encoded_word` found:
/// its charset (an RFC 2231 `*language` dropped) and its bytes.
fn decode_word(word: &str) -> Option<(String, Vec<u8>)> {
    let inner = word.strip_prefix("=?")?.strip_suffix("?=")?;
    let (charset, rest) = inner.split_once('?')?;
    let (encoding, text) = rest.split_once('?')?;
    let charset = charset.split('*').next().unwrap_or_default().to_string();
    let bytes = match encoding {
        "Q" | "q" => decode_q(text.as_bytes()),
        "B" | "b" => decode_b(text.as_bytes())?,
        _ => return None,
    };
    Some((charset, bytes))
}

/// Q: `_` a space, `=XX` a byte, anything else itself.
fn decode_q(text: &[u8]) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let pair = (
            text.get(i + 1).copied().and_then(hex),
            text.get(i + 2).copied().and_then(hex),
        );
        match (text[i], pair) {
            (b'_', _) => out.push(b' '),
            (b'=', (Some(h), Some(l))) => {
                out.push(h << 4 | l);
                i += 2;
            }
            (b, _) => out.push(b),
        }
        i += 1;
    }
    out
}

/// B: base64 up to the first `=`, padding not required; a character
/// outside the alphabet fails the word, which then shows raw.
fn decode_b(text: &[u8]) -> Option<Vec<u8>> {
    let val = |b: u8| match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut n) = (0u32, 0u32);
    for &b in text.iter().take_while(|&&b| b != b'=') {
        bits = bits << 6 | u32::from(val(b)?);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Some(out)
}

// Encoding, for what rmut sends: mutt's `rfc2047_encode`, with UTF-8
// the only charset in and out (rmut declares utf-8 everywhere).

const ENCWORD_LEN_MAX: usize = 75;
/// strlen("=?.?.?.?=")
const ENCWORD_LEN_MIN: usize = 9;
const CHARSET: &str = "utf-8";
/// mutt's RFC822Specials: in a display name they force the encoding to
/// take them in, since an encoded word cannot sit in a quoted string.
const RFC822_SPECIALS: &[u8] = b"@.,:;<>[]\\\"()";
/// mutt's RFC2047Specials: what a Q-encoded word has to escape.
const RFC2047_SPECIALS: &[u8] = b"@.,;:<>[]\\\"()?/= \t";
/// The headers mutt's rfc2047_encode_envelope treats as address
/// lists, whose display names are encoded and addresses left alone.
const ADDRESS_HEADERS: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "reply-to",
    "mail-followup-to",
    "sender",
];

#[derive(Clone, Copy)]
enum Encoder {
    B,
    Q,
}

fn continuation(b: u8) -> bool {
    b & 0xc0 == 0x80
}

/// What a Q-encoded word writes as =XX (a space is the one-character
/// `_`).
fn q_escaped(c: u8) -> bool {
    !(0x20..0x7f).contains(&c) || c == b'_' || (c != b' ' && RFC2047_SPECIALS.contains(&c))
}

/// mutt's HSPACE, where the end of the text counts as space.
fn hspace(u: &[u8], i: usize) -> bool {
    i >= u.len() || u[i] == b' ' || u[i] == b'\t'
}

/// mutt's try_block: the encoder and the length of the one encoded
/// word that holds `d`, or, when none can, an upper bound on how much
/// of it one could.
fn try_block(d: &[u8]) -> std::result::Result<(Encoder, usize), usize> {
    // mutt converts into a buffer this size, less the charset name.
    let room = ENCWORD_LEN_MAX - ENCWORD_LEN_MIN + 1 - CHARSET.len();
    if d.len() > room {
        let mut fits = room;
        while fits > 0 && continuation(d[fits]) {
            fits -= 1;
        }
        return Err(fits + 1);
    }
    let count = d.iter().filter(|&&c| q_escaped(c)).count();
    let len = ENCWORD_LEN_MIN - 2 + CHARSET.len();
    let len_b = len + d.len().div_ceil(3) * 4;
    let len_q = len + d.len() + 2 * count;
    if len_b < len_q && len_b <= ENCWORD_LEN_MAX {
        Ok((Encoder::B, len_b))
    } else if len_q <= ENCWORD_LEN_MAX {
        Ok((Encoder::Q, len_q))
    } else {
        Err(d.len())
    }
}

/// mutt's choose_block: how much of `d` goes in one encoded word
/// starting at column `col`, with its encoder and length.
fn choose_block(d: &[u8], col: usize) -> (usize, Encoder, usize) {
    let mut n = d.len();
    loop {
        let nn = match try_block(&d[..n]) {
            Ok((encoder, wlen)) if col + wlen <= ENCWORD_LEN_MAX + 1 || n <= 1 => {
                return (n, encoder, wlen);
            }
            Ok(_) => 0,
            Err(nn) => nn,
        };
        n = if nn > 0 { nn } else { n } - 1;
        while n > 1 && continuation(d[n]) {
            n -= 1;
        }
    }
}

fn encode_block(d: &[u8], encoder: Encoder) -> String {
    let text = match encoder {
        Encoder::B => crate::smtp::b64(d),
        Encoder::Q => d
            .iter()
            .map(|&c| match c {
                b' ' => "_".to_string(),
                c if q_escaped(c) => format!("={c:02X}"),
                c => char::from(c).to_string(),
            })
            .collect(),
    };
    let tag = match encoder {
        Encoder::B => 'B',
        Encoder::Q => 'Q',
    };
    format!("=?{CHARSET}?{tag}?{text}?=")
}

/// mutt's rfc2047_encode: the stretch of `s` from the first word that
/// needs it (non-ASCII, or what would read as an encoded word) to the
/// last, as encoded words of at most 75 characters folded onto lines
/// of their own; the ASCII either side as it stands. With `specials`,
/// those characters are taken into the stretch too. `col` is the
/// column the text starts in.
fn encode_text(s: &str, mut col: usize, specials: &[u8]) -> String {
    let u = s.as_bytes();
    let ulen = u.len();
    let (mut t0, mut t1, mut s0, mut s1) = (None, None, None, None);
    for t in 0..ulen {
        if u[t] & 0x80 != 0
            || (u[t] == b'=' && u.get(t + 1) == Some(&b'?') && (t == 0 || hspace(u, t - 1)))
        {
            t0.get_or_insert(t);
            t1 = Some(t);
        } else if specials.contains(&u[t]) {
            s0.get_or_insert(t);
            s1 = Some(t);
        }
    }
    let (Some(mut t0), Some(mut t1)) = (t0, t1) else {
        return s.to_string();
    };
    if let Some(s0) = s0 {
        t0 = t0.min(s0);
    }
    if let Some(s1) = s1 {
        t1 = t1.max(s1);
    }
    // Take in ASCII before the stretch that would not fit on the line.
    t0 = t0.min((ENCWORD_LEN_MAX + 1).saturating_sub(col + ENCWORD_LEN_MIN));
    // Back to the start of a word.
    while t0 > 0 {
        if hspace(u, t0 - 1) {
            let mut t = t0 + 1;
            while t < ulen && continuation(u[t]) {
                t += 1;
            }
            if let Ok((_, wlen)) = try_block(&u[t0..t])
                && col + t0 + wlen <= ENCWORD_LEN_MAX + 1
            {
                break;
            }
        }
        t0 -= 1;
    }
    // On to the end of a word.
    while t1 < ulen {
        if hspace(u, t1) {
            let mut t = t1 - 1;
            while continuation(u[t]) {
                t -= 1;
            }
            if let Ok((_, wlen)) = try_block(&u[t..t1])
                && 1 + wlen + (ulen - t1) <= ENCWORD_LEN_MAX + 1
            {
                break;
            }
        }
        t1 += 1;
    }
    // Encode [t0, t1).
    let mut out = s[..t0].to_string();
    col += t0;
    let mut t = t0;
    loop {
        let (mut n, mut encoder, wlen) = choose_block(&u[t..t1], col);
        if n == t1 - t {
            // The ASCII after it fits on the line too: done.
            if col + wlen + (ulen - t1) <= ENCWORD_LEN_MAX + 1 {
                out += &encode_block(&u[t..t1], encoder);
                break;
            }
            n = t1 - t - 1;
            while continuation(u[t + n]) {
                n -= 1;
            }
            if n == 0 {
                // One character is all that needs encoding, with too
                // much after it for one word: take the next word in.
                t1 += 1;
                while t1 < ulen && !hspace(u, t1) {
                    t1 += 1;
                }
                continue;
            }
            (n, encoder, _) = choose_block(&u[t..t + n], col);
        }
        out += &encode_block(&u[t..t + n], encoder);
        out += "\n\t";
        col = 1;
        t += n;
    }
    out += &s[t1..];
    out
}

/// One display name as it goes out: encoded if it has to be, quoted
/// if it holds specials, else as it stands.
fn encode_phrase(name: &str, col: usize) -> String {
    if !name.is_ascii() {
        return encode_text(name, col, RFC822_SPECIALS);
    }
    if name.bytes().any(|c| RFC822_SPECIALS.contains(&c)) {
        return format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""));
    }
    name.to_string()
}

/// mutt's rfc2047_encode_adrlist over a header's value: each display
/// name encoded, starting where mutt has it start (the tag's width),
/// the addresses as they are. A value that will not parse goes out as
/// it stands.
fn encode_addresses(name: &str, value: &str) -> String {
    let Ok(list) = mailparse::addrparse(value) else {
        return value.to_string();
    };
    let col = name.len() + 2;
    let single = |a: &mailparse::SingleInfo| match a.display_name.as_deref() {
        Some(n) if !n.is_empty() => format!("{} <{}>", encode_phrase(n, col), a.addr),
        _ => a.addr.clone(),
    };
    list.iter()
        .map(|entry| match entry {
            mailparse::MailAddr::Single(a) => single(a),
            mailparse::MailAddr::Group(g) => format!(
                "{}: {};",
                encode_phrase(&g.group_name, col),
                g.addrs.iter().map(single).collect::<Vec<_>>().join(", ")
            ),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// A finalized draft's header block made fit to send, the way mutt's
/// rfc2047_encode_envelope and encode_headers prepare one: every
/// header whose value is not plain ASCII is unfolded and encoded, the
/// display names of an address list, anything else as text from
/// column 32 (mutt's rfc2047_encode_string). ASCII headers are left
/// as written, folding and all.
pub fn encode_head(head: &str) -> String {
    let mut fields: Vec<String> = Vec::new();
    for line in head.lines() {
        match fields.last_mut() {
            Some(field) if line.starts_with([' ', '\t']) => {
                field.push('\n');
                field.push_str(line);
            }
            _ => fields.push(line.to_string()),
        }
    }
    let encoded: Vec<String> = fields
        .into_iter()
        .map(|field| {
            let Some((name, value)) = field.split_once(':').filter(|_| !field.is_ascii()) else {
                return field;
            };
            let value = unfold(value.as_bytes());
            let value = match ADDRESS_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str()) {
                true => encode_addresses(name, &value),
                false => encode_text(&value, 32, &[]),
            };
            format!("{name}: {value}")
        })
        .collect();
    encoded.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_decode() {
        assert_eq!(
            decode("=?iso-8859-1?Q?=A1Hola,_se=F1or!?="),
            "\u{a1}Hola, se\u{f1}or!"
        );
        assert_eq!(decode("=?utf-8?B?xb5sdXTDvQ==?= kůň"), "žlutý kůň");
    }

    #[test]
    fn a_word_needs_no_whitespace_around_it() {
        // Out of spec, and mutt decodes both anyway.
        assert_eq!(decode("=?utf-8?Q?Nov=C3=BD_?=text"), "Nový text");
        assert_eq!(decode("Sale!=?utf-8?B?8J+Ssg==?="), "Sale!💲");
    }

    #[test]
    fn a_character_split_across_two_words_joins() {
        // The C3 BD of the ý straddles the two words.
        assert_eq!(
            decode("=?utf-8?B?xb5sdXTD?= =?utf-8?B?vSBrxa/FiA==?="),
            "žlutý kůň"
        );
    }

    #[test]
    fn whitespace_between_words_goes_and_other_text_stays() {
        assert_eq!(decode("=?utf-8?Q?a?=  =?utf-8?Q?b?="), "ab");
        assert_eq!(decode("=?utf-8?Q?a?= - =?utf-8?Q?b?="), "a - b");
        assert_eq!(decode("Re: =?utf-8?Q?a?= x"), "Re: a x");
    }

    #[test]
    fn different_charsets_convert_apart() {
        assert_eq!(decode("=?iso-8859-2?Q?=B9?= =?utf-8?Q?=C5=A1?="), "šš");
    }

    #[test]
    fn what_is_not_a_word_stays_as_it_was() {
        assert_eq!(decode("a =? b ?= c"), "a =? b ?= c");
        assert_eq!(decode("=?utf-8?X?abc?="), "=?utf-8?X?abc?=");
        // A bad base64 character fails the word, which then shows raw.
        assert_eq!(decode("=?utf-8?B?a!b?="), "=?utf-8?B?a!b?=");
    }

    #[test]
    fn rfc2231_language_is_dropped_and_unknown_charsets_read_as_utf8() {
        assert_eq!(decode("=?utf-8*cs?Q?=C5=BE?="), "ž");
        assert_eq!(decode("=?x-nothing?Q?ok?="), "ok");
    }

    #[test]
    fn folded_values_unfold() {
        let (h, _) = mailparse::parse_header(
            b"Subject: =?utf-8?B?xb5sdXTD?=\r\n =?utf-8?B?vSBrxa/FiA==?=\r\n  a osel\r\n",
        )
        .unwrap();
        assert_eq!(value(&h), "žlutý kůň a osel");
    }

    #[test]
    fn trailing_whitespace_goes_as_in_mutt() {
        let (h, _) = mailparse::parse_header(b"Subject: Re: hello \t\r\n").unwrap();
        assert_eq!(value(&h), "Re: hello");
        let (h, _) = mailparse::parse_header(b"Subject: a  \r\n\t  b\r\n").unwrap();
        assert_eq!(value(&h), "a b");
    }

    /// The header block parsed back, each value decoded by mailparse:
    /// what a strict reader makes of what rmut sends.
    fn sent(head: &str) -> Vec<(String, String)> {
        let raw = format!("{}\n\nbody\n", encode_head(head));
        let (headers, _) = mailparse::parse_headers(raw.as_bytes()).unwrap();
        headers
            .iter()
            .map(|h| (h.get_key(), h.get_value()))
            .collect()
    }

    #[test]
    fn a_subject_goes_out_encoded_and_comes_back() {
        let out = encode_head("Subject: Příliš žluťoučký kůň");
        assert!(out.is_ascii(), "{out}");
        assert!(out.starts_with("Subject: =?utf-8?"), "{out}");
        assert_eq!(
            sent("Subject: Příliš žluťoučký kůň")[0].1,
            "Příliš žluťoučký kůň"
        );
        // ASCII before and after the stretch stays readable.
        let out = encode_head("Subject: Re: Fwd: Grüße of it all");
        assert!(out.starts_with("Subject: Re: Fwd: =?utf-8?"), "{out}");
        assert!(out.ends_with("?= of it all"), "{out}");
        assert_eq!(
            sent("Subject: Re: Fwd: Grüße of it all")[0].1,
            "Re: Fwd: Grüße of it all"
        );
    }

    #[test]
    fn a_long_subject_folds_into_words_a_reader_joins() {
        let subject = "Příliš žluťoučký kůň úpěl ďábelské ódy, ".repeat(6);
        let subject = subject.trim_end();
        let out = encode_head(&format!("Subject: {subject}"));
        assert!(out.is_ascii(), "{out}");
        for word in out.split_whitespace().filter(|w| w.starts_with("=?")) {
            assert!(word.len() <= 75, "{word}");
        }
        for line in out.lines() {
            assert!(line.len() <= 78, "{line}");
        }
        assert_eq!(sent(&format!("Subject: {subject}"))[0].1, subject);
        // mutt's decoder too, whitespace between the words dropped.
        let (_, value) = out.split_once(": ").unwrap();
        assert_eq!(decode(&unfold(value.as_bytes())), subject);
    }

    #[test]
    fn only_display_names_are_encoded_in_an_address_list() {
        let head = "From: Jana Nováková <jana@example.com>\n\
                    To: \"Dvořák, Jan\" <jan@example.com>, plain@example.com,\n\
                    \tJohn Doe <john@example.com>";
        let out = encode_head(head);
        assert!(out.is_ascii(), "{out}");
        assert!(out.contains("<jana@example.com>"), "{out}");
        assert!(out.contains("plain@example.com"), "{out}");
        let raw = format!("{out}\n\nbody\n");
        let (headers, _) = mailparse::parse_headers(raw.as_bytes()).unwrap();
        let from = mailparse::addrparse_header(&headers[0]).unwrap();
        assert_eq!(
            from.extract_single_info().unwrap().display_name.as_deref(),
            Some("Jana Nováková")
        );
        let to = mailparse::addrparse_header(&headers[1]).unwrap();
        let names: Vec<_> = to
            .iter()
            .map(|a| match a {
                mailparse::MailAddr::Single(s) => (s.display_name.clone(), s.addr.clone()),
                mailparse::MailAddr::Group(_) => panic!("no group here"),
            })
            .collect();
        assert_eq!(
            names,
            [
                (
                    Some("Dvořák, Jan".to_string()),
                    "jan@example.com".to_string()
                ),
                (None, "plain@example.com".to_string()),
                (Some("John Doe".to_string()), "john@example.com".to_string()),
            ]
        );
    }

    #[test]
    fn ascii_headers_are_left_as_written() {
        let head = "To: Jane <jane@example.com>,\n\tjohn@example.com\nSubject: hi =? there\nX-Note: =?not encoded";
        assert_eq!(encode_head(head), head);
    }

    #[test]
    fn what_would_read_as_an_encoded_word_is_encoded() {
        // Non-ASCII elsewhere makes the header one to encode; a literal
        // =? starting a word inside the stretch must not be decoded.
        let head = "Subject: =?utf-8?Q?x?= café";
        assert_eq!(sent(head)[0].1, "=?utf-8?Q?x?= café");
    }
}
