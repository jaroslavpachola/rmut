//! PGP support by shelling out to gpg(1): decrypt and verify messages
//! for the pager, sign and encrypt outgoing drafts. Handles PGP/MIME
//! (RFC 3156) and inline ("armor in the body") messages. Passphrases
//! are between gpg and its agent; rmut never sees or stores them.

use std::io::Write as _;
use std::path::PathBuf;
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
        return Err(out.error("decryption failed"));
    }
    if !was_encrypted && sig.is_none() && !out.success {
        return Err(out.error("gpg failed"));
    }
    Ok(Opened {
        plaintext: out.stdout,
        sig,
    })
}

/// Verify a detached signature over `data` (already in the CRLF form
/// it was signed in). gpg wants the signature as a file argument.
pub fn verify_detached(cfg: &Pgp, signature: &[u8], data: &[u8]) -> Result<Sig> {
    let sigfile = temp_path("sig");
    std::fs::write(&sigfile, signature)
        .with_context(|| format!("writing {}", sigfile.display()))?;
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

/// What the pager should show for a PGP message: a replacement body
/// (when decryption produced one) and a one-line status note. gpg
/// trouble goes in the note; the original body stays available.
pub struct View {
    pub body: Option<Body>,
    pub note: String,
}

/// What decryption produced, when it produced anything.
pub enum Body {
    /// Plain text, from inline PGP: show it as it stands.
    Text(String),
    /// A MIME entity, from PGP/MIME: the caller renders the tree, so
    /// attachments inside encrypted mail show like any others.
    Entity(Vec<u8>),
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

/// Inspect a raw message; Some when it is PGP-encrypted or signed in
/// any of the four shapes (PGP/MIME encrypted or signed, inline
/// encrypted, clearsigned).
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
    if mail.ctype.mimetype == "multipart/encrypted" {
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
    // Inline PGP: look at the text body's opening marker.
    if let Some(text) = message::extract_text(&mail) {
        let t = text.trim_start();
        if t.starts_with("-----BEGIN PGP MESSAGE-----") {
            return Crypto {
                encrypted: true,
                signed: false,
            };
        }
        if t.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
            return Crypto {
                signed: true,
                encrypted: false,
            };
        }
    }
    Crypto::default()
}

pub fn view(cfg: &Pgp, raw: &[u8]) -> Option<View> {
    let mail = parse_mail(raw).ok()?;
    if mail.ctype.mimetype == "multipart/encrypted" && mail.subparts.len() >= 2 {
        return Some(view_mime_encrypted(cfg, &mail));
    }
    if mail.ctype.mimetype == "multipart/signed"
        && mail.ctype.params.get("protocol").map(String::as_str)
            == Some("application/pgp-signature")
        && mail.subparts.len() >= 2
    {
        return Some(view_mime_signed(cfg, &mail));
    }
    let text = message::extract_text(&mail)?;
    let trimmed = text.trim_start();
    if trimmed.starts_with("-----BEGIN PGP MESSAGE-----") {
        return Some(view_inline(cfg, trimmed, true));
    }
    if trimmed.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
        return Some(view_inline(cfg, trimmed, false));
    }
    None
}

fn view_mime_encrypted(cfg: &Pgp, mail: &ParsedMail) -> View {
    let cipher = match mail.subparts[1].get_body_raw() {
        Ok(c) => c,
        Err(err) => {
            return View {
                body: None,
                note: note(&format!("cannot read the encrypted part: {err}")),
            };
        }
    };
    match decrypt(cfg, &cipher) {
        Ok(opened) => {
            let text = match &opened.sig {
                Some(sig) => format!("decrypted; {}", sig_phrase(sig)),
                None => "decrypted".into(),
            };
            View {
                // The plaintext is itself a MIME entity.
                body: Some(Body::Entity(opened.plaintext)),
                note: note(&text),
            }
        }
        Err(err) => View {
            body: None,
            note: note(&format!("decryption failed: {err:#}")),
        },
    }
}

fn view_mime_signed(cfg: &Pgp, mail: &ParsedMail) -> View {
    let signed = crlf(mail.subparts[0].raw_bytes);
    let sig_armor = match mail.subparts[1].get_body_raw() {
        Ok(s) => s,
        Err(err) => {
            return View {
                body: None,
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
        body: None,
        note: match verdict {
            Ok(sig) => note(&sig_phrase(&sig)),
            Err(err) => note(&format!("cannot verify signature: {err:#}")),
        },
    }
}

fn view_inline(cfg: &Pgp, text: &str, encrypted: bool) -> View {
    match decrypt(cfg, text.as_bytes()) {
        Ok(opened) => {
            let phrase = match (&opened.sig, encrypted) {
                (Some(sig), true) => format!("decrypted; {}", sig_phrase(sig)),
                (Some(sig), false) => sig_phrase(sig),
                (None, true) => "decrypted".into(),
                (None, false) => "signed, no verdict from gpg".into(),
            };
            View {
                body: Some(Body::Text(
                    String::from_utf8_lossy(&opened.plaintext).into_owned(),
                )),
                note: note(&phrase),
            }
        }
        Err(err) => View {
            body: None,
            note: note(&format!(
                "{} failed: {err:#}",
                if encrypted {
                    "decryption"
                } else {
                    "verification"
                }
            )),
        },
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

fn temp_path(what: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!(
        "rmut-{what}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    /// Whatever decryption produced, as text (a PGP/MIME entity comes
    /// back raw, headers and all).
    fn body_text(v: &View) -> String {
        match v.body.as_ref().expect("a decrypted body") {
            Body::Text(t) => t.clone(),
            Body::Entity(raw) => String::from_utf8_lossy(raw).into_owned(),
        }
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
        assert!(v.body.is_none());
        assert!(v.note.contains("decryption failed"), "{}", v.note);
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
        assert!(v.body.is_none());
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
        let plain = b"Content-Type: text/plain\r\n\r\nnothing here\r\n";
        let c = classify(plain);
        assert!(!c.signed && !c.encrypted);
    }
}
