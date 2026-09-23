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
}
