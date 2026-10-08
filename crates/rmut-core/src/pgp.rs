//! PGP support by shelling out to gpg(1): decrypt and verify messages
//! for the pager, sign and encrypt outgoing drafts. Handles PGP/MIME
//! (RFC 3156) and inline ("armor in the body") messages. Passphrases
//! are between gpg and its agent; rmut never sees or stores them.

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use mailparse::{ParsedMail, parse_mail};

use crate::config::Pgp;
use crate::message;

/// Signature verdict from gpg's --status-fd lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sig {
    /// Valid signature by this user id.
    Good(String),
    /// The signature does not match: the content was altered.
    Bad(String),
    /// Cannot check (missing public key, unsupported algorithm, ...).
    Unknown(String),
}

// ---- running gpg ----

struct Gpg {
    stdout: Vec<u8>,
    /// "[GNUPG:] " lines from stderr, prefix stripped.
    status: Vec<String>,
    /// The remaining (human-readable) stderr, for error messages.
    diag: String,
    success: bool,
}

impl Gpg {
    fn error(&self, what: &str) -> anyhow::Error {
        let first = self.diag.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            anyhow::anyhow!("{what}")
        } else {
            anyhow::anyhow!("{what}: {}", first.trim_start_matches("gpg: "))
        }
    }
}

/// Run gpg with `input` on stdin. --batch/--no-tty keep gpg off our
/// terminal (pinentry still works through the agent); --status-fd 2
/// adds machine-readable lines to stderr, where the [GNUPG:] prefix
/// keeps them separable from diagnostics.
fn run(cfg: &Pgp, args: &[&str], input: &[u8]) -> Result<Gpg> {
    let spawn = || {
        Command::new(&cfg.command)
            .args(["--batch", "--no-tty", "--status-fd", "2"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    };
    let mut child = spawn()
        .or_else(|err| {
            // ETXTBSY: a freshly written executable can be transiently
            // busy (fd still open across a fork elsewhere), so retry.
            if err.raw_os_error() == Some(26) {
                std::thread::sleep(std::time::Duration::from_millis(20));
                spawn()
            } else {
                Err(err)
            }
        })
        .with_context(|| format!("running {}", cfg.command))?;
    let mut stdin = child.stdin.take().context("no stdin on gpg child")?;
    let input = input.to_vec();
    // Feed stdin from a thread while wait_with_output drains stdout and
    // stderr, so a large message cannot deadlock on full pipes.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    let mut status = Vec::new();
    let mut diag = Vec::new();
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        match line.strip_prefix("[GNUPG:] ") {
            Some(s) => status.push(s.to_string()),
            None => diag.push(line.to_string()),
        }
    }
    Ok(Gpg {
        stdout: out.stdout,
        status,
        diag: diag.join("\n"),
        success: out.status.success(),
    })
}

fn sig_from_status(status: &[String]) -> Option<Sig> {
    for line in status {
        let (kw, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        // GOODSIG/BADSIG carry "<keyid> <user id>".
        let uid = || rest.split_once(' ').map_or(rest, |(_, u)| u).to_string();
        match kw {
            "GOODSIG" => return Some(Sig::Good(uid())),
            "EXPKEYSIG" => return Some(Sig::Good(format!("{} (expired key)", uid()))),
            "REVKEYSIG" => return Some(Sig::Good(format!("{} (revoked key)", uid()))),
            "BADSIG" => return Some(Sig::Bad(uid())),
            "ERRSIG" => {
                let keyid = rest.split(' ').next().unwrap_or("?").to_string();
                let missing = status.iter().any(|l| l.starts_with("NO_PUBKEY"));
                return Some(Sig::Unknown(if missing {
                    format!("no public key {keyid}")
                } else {
                    format!("key {keyid}")
                }));
            }
            _ => {}
        }
    }
    None
}

// ---- primitives ----

#[derive(Debug)]
pub struct Opened {
    pub plaintext: Vec<u8>,
    pub sig: Option<Sig>,
}

/// Decrypt an armored PGP message. Also accepts clearsigned input,
/// where gpg strips the armor and verifies instead. A bad signature is
/// reported in `sig`, not as an error; the plaintext is still wanted.
pub fn decrypt(cfg: &Pgp, data: &[u8]) -> Result<Opened> {
    let out = run(cfg, &["--decrypt"], data)?;
    let sig = sig_from_status(&out.status);
    let was_encrypted = out.status.iter().any(|s| s.starts_with("BEGIN_DECRYPTION"));
    let decrypted = out.status.iter().any(|s| s.starts_with("DECRYPTION_OKAY"));
    if was_encrypted && !decrypted {
        // NO_SECKEY names each key the message was encrypted to that
        // has no secret here; gpg's first line would only say which
        // key it tried.
        let missing = keyids(&out.status, "NO_SECKEY ");
        if !missing.is_empty() {
            bail!("no secret key for {}", no_secret(&missing, &out.diag));
        }
        // The view says "decryption failed" already; gpg's last line
        // is its verdict, its first only which key it tried.
        let last = out.diag.lines().map(str::trim).rfind(|l| !l.is_empty());
        let reason = last.unwrap_or("gpg gave no reason");
        let reason = reason.trim_start_matches("gpg: ");
        bail!("{}", reason.trim_start_matches("decryption failed: "));
    }
    if !was_encrypted && sig.is_none() && !out.success {
        return Err(out.error("gpg failed"));
    }
    Ok(Opened {
        plaintext: out.stdout,
        sig,
    })
}

/// The key ids of the status lines starting with `prefix`.
fn keyids(status: &[String], prefix: &str) -> Vec<String> {
    status
        .iter()
        .filter_map(|l| l.strip_prefix(prefix))
        .filter_map(|rest| rest.split(' ').next())
        .map(str::to_string)
        .collect()
}

/// The keys without a secret here, told apart. One whose public half
/// is in the keyring is named with its user id and creation date (an
/// old key of one's own, typically); the rest belong to the other
/// recipients and are only counted, as their ids say nothing.
fn no_secret(ids: &[String], diag: &str) -> String {
    let lines: Vec<&str> = diag.lines().map(str::trim).collect();
    let known: Vec<String> = ids
        .iter()
        .filter_map(|id| {
            // "encrypted with <algo> key, ID <id>, created <date>",
            // then the user id quoted when the public key is here.
            let at = lines.iter().position(|l| {
                l.split_once(", ID ")
                    .is_some_and(|(_, rest)| rest.split(',').next() == Some(id.as_str()))
            })?;
            let uid = lines
                .get(at + 1)
                .filter(|l| l.len() > 1 && l.starts_with('"') && l.ends_with('"'))?
                .trim_matches('"');
            Some(match lines[at].split_once(", created ") {
                Some((_, date)) => format!("{id} ({uid}, created {})", date.trim()),
                None => format!("{id} ({uid})"),
            })
        })
        .collect();
    let named = known.join(", ");
    match (known.len(), ids.len() - known.len()) {
        (0, 1) => format!("{}, not in your keyring", ids[0]),
        (0, _) => format!("{}, none of them in your keyring", ids.join(", ")),
        (_, 0) => named,
        (_, 1) => format!("{named} or the other recipient's key"),
        (_, n) => format!("{named} or the {n} other recipients' keys"),
    }
}

/// `ids`, followed by the user id gpg printed (quoted, on the line
/// under "encrypted with ...") when there was one.
fn with_uid(ids: &[String], diag: &str) -> String {
    let uid = diag
        .lines()
        .map(str::trim)
        .find(|l| l.len() > 1 && l.starts_with('"') && l.ends_with('"'));
    match uid {
        Some(uid) => format!("{} ({})", ids.join(", "), uid.trim_matches('"')),
        None => ids.join(", "),
    }
}

/// Would a message encrypted to `recipient` be one this keyring can
/// open? gpg picks the key by address, and that can be a key whose
/// secret half is gone (an old key that a keyserver still serves).
/// Encrypts nothing to it, reads which key was used, and returns that
/// key described when no secret for it is here. Anything gpg refuses
/// is left for the real encryption to report.
pub fn missing_secret(cfg: &Pgp, recipient: &str) -> Result<Option<String>> {
    let probe = run(
        cfg,
        &[
            "--armor",
            "--encrypt",
            "--trust-model",
            "always",
            "--recipient",
            recipient,
        ],
        b"",
    )?;
    if !probe.success || probe.stdout.is_empty() {
        return Ok(None);
    }
    let listed = run(cfg, &["--list-only", "--decrypt"], &probe.stdout)?;
    let ids = keyids(&listed.status, "ENC_TO ");
    if ids.is_empty() {
        return Ok(None);
    }
    let secret = run(cfg, &["--with-colons", "--list-secret-keys"], b"")?;
    let secret = String::from_utf8_lossy(&secret.stdout);
    // sec/ssb lines: field 5 is the key id, field 15 "#" a stub whose
    // secret is elsewhere (or nowhere).
    let held = |id: &String| {
        secret.lines().any(|l| {
            let f: Vec<&str> = l.split(':').collect();
            matches!(f[0], "sec" | "ssb")
                && f.get(4).is_some_and(|k| k.eq_ignore_ascii_case(id))
                && f.get(14) != Some(&"#")
        })
    };
    if ids.iter().any(held) {
        return Ok(None);
    }
    Ok(Some(with_uid(&ids, &listed.diag)))
}

/// Verify a detached signature over `data` (already in the CRLF form
/// it was signed in). gpg wants the signature as a file argument.
pub fn verify_detached(cfg: &Pgp, signature: &[u8], data: &[u8]) -> Result<Sig> {
    let sigfile = crate::scratch::write("sig", "", signature)?;
    let result = run(
        cfg,
        &["--verify", &sigfile.display().to_string(), "-"],
        data,
    );
    let _ = std::fs::remove_file(&sigfile);
    let out = result?;
    sig_from_status(&out.status).ok_or_else(|| out.error("no verdict from gpg"))
}

/// Detached armored signature over `data`, plus the micalg parameter
/// for the multipart/signed header.
pub fn sign_detached(cfg: &Pgp, data: &[u8]) -> Result<(String, String)> {
    let mut args = vec!["--armor", "--detach-sign"];
    if let Some(key) = &cfg.sign_key {
        args.extend(["--local-user", key.as_str()]);
    }
    let out = run(cfg, &args, data)?;
    ensure!(
        out.success && !out.stdout.is_empty(),
        out.error("signing failed")
    );
    let sig = String::from_utf8(out.stdout).context("gpg produced non-UTF-8 armor")?;
    Ok((sig, micalg(&out.status)))
}

/// "SIG_CREATED <type> <pk algo> <hash algo> ...": map the hash
/// number to the RFC 3156 micalg value, assuming SHA-256 when absent.
fn micalg(status: &[String]) -> String {
    let hash = status
        .iter()
        .find_map(|l| l.strip_prefix("SIG_CREATED "))
        .and_then(|rest| rest.split(' ').nth(2))
        .and_then(|n| n.parse::<u32>().ok());
    let name = match hash {
        Some(1) => "md5",
        Some(2) => "sha1",
        Some(3) => "ripemd160",
        Some(9) => "sha384",
        Some(10) => "sha512",
        Some(11) => "sha224",
        _ => "sha256",
    };
    format!("pgp-{name}")
}

/// Armored encryption of `data` to every recipient address (gpg finds
/// the keys), optionally signed in the same pass. --trust-model always
/// mirrors mutt: keys are picked by address, not by web-of-trust.
pub fn encrypt(cfg: &Pgp, recipients: &[String], sign: bool, data: &[u8]) -> Result<String> {
    let mut args = vec!["--armor", "--encrypt", "--trust-model", "always"];
    for r in recipients {
        args.extend(["--recipient", r.as_str()]);
    }
    if sign {
        args.push("--sign");
        if let Some(key) = &cfg.sign_key {
            args.extend(["--local-user", key.as_str()]);
        }
    }
    let out = run(cfg, &args, data)?;
    if !out.success || out.stdout.is_empty() {
        // INV_RECP names each address gpg has no key for.
        let missing: Vec<&str> = out
            .status
            .iter()
            .filter_map(|l| l.strip_prefix("INV_RECP "))
            .filter_map(|rest| rest.split(' ').nth(1))
            .collect();
        if !missing.is_empty() {
            bail!("no key for {}", missing.join(", "));
        }
        return Err(out.error("encryption failed"));
    }
    String::from_utf8(out.stdout).context("gpg produced non-UTF-8 armor")
}

// ---- viewing ----

/// What the pager should show for a PGP message: the message as it
/// reads once opened, and a one-line status note. gpg trouble goes in
/// the note; the original message stays available.
pub struct View {
    /// The raw message with every part that decrypted (or verified
    /// inline) replaced by its plaintext, so the pager, a reply's
    /// quote and the attachment menu all read the same tree. None
    /// when nothing opened: the original stands.
    pub opened: Option<Vec<u8>>,
    pub note: String,
}

fn note(text: &str) -> String {
    format!("[-- PGP: {text} --]")
}

fn sig_phrase(sig: &Sig) -> String {
    match sig {
        Sig::Good(uid) => format!("good signature from {uid}"),
        Sig::Bad(uid) => format!("BAD signature from {uid}"),
        Sig::Unknown(what) => format!("signature not verified ({what})"),
    }
}

/// Whether a message is signed and/or encrypted, without running gpg:
/// just the MIME type and the inline PGP markers. For the reply-crypto
/// defaults, which must not decrypt anything.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Crypto {
    pub signed: bool,
    pub encrypted: bool,
}

pub fn classify(raw: &[u8]) -> Crypto {
    let Ok(mail) = parse_mail(raw) else {
        return Crypto::default();
    };
    if mail.ctype.mimetype == "multipart/encrypted" || !encrypted_parts(&mail).is_empty() {
        return Crypto {
            encrypted: true,
            signed: false,
        };
    }
    if mail.ctype.mimetype == "multipart/signed"
        && mail.ctype.params.get("protocol").map(String::as_str)
            == Some("application/pgp-signature")
    {
        return Crypto {
            signed: true,
            encrypted: false,
        };
    }
    // Inline PGP: the armor markers in the text body.
    if let Some(text) = message::extract_text(&mail) {
        let blocks = armor_blocks(&text);
        if blocks.iter().any(|b| b.encrypted) {
            return Crypto {
                encrypted: true,
                signed: false,
            };
        }
        if !blocks.is_empty() {
            return Crypto {
                signed: true,
                encrypted: false,
            };
        }
    }
    Crypto::default()
}

/// Inspect a raw message; Some when it is PGP-encrypted or signed in
/// any of its shapes (PGP/MIME encrypted, at the top or further down,
/// or signed; inline encrypted or clearsigned).
pub fn view(cfg: &Pgp, raw: &[u8]) -> Option<View> {
    let mail = parse_mail(raw).ok()?;
    let encrypted = encrypted_parts(&mail);
    if !encrypted.is_empty() {
        return Some(view_encrypted(cfg, raw, &encrypted));
    }
    if mime_signed(&mail) {
        return Some(view_mime_signed(cfg, &mail));
    }
    let leaf = message::text_leaf(&mail)?;
    let text = message::text_body(leaf).ok()?;
    let blocks = armor_blocks(&text);
    if blocks.is_empty() {
        return None;
    }
    Some(view_inline(cfg, raw, leaf, &text, &blocks))
}

fn mime_signed(mail: &ParsedMail) -> bool {
    mail.ctype.mimetype == "multipart/signed"
        && mail.ctype.params.get("protocol").map(String::as_str)
            == Some("application/pgp-signature")
        && mail.subparts.len() >= 2
}

/// A PGP/MIME encrypted part and the part inside it holding the
/// ciphertext.
struct Sealed<'m, 'a> {
    whole: &'m ParsedMail<'a>,
    cipher: &'m ParsedMail<'a>,
}

/// The PGP/MIME encrypted parts of `mail`, the top of it included,
/// outermost ones only, in document order. Besides multipart/encrypted
/// itself, a mailing list may wrap one in a multipart/mixed with its
/// footer, and Exchange rewrites one into a multipart/mixed of the
/// same two parts, an empty text part in front (mutt's
/// malformed_multipart_pgp_encrypted).
fn encrypted_parts<'m, 'a>(mail: &'m ParsedMail<'a>) -> Vec<Sealed<'m, 'a>> {
    fn cipher<'m, 'a>(part: &'m ParsedMail<'a>) -> Option<&'m ParsedMail<'a>> {
        let mut subs = part.subparts.as_slice();
        match part.ctype.mimetype.as_str() {
            "multipart/encrypted" => return subs.get(1).filter(|_| subs.len() >= 2),
            "multipart/mixed" => {}
            _ => return None,
        }
        if let [first, rest @ ..] = subs
            && first.ctype.mimetype == "text/plain"
            && first
                .get_body_raw()
                .is_ok_and(|b| b.iter().all(u8::is_ascii_whitespace))
        {
            subs = rest;
        }
        match subs {
            [control, data]
                if control.ctype.mimetype == "application/pgp-encrypted"
                    && data.ctype.mimetype == "application/octet-stream" =>
            {
                Some(data)
            }
            _ => None,
        }
    }
    fn walk<'m, 'a>(part: &'m ParsedMail<'a>, out: &mut Vec<Sealed<'m, 'a>>) {
        match cipher(part) {
            Some(cipher) => out.push(Sealed {
                whole: part,
                cipher,
            }),
            None => part.subparts.iter().for_each(|sub| walk(sub, out)),
        }
    }
    let mut out = Vec::new();
    walk(mail, &mut out);
    out
}

fn decrypted_phrase(opened: &Opened) -> String {
    match &opened.sig {
        Some(sig) => format!("decrypted; {}", sig_phrase(sig)),
        None => "decrypted".into(),
    }
}

/// A message with PGP/MIME parts in it. Each part that decrypts is
/// replaced, in a copy of `raw`, by its plaintext entity, so the whole
/// tree renders with the clear parts around it (a list footer) still
/// in place. A part that does not decrypt stays as it was.
fn view_encrypted(cfg: &Pgp, raw: &[u8], parts: &[Sealed]) -> View {
    let mut opened = raw.to_vec();
    let mut phrases = Vec::new();
    let mut any = false;
    // Back to front, so the spans of the earlier parts stay valid.
    for part in parts.iter().rev() {
        let plain = part
            .cipher
            .get_body_raw()
            .context("cannot read the encrypted part")
            .and_then(|cipher| decrypt(cfg, &cipher));
        match plain {
            Ok(plain) => {
                replace(&mut opened, raw, part.whole, &as_part(&plain.plaintext));
                phrases.push(decrypted_phrase(&plain));
                any = true;
            }
            Err(err) => phrases.push(format!("decryption failed: {err:#}")),
        }
    }
    phrases.reverse();
    View {
        opened: any.then_some(opened),
        note: note(&phrases.join("; ")),
    }
}

/// Put `entity` where `part` (a slice of `raw`, as mailparse hands
/// out subparts) stands in `out`, a copy of `raw` changed only after
/// `part` so far. The top of the message keeps its own header fields,
/// all but the Content- ones, which the entity brings along.
fn replace(out: &mut Vec<u8>, raw: &[u8], part: &ParsedMail, entity: &[u8]) {
    let start = part.raw_bytes.as_ptr() as usize - raw.as_ptr() as usize;
    let end = start + part.raw_bytes.len();
    let mut new = Vec::with_capacity(entity.len() + 1024);
    if start == 0 {
        new.extend_from_slice(&outer_head(raw));
    }
    new.extend_from_slice(entity);
    out.splice(start..end, new);
}

/// The message's header block without its Content- fields and the
/// blank line after it, folded lines kept with their field.
fn outer_head(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut keep = true;
    for line in raw.split_inclusive(|&b| b == b'\n') {
        if line == b"\n" || line == b"\r\n" {
            break;
        }
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            keep = !line
                .get(..8)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"content-"));
        }
        if keep {
            out.extend_from_slice(line);
        }
    }
    out
}

/// A plaintext entity ready to stand where its encrypted part stood.
/// The newline before the next boundary belongs to the boundary (and
/// is still there), so the entity gives its own last one up. Text
/// that is not a MIME entity at all gets a bare text/plain header.
fn as_part(plaintext: &[u8]) -> Vec<u8> {
    let mut text = plaintext;
    text = text.strip_suffix(b"\n").unwrap_or(text);
    text = text.strip_suffix(b"\r").unwrap_or(text);
    // An entity opens on a header field; mailparse would take any
    // first line for one, so look at its shape: a name, then a colon.
    let first = text.split(|&b| b == b'\n').next().unwrap_or_default();
    let is_entity = first
        .iter()
        .position(|&b| b == b':')
        .is_some_and(|colon| colon > 0 && first[..colon].iter().all(|&b| b.is_ascii_graphic()));
    let mut out = Vec::with_capacity(text.len() + 32);
    if !is_entity {
        out.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
    }
    out.extend_from_slice(text);
    out
}

fn view_mime_signed(cfg: &Pgp, mail: &ParsedMail) -> View {
    let signed = crlf(mail.subparts[0].raw_bytes);
    let sig_armor = match mail.subparts[1].get_body_raw() {
        Ok(s) => s,
        Err(err) => {
            return View {
                opened: None,
                note: note(&format!("cannot read the signature part: {err}")),
            };
        }
    };
    // The CRLF before the closing boundary belongs to the boundary
    // delimiter, but some senders sign a trailing newline anyway, so
    // on a mismatch, retry with one appended before calling it bad.
    let verdict = verify_detached(cfg, &sig_armor, &signed).and_then(|sig| {
        if !matches!(sig, Sig::Bad(_)) {
            return Ok(sig);
        }
        let mut with_crlf = signed.clone();
        with_crlf.extend_from_slice(b"\r\n");
        match verify_detached(cfg, &sig_armor, &with_crlf)? {
            good @ Sig::Good(_) => Ok(good),
            _ => Ok(sig),
        }
    });
    View {
        opened: None,
        note: match verdict {
            Ok(sig) => note(&sig_phrase(&sig)),
            Err(err) => note(&format!("cannot verify signature: {err:#}")),
        },
    }
}

/// One armored block in a text body, by byte offsets, its END line
/// and that line's newline included.
struct Armor {
    start: usize,
    end: usize,
    encrypted: bool,
}

/// The inline PGP blocks in `text`, wherever they start: a reply or a
/// greeting may stand before one. A block with no END line runs to
/// the end of the text, for gpg to judge.
fn armor_blocks(text: &str) -> Vec<Armor> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, bool)> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end();
        match open {
            None if bare == "-----BEGIN PGP MESSAGE-----" => open = Some((at, true)),
            None if bare == "-----BEGIN PGP SIGNED MESSAGE-----" => open = Some((at, false)),
            Some((start, encrypted))
                if bare
                    == if encrypted {
                        "-----END PGP MESSAGE-----"
                    } else {
                        "-----END PGP SIGNATURE-----"
                    } =>
            {
                blocks.push(Armor {
                    start,
                    end: at + line.len(),
                    encrypted,
                });
                open = None;
            }
            _ => {}
        }
        at += line.len();
    }
    if let Some((start, encrypted)) = open {
        blocks.push(Armor {
            start,
            end: text.len(),
            encrypted,
        });
    }
    blocks
}

/// Inline PGP: each block in the text part `leaf` decrypted or
/// verified in place. Text outside the blocks was neither encrypted
/// nor signed, so when there is any, mutt's BEGIN/END lines mark
/// where the protected text starts and stops.
fn view_inline(cfg: &Pgp, raw: &[u8], leaf: &ParsedMail, text: &str, blocks: &[Armor]) -> View {
    let mut outside = 0;
    let mut rest = 0;
    for b in blocks {
        outside += text[rest..b.start].trim().len();
        rest = b.end;
    }
    let framed = outside + text[rest..].trim().len() > 0;
    let mut body = String::with_capacity(text.len());
    let mut phrases = Vec::new();
    let mut any = false;
    let mut rest = 0;
    for b in blocks {
        body.push_str(&text[rest..b.start]);
        rest = b.end;
        let block = &text[b.start..b.end];
        let (what, failed) = match b.encrypted {
            true => ("PGP MESSAGE", "decryption"),
            false => ("PGP SIGNED MESSAGE", "verification"),
        };
        match decrypt(cfg, block.as_bytes()) {
            Ok(opened) => {
                phrases.push(match (&opened.sig, b.encrypted) {
                    (Some(sig), true) => format!("decrypted; {}", sig_phrase(sig)),
                    (Some(sig), false) => sig_phrase(sig),
                    (None, true) => "decrypted".into(),
                    (None, false) => "signed, no verdict from gpg".into(),
                });
                any = true;
                if framed {
                    body.push_str(&format!("[-- BEGIN {what} --]\n"));
                }
                body.push_str(&in_charset(&opened.plaintext, leaf));
                if framed {
                    if !body.ends_with('\n') {
                        body.push('\n');
                    }
                    body.push_str(&format!("[-- END {what} --]\n"));
                }
            }
            Err(err) => {
                phrases.push(format!("{failed} failed: {err:#}"));
                body.push_str(block);
            }
        }
    }
    body.push_str(&text[rest..]);
    let opened = any.then(|| {
        let entity = format!(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}"
        );
        let mut out = raw.to_vec();
        replace(&mut out, raw, leaf, entity.as_bytes());
        out
    });
    View {
        opened,
        note: note(&phrases.join("; ")),
    }
}

/// Inline plaintext as text. The armor is ASCII whatever the sender
/// wrote, so the plaintext inside is in the charset the part declares
/// (mutt converts it the same way); us-ascii, a label meant only for
/// the armor, or no charset at all leaves it read as UTF-8.
fn in_charset(plaintext: &[u8], leaf: &ParsedMail) -> String {
    let declared = leaf
        .ctype
        .params
        .get("charset")
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| !matches!(c.as_str(), "us-ascii" | "ascii" | "utf-8" | "utf8"))
        .and_then(|c| charset::Charset::for_label_no_replacement(c.as_bytes()));
    match declared {
        Some(cs) => cs.decode_without_bom_handling(plaintext).0.into_owned(),
        None => String::from_utf8_lossy(plaintext).into_owned(),
    }
}

// ---- outgoing (RFC 3156) ----

/// Wrap a finalized draft in multipart/signed. `flowed` is mutt's
/// $text_flowed, passed through to the text part inside.
pub fn sign_message(cfg: &Pgp, text: &str, flowed: bool) -> Result<String> {
    let (head, body) = split_head_body(text);
    sign_entity(cfg, head, &inner_entity(body, flowed))
}

/// Wrap an arbitrary MIME entity (its own Content-Type header + body,
/// CRLF endings, e.g. compose::mixed_entity) in multipart/signed
/// under `head`.
pub fn sign_entity(cfg: &Pgp, head: &str, entity: &[u8]) -> Result<String> {
    let (sig, micalg) = sign_detached(cfg, entity)?;
    let b = boundary(&[entity, sig.as_bytes()]);
    let mut out = head.trim_end().to_string();
    out += &format!(
        "\nMIME-Version: 1.0\nContent-Type: multipart/signed; boundary=\"{b}\";\n\tmicalg={micalg}; protocol=\"application/pgp-signature\"\n\n"
    );
    out += &format!("--{b}\r\n");
    // Byte-identical to what was signed: entity, then the delimiter.
    out += std::str::from_utf8(entity).expect("entity is built from str");
    out += &format!(
        "\r\n--{b}\r\nContent-Type: application/pgp-signature\r\n\r\n{}\r\n--{b}--\r\n",
        sig.trim_end()
    );
    Ok(out)
}

/// Wrap a finalized draft in multipart/encrypted for `recipients`
/// (which the caller assembles from To/Cc/Bcc plus the sender, so the
/// author can read their own mail); `sign` adds a signature inside the
/// encryption layer. Headers, including Subject, stay in clear.
pub fn encrypt_message(
    cfg: &Pgp,
    recipients: &[String],
    sign: bool,
    text: &str,
    flowed: bool,
) -> Result<String> {
    let (head, body) = split_head_body(text);
    encrypt_entity(cfg, recipients, sign, head, &inner_entity(body, flowed))
}

/// Like `encrypt_message`, but over an arbitrary MIME entity.
pub fn encrypt_entity(
    cfg: &Pgp,
    recipients: &[String],
    sign: bool,
    head: &str,
    entity: &[u8],
) -> Result<String> {
    let armor = encrypt(cfg, recipients, sign, entity)?;
    let b = boundary(&[armor.as_bytes()]);
    let mut out = head.trim_end().to_string();
    out += &format!(
        "\nMIME-Version: 1.0\nContent-Type: multipart/encrypted; boundary=\"{b}\";\n\tprotocol=\"application/pgp-encrypted\"\n\n"
    );
    out += &format!("--{b}\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n");
    out += &format!(
        "--{b}\r\nContent-Type: application/octet-stream\r\n\r\n{}\r\n--{b}--\r\n",
        armor.trim_end()
    );
    Ok(out)
}

fn split_head_body(text: &str) -> (&str, &str) {
    text.split_once("\n\n").unwrap_or((text.trim_end(), ""))
}

/// The draft body as a text/plain MIME entity with CRLF endings: the
/// exact bytes that get signed and shipped inside the multiparts.
fn inner_entity(body: &str, flowed: bool) -> Vec<u8> {
    crate::compose::text_entity(body, flowed).into_bytes()
}

/// RFC 3156 canonical form: every line ending is CRLF.
pub(crate) fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for (i, line) in data.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(line.strip_suffix(b"\r").unwrap_or(line));
    }
    out
}

/// A MIME boundary checked to not occur in any of the wrapped parts.
fn boundary(content: &[&[u8]]) -> String {
    for n in 0.. {
        let b = format!("=-rmut-{}-{n}", std::process::id());
        let bb = b.as_bytes();
        if content
            .iter()
            .all(|c| !c.windows(bb.len()).any(|w| w == bb))
        {
            return b;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// The opened message's text body, as a reply would quote it.
    fn body_text(v: &View) -> String {
        message::body_text_in(v.opened.as_ref().expect("an opened message")).unwrap()
    }

    /// A fake gpg: a shell script that inspects "$@", reads stdin, and
    /// prints canned stdout/status lines. $D is its own directory, for
    /// scratch files the test can assert on.
    fn stub(script: &str) -> (tempfile::TempDir, Pgp) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gpg");
        std::fs::write(&path, format!("#!/bin/sh\nD=$(dirname \"$0\")\n{script}\n")).unwrap();
        let mut perm = std::fs::metadata(&path).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&path, perm).unwrap();
        let cfg = Pgp {
            command: path.display().to_string(),
            ..Default::default()
        };
        (dir, cfg)
    }

    fn scratch(dir: &Path, name: &str) -> Vec<u8> {
        std::fs::read(dir.join(name)).unwrap()
    }

    #[test]
    fn sig_from_status_maps_keywords() {
        let s = |l: &str| vec![l.to_string()];
        assert_eq!(
            sig_from_status(&s("GOODSIG AAA Jane <j@x>")),
            Some(Sig::Good("Jane <j@x>".into()))
        );
        assert_eq!(
            sig_from_status(&s("BADSIG AAA Mallory")),
            Some(Sig::Bad("Mallory".into()))
        );
        assert_eq!(
            sig_from_status(&["ERRSIG KEY1 1 8 00 12 9".into(), "NO_PUBKEY KEY1".into()]),
            Some(Sig::Unknown("no public key KEY1".into()))
        );
        assert_eq!(
            sig_from_status(&s("EXPKEYSIG AAA Old <o@x>")),
            Some(Sig::Good("Old <o@x> (expired key)".into()))
        );
        assert_eq!(sig_from_status(&s("PLAINTEXT 74 123 x")), None);
    }

    #[test]
    fn micalg_maps_hash_numbers() {
        assert_eq!(
            micalg(&["SIG_CREATED D 1 8 00 12 FPR".into()]),
            "pgp-sha256"
        );
        assert_eq!(
            micalg(&["SIG_CREATED D 1 10 00 12 FPR".into()]),
            "pgp-sha512"
        );
        assert_eq!(micalg(&["SIG_CREATED D 1 2 00 12 FPR".into()]), "pgp-sha1");
        assert_eq!(micalg(&[]), "pgp-sha256");
    }

    #[test]
    fn crlf_canonicalizes_mixed_endings() {
        assert_eq!(crlf(b"a\nb\r\nc"), b"a\r\nb\r\nc");
        assert_eq!(crlf(b"a\n"), b"a\r\n");
        assert_eq!(crlf(b""), b"");
    }

    #[test]
    fn decrypt_returns_plaintext_and_sig() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2
printf 'secret'"#,
        );
        let opened = decrypt(&cfg, b"armor").unwrap();
        assert_eq!(opened.plaintext, b"secret");
        assert_eq!(opened.sig, Some(Sig::Good("Jane <j@x>".into())));
    }

    #[test]
    fn decrypt_failure_carries_gpg_diagnostic() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_FAILED" >&2
echo "gpg: decryption failed: No secret key" >&2
exit 2"#,
        );
        let err = decrypt(&cfg, b"armor").unwrap_err();
        assert!(format!("{err:#}").contains("No secret key"), "{err:#}");
    }

    #[test]
    fn decrypt_names_the_key_without_a_secret() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] ENC_TO 0LDKEY0000000001 1 0" >&2
echo "gpg: encrypted with rsa2048 key, ID 0LDKEY0000000001, created 2001-01-01" >&2
echo '      "Jane Doe <jane@x>"' >&2
echo "[GNUPG:] NO_SECKEY 0LDKEY0000000001" >&2
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_FAILED" >&2
echo "gpg: decryption failed: No secret key" >&2
exit 2"#,
        );
        let err = decrypt(&cfg, b"armor").unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "no secret key for 0LDKEY0000000001 (Jane Doe <jane@x>, created 2001-01-01)"
        );
    }

    /// gpg's lines for a message to an old key of one's own and to two
    /// other recipients, whose public keys are not here either.
    const TO_THREE: &str = "\
gpg: encrypted with ECDH key, ID 0THER00000000002
gpg: encrypted with rsa2048 key, ID 0LDKEY0000000001, created 2001-01-01
      \"Jane Doe <jane@x>\"
gpg: encrypted with rsa4096 key, ID 0THER00000000003
gpg: decryption failed: No secret key";

    #[test]
    fn no_secret_names_the_known_key_and_counts_the_rest() {
        let ids = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(
            no_secret(
                &ids("0THER00000000002 0LDKEY0000000001 0THER00000000003"),
                TO_THREE
            ),
            "0LDKEY0000000001 (Jane Doe <jane@x>, created 2001-01-01) \
             or the 2 other recipients' keys"
        );
        assert_eq!(
            no_secret(&ids("0LDKEY0000000001 0THER00000000003"), TO_THREE),
            "0LDKEY0000000001 (Jane Doe <jane@x>, created 2001-01-01) \
             or the other recipient's key"
        );
        assert_eq!(
            no_secret(&ids("0THER00000000002 0THER00000000003"), TO_THREE),
            "0THER00000000002, 0THER00000000003, none of them in your keyring"
        );
        assert_eq!(
            no_secret(&ids("0THER00000000002"), TO_THREE),
            "0THER00000000002, not in your keyring"
        );
    }

    /// A stub keyring: `old@x` resolves to the old key OLD, whose
    /// secret is gone; `new@x` to NEW, whose secret is here.
    const KEYRING: &str = r#"case "$*" in
*--list-secret-keys*)
  echo "sec:u:255:22:NEWPRIMARY:1:::u:::scESC:::+:::23::0:"
  echo "ssb:u:255:18:NEW:1::::::e:::+:::23:" ;;
*--list-only*)
  cat >/dev/null
  echo "[GNUPG:] ENC_TO $(cat "$D/to") 1 0" >&2
  echo '      "Jane <old@x>"' >&2 ;;
*--encrypt*)
  cat >/dev/null
  case "$*" in *old@x*) echo OLD > "$D/to" ;; *) echo NEW > "$D/to" ;; esac
  printf -- '-----BEGIN PGP MESSAGE-----
P
-----END PGP MESSAGE-----
' ;;
esac"#;

    #[test]
    fn missing_secret_spots_a_key_with_no_secret_here() {
        let (_dir, cfg) = stub(KEYRING);
        assert_eq!(
            missing_secret(&cfg, "old@x").unwrap().as_deref(),
            Some("OLD (Jane <old@x>)")
        );
        assert_eq!(missing_secret(&cfg, "new@x").unwrap(), None);
    }

    #[test]
    fn missing_secret_leaves_refusals_to_the_send() {
        let (_dir, cfg) = stub(
            "cat >/dev/null
echo '[GNUPG:] INV_RECP 0 x' >&2
exit 2",
        );
        assert_eq!(missing_secret(&cfg, "nobody@x").unwrap(), None);
    }

    #[test]
    fn verify_detached_passes_signature_file() {
        let (dir, cfg) = stub(
            r#"cat > "$D/data.in"
echo "$@" > "$D/args"
case "$*" in *--verify*) : ;; *) exit 9 ;; esac
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2"#,
        );
        let sig = verify_detached(&cfg, b"SIGDATA", b"payload").unwrap();
        assert_eq!(sig, Sig::Good("Jane <j@x>".into()));
        assert_eq!(scratch(dir.path(), "data.in"), b"payload");
        // The signature went through a (since removed) temp file.
        let args = String::from_utf8(scratch(dir.path(), "args")).unwrap();
        let sigfile = args
            .split_whitespace()
            .skip_while(|a| *a != "--verify")
            .nth(1)
            .unwrap();
        assert!(!Path::new(sigfile).exists());
    }

    #[test]
    fn sign_detached_returns_armor_and_micalg() {
        let (dir, cfg) = stub(
            r#"cat > "$D/signed.in"
echo "$@" > "$D/args"
echo "[GNUPG:] SIG_CREATED D 1 10 00 12 FPR" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\nAAA\n-----END PGP SIGNATURE-----\n'"#,
        );
        let cfg = Pgp {
            sign_key: Some("jane@x".into()),
            ..cfg
        };
        let (sig, micalg) = sign_detached(&cfg, b"payload").unwrap();
        assert!(sig.contains("BEGIN PGP SIGNATURE"));
        assert_eq!(micalg, "pgp-sha512");
        assert_eq!(scratch(dir.path(), "signed.in"), b"payload");
        let args = String::from_utf8(scratch(dir.path(), "args")).unwrap();
        assert!(args.contains("--local-user jane@x"), "{args}");
    }

    #[test]
    fn encrypt_reports_missing_keys() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] INV_RECP 0 bob@nowhere" >&2
exit 2"#,
        );
        let err = encrypt(&cfg, &["bob@nowhere".into()], false, b"x").unwrap_err();
        assert!(err.to_string().contains("bob@nowhere"));
    }

    #[test]
    fn encrypt_passes_recipients_and_sign() {
        let (dir, cfg) = stub(
            r#"cat >/dev/null
echo "$@" > "$D/args"
printf -- '-----BEGIN PGP MESSAGE-----\nCCC\n-----END PGP MESSAGE-----\n'"#,
        );
        let armor = encrypt(&cfg, &["bob@x".into(), "jane@x".into()], true, b"data").unwrap();
        assert!(armor.contains("BEGIN PGP MESSAGE"));
        let args = String::from_utf8(scratch(dir.path(), "args")).unwrap();
        assert!(args.contains("--recipient bob@x"), "{args}");
        assert!(args.contains("--recipient jane@x"), "{args}");
        assert!(args.contains("--sign"), "{args}");
        assert!(args.contains("--trust-model always"), "{args}");
    }

    const MIME_ENCRYPTED: &str = concat!(
        "From: a@x\r\n",
        "Subject: sealed\r\n",
        "Content-Type: multipart/encrypted; boundary=\"b\";\r\n",
        "\tprotocol=\"application/pgp-encrypted\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: application/pgp-encrypted\r\n",
        "\r\n",
        "Version: 1\r\n",
        "--b\r\n",
        "Content-Type: application/octet-stream\r\n",
        "\r\n",
        "-----BEGIN PGP MESSAGE-----\r\n",
        "XYZ\r\n",
        "-----END PGP MESSAGE-----\r\n",
        "--b--\r\n",
    );

    #[test]
    fn view_decrypts_pgp_mime() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2
printf 'Content-Type: text/plain\r\n\r\nthe secret plan\r\n'"#,
        );
        let v = view(&cfg, MIME_ENCRYPTED.as_bytes()).unwrap();
        assert!(body_text(&v).contains("the secret plan"));
        assert!(v.note.contains("decrypted"), "{}", v.note);
        assert!(
            v.note.contains("good signature from Jane <j@x>"),
            "{}",
            v.note
        );
    }

    #[test]
    fn view_reports_decryption_failure_keeping_body() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_FAILED" >&2
exit 2"#,
        );
        let v = view(&cfg, MIME_ENCRYPTED.as_bytes()).unwrap();
        assert!(v.opened.is_none());
        assert!(v.note.contains("decryption failed"), "{}", v.note);
    }

    /// MIME_ENCRYPTED's two parts inside a multipart/mixed, the way a
    /// mailing list delivers it with its footer appended.
    const NESTED_ENCRYPTED: &str = concat!(
        "From: a@x\r\n",
        "Content-Type: multipart/mixed; boundary=\"o\"\r\n",
        "\r\n",
        "--o\r\n",
        "Content-Type: multipart/encrypted; boundary=\"b\";\r\n",
        "\tprotocol=\"application/pgp-encrypted\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: application/pgp-encrypted\r\n",
        "\r\n",
        "Version: 1\r\n",
        "--b\r\n",
        "Content-Type: application/octet-stream\r\n",
        "\r\n",
        "-----BEGIN PGP MESSAGE-----\r\n",
        "XYZ\r\n",
        "-----END PGP MESSAGE-----\r\n",
        "--b--\r\n",
        "--o\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "list footer\r\n",
        "--o--\r\n",
    );

    #[test]
    fn view_decrypts_pgp_mime_inside_a_multipart() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2
printf 'Content-Type: multipart/mixed; boundary="in"\r\n\r\n--in\r\nContent-Type: text/plain\r\n\r\nthe secret plan\r\n--in\r\nContent-Type: application/pdf; name=x.pdf\r\n\r\nPDF\r\n--in--\r\n'"#,
        );
        let v = view(&cfg, NESTED_ENCRYPTED.as_bytes()).unwrap();
        assert_eq!(
            v.note,
            "[-- PGP: decrypted; good signature from Jane <j@x> --]"
        );
        // The decrypted tree stands where the encrypted part stood,
        // the footer after it.
        let Some(raw) = &v.opened else {
            panic!("an entity");
        };
        let mail = parse_mail(raw).unwrap();
        let types: Vec<&str> = mail
            .subparts
            .iter()
            .map(|p| p.ctype.mimetype.as_str())
            .collect();
        assert_eq!(types, ["multipart/mixed", "text/plain"]);
        let inner = &mail.subparts[0].subparts;
        assert_eq!(inner[0].get_body().unwrap(), "the secret plan");
        assert_eq!(inner[1].ctype.params["name"], "x.pdf");
        assert_eq!(mail.subparts[1].get_body().unwrap(), "list footer");
    }

    #[test]
    fn view_gives_nested_plaintext_without_headers_a_text_part() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf 'just words\n'"#,
        );
        let v = view(&cfg, NESTED_ENCRYPTED.as_bytes()).unwrap();
        let Some(raw) = &v.opened else {
            panic!("an entity");
        };
        let mail = parse_mail(raw).unwrap();
        assert_eq!(mail.subparts[0].ctype.mimetype, "text/plain");
        assert_eq!(mail.subparts[0].get_body().unwrap(), "just words");
        assert_eq!(mail.subparts[1].get_body().unwrap(), "list footer");
    }

    #[test]
    fn view_reports_nested_decryption_failure_keeping_body() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_FAILED" >&2
echo "gpg: decryption failed: No secret key" >&2
exit 2"#,
        );
        let v = view(&cfg, NESTED_ENCRYPTED.as_bytes()).unwrap();
        assert!(v.opened.is_none());
        assert_eq!(v.note, "[-- PGP: decryption failed: No secret key --]");
    }

    const MIME_SIGNED: &str = concat!(
        "From: a@x\r\n",
        "Content-Type: multipart/signed; boundary=\"b\";\r\n",
        "\tmicalg=pgp-sha256; protocol=\"application/pgp-signature\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "hello\r\n",
        "signed text\r\n",
        "--b\r\n",
        "Content-Type: application/pgp-signature\r\n",
        "\r\n",
        "-----BEGIN PGP SIGNATURE-----\r\n",
        "SSS\r\n",
        "-----END PGP SIGNATURE-----\r\n",
        "--b--\r\n",
    );

    #[test]
    fn view_verifies_mime_signed_over_exact_part_bytes() {
        let (dir, cfg) = stub(
            r#"cat > "$D/data.in"
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2"#,
        );
        let v = view(&cfg, MIME_SIGNED.as_bytes()).unwrap();
        assert!(v.opened.is_none());
        assert!(v.note.contains("good signature"), "{}", v.note);
        // The signed data is the exact first part, CRLF-canonical, with
        // the boundary's own CRLF excluded.
        assert_eq!(
            scratch(dir.path(), "data.in"),
            b"Content-Type: text/plain\r\n\r\nhello\r\nsigned text"
        );
    }

    #[test]
    fn view_retries_bad_signature_with_trailing_crlf() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
if [ -f "$D/second" ]; then
  echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2
else
  touch "$D/second"
  echo "[GNUPG:] BADSIG AAA Jane <j@x>" >&2
fi"#,
        );
        let v = view(&cfg, MIME_SIGNED.as_bytes()).unwrap();
        assert!(v.note.contains("good signature"), "{}", v.note);
    }

    #[test]
    fn view_handles_inline_and_clearsigned() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf 'inline secret'"#,
        );
        let msg =
            "From: a@x\r\n\r\n-----BEGIN PGP MESSAGE-----\r\nXYZ\r\n-----END PGP MESSAGE-----\r\n";
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert_eq!(body_text(&v), "inline secret");
        assert!(v.note.contains("decrypted"), "{}", v.note);

        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2
printf 'stripped text'"#,
        );
        let msg = "From: a@x\r\n\r\n-----BEGIN PGP SIGNED MESSAGE-----\r\nHash: SHA256\r\n\r\nstripped text\r\n-----BEGIN PGP SIGNATURE-----\r\nSSS\r\n-----END PGP SIGNATURE-----\r\n";
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert_eq!(body_text(&v), "stripped text");
        assert!(v.note.contains("good signature"), "{}", v.note);
    }

    /// gpg handing back a PGP/MIME plaintext with an attachment.
    const OPENS_TO_TEXT_AND_PDF: &str = r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf 'Content-Type: multipart/mixed; boundary="in"\r\n\r\n--in\r\nContent-Type: text/plain\r\n\r\nthe secret plan\r\n--in\r\nContent-Type: application/pdf; name=x.pdf\r\n\r\nPDF\r\n--in--\r\n'"#;

    #[test]
    fn opened_pgp_mime_keeps_the_envelope_and_lists_the_inner_parts() {
        let (_dir, cfg) = stub(OPENS_TO_TEXT_AND_PDF);
        let v = view(&cfg, MIME_ENCRYPTED.as_bytes()).unwrap();
        let raw = v.opened.as_ref().unwrap();
        let mail = parse_mail(raw).unwrap();
        // The outer header fields stay, the plaintext's Content-Type
        // takes the place of multipart/encrypted.
        use mailparse::MailHeaderMap as _;
        assert_eq!(mail.headers.get_first_value("Subject").unwrap(), "sealed");
        assert_eq!(mail.ctype.mimetype, "multipart/mixed");
        assert_eq!(mail.headers.get_all_values("Content-Type").len(), 1);
        // What a reply quotes and what the attachment menu lists.
        assert_eq!(body_text(&v).trim_end(), "the secret plan");
        let parts = message::parts_in(raw).unwrap();
        let names: Vec<_> = parts.iter().map(|p| p.filename.as_deref()).collect();
        assert_eq!(names, [None, Some("x.pdf")]);
        assert_eq!(message::part_bytes_in(raw, 1).unwrap(), b"PDF");
    }

    /// MIME_ENCRYPTED as Exchange delivers it: multipart/mixed, an
    /// empty text part in front.
    const EXCHANGE_MANGLED: &str = concat!(
        "From: a@x\r\n",
        "Content-Type: multipart/mixed; boundary=\"b\"\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "\r\n",
        "--b\r\n",
        "Content-Type: application/pgp-encrypted\r\n",
        "\r\n",
        "Version: 1\r\n",
        "--b\r\n",
        "Content-Type: application/octet-stream\r\n",
        "\r\n",
        "-----BEGIN PGP MESSAGE-----\r\n",
        "XYZ\r\n",
        "-----END PGP MESSAGE-----\r\n",
        "--b--\r\n",
    );

    #[test]
    fn view_decrypts_exchange_mangled_pgp_mime() {
        assert!(classify(EXCHANGE_MANGLED.as_bytes()).encrypted);
        let (_dir, cfg) = stub(OPENS_TO_TEXT_AND_PDF);
        let v = view(&cfg, EXCHANGE_MANGLED.as_bytes()).unwrap();
        assert_eq!(v.note, "[-- PGP: decrypted --]");
        assert_eq!(body_text(&v).trim_end(), "the secret plan");
        // Without the empty text part, too; with other parts beside
        // the two, it is an ordinary multipart.
        let two = EXCHANGE_MANGLED.replacen("--b\r\nContent-Type: text/plain\r\n\r\n\r\n", "", 1);
        assert!(view(&cfg, two.as_bytes()).unwrap().opened.is_some());
        let more = EXCHANGE_MANGLED.replacen("\r\n\r\n\r\n--b", "\r\n\r\nhello\r\n--b", 1);
        assert!(view(&cfg, more.as_bytes()).is_none());
    }

    #[test]
    fn view_finds_inline_armor_after_other_text() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf 'inline secret\n'"#,
        );
        let msg = "From: a@x\r\n\r\nHi,\r\n\r\n-----BEGIN PGP MESSAGE-----\r\nXYZ\r\n-----END PGP MESSAGE-----\r\nbye\r\n";
        assert!(classify(msg.as_bytes()).encrypted);
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert_eq!(v.note, "[-- PGP: decrypted --]");
        // The clear text around it stays, marked off from what was
        // encrypted.
        assert_eq!(
            body_text(&v),
            "Hi,\r\n\r\n[-- BEGIN PGP MESSAGE --]\ninline secret\n[-- END PGP MESSAGE --]\nbye\r\n"
        );
    }

    #[test]
    fn view_reads_inline_plaintext_in_the_declared_charset() {
        // "šťastný" in iso-8859-2.
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] BEGIN_DECRYPTION" >&2
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf '\271\273astn\375'"#,
        );
        let msg = "From: a@x\r\nContent-Type: text/plain; charset=iso-8859-2\r\n\r\n-----BEGIN PGP MESSAGE-----\r\nXYZ\r\n-----END PGP MESSAGE-----\r\n";
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert_eq!(body_text(&v), "šťastný");
        // Labelled us-ascii (the armor's own charset), it is UTF-8.
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
echo "[GNUPG:] DECRYPTION_OKAY" >&2
printf '\305\241\305\245astn\303\275'"#,
        );
        let v = view(&cfg, msg.replace("iso-8859-2", "us-ascii").as_bytes()).unwrap();
        assert_eq!(body_text(&v), "šťastný");
    }

    #[test]
    fn view_ignores_ordinary_mail() {
        let cfg = Pgp {
            command: "/nonexistent-gpg".into(),
            ..Default::default()
        };
        assert!(view(&cfg, b"From: a@x\r\n\r\nplain old mail\r\n").is_none());
        assert!(view(&cfg, MULTIPART_PLAIN.as_bytes()).is_none());
    }

    const MULTIPART_PLAIN: &str = concat!(
        "Content-Type: multipart/mixed; boundary=\"m\"\r\n",
        "\r\n",
        "--m\r\n",
        "Content-Type: text/plain\r\n",
        "\r\n",
        "nothing pgp here\r\n",
        "--m--\r\n",
    );

    /// sign_message output must verify: what gpg signed and what a
    /// receiver extracts for verification are byte-identical.
    #[test]
    fn sign_message_roundtrips_through_view() {
        let (dir, cfg) = stub(
            r#"case "$*" in
*--detach-sign*)
  cat > "$D/signed.in"
  echo "[GNUPG:] SIG_CREATED D 1 8 00 12 FPR" >&2
  printf -- '-----BEGIN PGP SIGNATURE-----\nAAA\n-----END PGP SIGNATURE-----\n' ;;
*--verify*)
  cat > "$D/verify.in"
  echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2 ;;
esac"#,
        );
        let draft = "To: bob@x\nFrom: jane@x\nSubject: s\n\nline one\nline two\n";
        let msg = sign_message(&cfg, draft, false).unwrap();
        assert!(msg.contains("Content-Type: multipart/signed"));
        assert!(msg.contains("micalg=pgp-sha256"));
        let mail = parse_mail(msg.as_bytes()).unwrap();
        assert_eq!(mail.subparts.len(), 2);
        assert_eq!(
            mail.subparts[0].get_body().unwrap(),
            "line one\r\nline two\r\n"
        );
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert!(v.note.contains("good signature"), "{}", v.note);
        assert_eq!(
            scratch(dir.path(), "signed.in"),
            scratch(dir.path(), "verify.in")
        );
    }

    /// Signing a multipart entity (draft with attachments) keeps the
    /// entity byte-identical and verifiable, like the text/plain case.
    #[test]
    fn sign_entity_roundtrips_a_multipart() {
        let (dir, cfg) = stub(
            r#"case "$*" in
*--detach-sign*)
  cat > "$D/signed.in"
  echo "[GNUPG:] SIG_CREATED D 1 8 00 12 FPR" >&2
  printf -- '-----BEGIN PGP SIGNATURE-----\nAAA\n-----END PGP SIGNATURE-----\n' ;;
*--verify*)
  cat > "$D/verify.in"
  echo "[GNUPG:] GOODSIG AAA Jane <j@x>" >&2 ;;
esac"#,
        );
        let entity = "Content-Type: multipart/mixed; boundary=\"mm\"\r\n\r\n\
                      --mm\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhi\r\n\
                      --mm\r\nContent-Type: application/pdf\r\n\r\ndata\r\n--mm--\r\n";
        let msg = sign_entity(
            &cfg,
            "To: bob@x\nFrom: jane@x\nSubject: s",
            entity.as_bytes(),
        )
        .unwrap();
        let mail = parse_mail(msg.as_bytes()).unwrap();
        assert_eq!(mail.ctype.mimetype, "multipart/signed");
        assert_eq!(mail.subparts[0].ctype.mimetype, "multipart/mixed");
        assert_eq!(mail.subparts[0].subparts.len(), 2);
        let v = view(&cfg, msg.as_bytes()).unwrap();
        assert!(v.note.contains("good signature"), "{}", v.note);
        assert_eq!(
            scratch(dir.path(), "signed.in"),
            scratch(dir.path(), "verify.in")
        );
    }

    #[test]
    fn encrypt_message_builds_rfc3156_shape() {
        let (_dir, cfg) = stub(
            r#"cat >/dev/null
printf -- '-----BEGIN PGP MESSAGE-----\nCCC\n-----END PGP MESSAGE-----\n'"#,
        );
        let draft = "To: bob@x\nFrom: jane@x\nSubject: s\n\ntop secret\n";
        let msg = encrypt_message(
            &cfg,
            &["bob@x".into(), "jane@x".into()],
            false,
            draft,
            false,
        )
        .unwrap();
        assert!(msg.contains("Content-Type: multipart/encrypted"));
        assert!(msg.contains("Subject: s"), "headers stay in clear");
        assert!(!msg.contains("top secret"), "body must not leak");
        let mail = parse_mail(msg.as_bytes()).unwrap();
        assert_eq!(mail.subparts.len(), 2);
        assert_eq!(mail.subparts[0].get_body().unwrap().trim(), "Version: 1");
        assert!(
            mail.subparts[1]
                .get_body()
                .unwrap()
                .contains("BEGIN PGP MESSAGE")
        );
    }
}

#[cfg(test)]
mod classify_tests {
    use super::classify;

    #[test]
    fn classify_reads_the_mime_type_only() {
        let enc = b"Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=b\r\n\r\n--b\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--b--\r\n";
        let c = classify(enc);
        assert!(c.encrypted && !c.signed);
        let sig = b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nhi\r\n--b--\r\n";
        let c = classify(sig);
        assert!(c.signed && !c.encrypted);
        let inline = b"Content-Type: text/plain\r\n\r\n-----BEGIN PGP MESSAGE-----\r\nxx\r\n-----END PGP MESSAGE-----\r\n";
        assert!(classify(inline).encrypted);
        let nested = b"Content-Type: multipart/mixed; boundary=o\r\n\r\n--o\r\nContent-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=b\r\n\r\n--b\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--b\r\nContent-Type: application/octet-stream\r\n\r\nxx\r\n--b--\r\n--o\r\nContent-Type: text/plain\r\n\r\nfooter\r\n--o--\r\n";
        let c = classify(nested);
        assert!(c.encrypted && !c.signed);
        let plain = b"Content-Type: text/plain\r\n\r\nnothing here\r\n";
        let c = classify(plain);
        assert!(!c.signed && !c.encrypted);
    }
}
