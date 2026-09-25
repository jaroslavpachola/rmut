//! Minimal SMTP submission client: EHLO, STARTTLS or implicit TLS,
//! AUTH PLAIN, then MAIL/RCPT/DATA with dot-stuffing. An alternative
//! to handing mail to sendmail(1).

use anyhow::{Context, Result, ensure};

use crate::config::{Account, AuthKind};
use crate::maildir;
use crate::net::{self, Conn};

/// What a submission says beside the message: mutt's
/// $use_envelope_from / $envelope_from_address and $dsn_notify /
/// $dsn_return. The default asks for nothing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Envelope {
    /// The envelope sender to insist on: sendmail's `-f`, SMTP's MAIL
    /// FROM in place of the message's From.
    pub sender: Option<String>,
    /// DSN NOTIFY, e.g. "failure,delay".
    pub notify: Option<String>,
    /// DSN RET, "hdrs" or "full".
    pub ret: Option<String>,
}

/// Submit `body` (any line endings; normalized to CRLF on the wire)
/// for delivery to `rcpts`, authenticating as the account's user.
/// The DSN requests go along only to a server that offers DSN, as in
/// mutt; one that does not would refuse the command.
pub fn send(
    account: &Account,
    password: &str,
    from: &str,
    rcpts: &[String],
    body: &[u8],
    envelope: &Envelope,
) -> Result<()> {
    let host = account
        .smtp_host
        .as_deref()
        .with_context(|| format!("account {} has no smtp_host", account.name))?;
    ensure!(!rcpts.is_empty(), "no recipients");
    // Port 465 is TLS from the first byte; anything else negotiates
    // STARTTLS (unless smtp_tls = false, for tests).
    let implicit_tls = account.smtp_tls && account.smtp_port == 465;
    let mut conn = Conn::new(
        net::connect(
            host,
            account.smtp_port,
            implicit_tls,
            &net::Cutoff::default(),
        )?,
        format!("{host}:{}", account.smtp_port),
    );
    expect(&mut conn, 220).context("SMTP greeting")?;
    let mut caps = ehlo(&mut conn)?;
    if account.smtp_tls && !implicit_tls {
        command(&mut conn, "STARTTLS", 220)?;
        let tcp = conn.into_stream().into_tcp()?;
        conn = Conn::new(
            net::wrap_tls(tcp, host)?,
            format!("{host}:{}", account.smtp_port),
        );
        caps = ehlo(&mut conn)?;
    }
    authenticate(&mut conn, &caps, account, password).context("SMTP authentication")?;
    let from = envelope.sender.as_deref().unwrap_or(from);
    let dsn = offers(&caps, "DSN");
    let mut mail_from = format!("MAIL FROM:<{from}>");
    if let Some(ret) = envelope.ret.as_deref().filter(|_| dsn) {
        mail_from += &format!(" RET={}", ret.to_ascii_uppercase());
    }
    command(&mut conn, &mail_from, 250)?;
    for rcpt in rcpts {
        let mut rcpt_to = format!("RCPT TO:<{rcpt}>");
        if let Some(notify) = envelope.notify.as_deref().filter(|_| dsn) {
            rcpt_to += &format!(" NOTIFY={}", notify.to_ascii_uppercase());
        }
        command(&mut conn, &rcpt_to, 250).with_context(|| format!("recipient {rcpt}"))?;
    }
    command(&mut conn, "DATA", 354)?;
    conn.write_all(&dot_stuff(body))?;
    expect(&mut conn, 250).context("message rejected after DATA")?;
    let _ = conn.write_all(b"QUIT\r\n");
    Ok(())
}

/// Whether the EHLO reply (its lines joined by "; ", each starting
/// with the "250-"/"250 " code) names this extension.
fn offers(caps: &str, extension: &str) -> bool {
    caps.split("; ")
        .filter_map(|line| line.get(4..))
        .any(|line| {
            line.split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(extension))
        })
}

fn ehlo(conn: &mut Conn) -> Result<String> {
    command(conn, &format!("EHLO {}", maildir::hostname()), 250)
}

/// AUTH PLAIN (or LOGIN when the server's EHLO offered only that) with
/// the password; the OAuth kinds run their SASL mechanism with the
/// access token in `secret`.
fn authenticate(conn: &mut Conn, caps: &str, account: &Account, secret: &str) -> Result<()> {
    let user = &account.user;
    match account.auth_kind()? {
        AuthKind::Password => {}
        kind => {
            let host = account.smtp_host.as_deref().unwrap_or_default();
            let initial = kind.initial_response(user, secret, host, account.smtp_port);
            command(conn, &format!("AUTH {}", kind.sasl_name()), 334)?;
            command(conn, &b64(initial.as_bytes()), 235)?;
            return Ok(());
        }
    }
    // `caps` is the EHLO reply with its lines joined by "; ", each
    // starting with the "250-"/"250 " code.
    let caps = caps.to_ascii_uppercase();
    let mechanisms = caps
        .split("; ")
        .filter_map(|line| line.get(4..))
        .find_map(|line| line.trim_start().strip_prefix("AUTH "));
    let login_only = mechanisms.is_some_and(|m| m.contains("LOGIN") && !m.contains("PLAIN"));
    if login_only {
        command(conn, "AUTH LOGIN", 334)?;
        command(conn, &b64(user.as_bytes()), 334)?;
        command(conn, &b64(secret.as_bytes()), 235)?;
    } else {
        let token = b64(format!("\0{user}\0{secret}").as_bytes());
        command(conn, &format!("AUTH PLAIN {token}"), 235)?;
    }
    Ok(())
}

fn command(conn: &mut Conn, cmd: &str, want: u16) -> Result<String> {
    conn.write_all(format!("{cmd}\r\n").as_bytes())?;
    expect(conn, want)
}

/// Read one (possibly multi-line) reply and require the given code;
/// 251 passes for 250 (forwarded recipient).
fn expect(conn: &mut Conn, want: u16) -> Result<String> {
    let mut text = String::new();
    loop {
        let line = conn.read_text_line()?;
        ensure!(line.len() >= 3, "short SMTP reply: {line}");
        let code: u16 = line[..3]
            .parse()
            .with_context(|| format!("malformed SMTP reply: {line}"))?;
        if !text.is_empty() {
            text.push_str("; ");
        }
        text.push_str(&line);
        if line.as_bytes().get(3) == Some(&b'-') {
            continue;
        }
        ensure!(
            code == want || (want == 250 && code == 251),
            "server said: {text}"
        );
        return Ok(text);
    }
}

/// CRLF-normalize, escape leading dots, and add the `.` terminator.
fn dot_stuff(body: &[u8]) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = body.split(|&b| b == b'\n').collect();
    if lines.last() == Some(&&b""[..]) {
        lines.pop();
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    for line in lines {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.first() == Some(&b'.') {
            out.push(b'.');
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    out
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64_ALPHABET[(n >> (18 - 6 * i)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{self, Expect};

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"\0jane\0secret"), "AGphbmUAc2VjcmV0");
    }

    #[test]
    fn dot_stuff_escapes_and_terminates() {
        assert_eq!(
            dot_stuff(b"hi\n.dot\nend\n"),
            b"hi\r\n..dot\r\nend\r\n.\r\n"
        );
        assert_eq!(
            dot_stuff(b"already\r\ncrlf\r\n"),
            b"already\r\ncrlf\r\n.\r\n"
        );
        assert_eq!(
            dot_stuff(b"no trailing newline"),
            b"no trailing newline\r\n.\r\n"
        );
        assert_eq!(dot_stuff(b""), b".\r\n");
    }

    #[test]
    fn session_against_scripted_server() {
        let (port, handle, log) = testserver::smtp(vec![
            Expect::new(
                "EHLO",
                "250-test.example\r\n250 AUTH PLAIN LOGIN\r\n".into(),
            ),
            Expect::new("AUTH PLAIN AGphbmUAc2VjcmV0", "235 ok\r\n".into()),
            Expect::new("MAIL FROM:<jane@example.com>", "250 ok\r\n".into()),
            Expect::new("RCPT TO:<bob@example.org>", "250 ok\r\n".into()),
            Expect::new("RCPT TO:<carol@example.org>", "251 forwarded\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let account = crate::config::Account {
            name: "t".into(),
            user: "jane".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
        };
        send(
            &account,
            "secret",
            "jane@example.com",
            &["bob@example.org".into(), "carol@example.org".into()],
            b"Subject: hi\n\n.leading dot\nbye\n",
            &Envelope::default(),
        )
        .unwrap();
        handle.join().unwrap();
        let log = log.lock().unwrap();
        let payload = log.iter().find(|l| l.contains("Subject")).unwrap();
        assert!(payload.contains("..leading dot"));
    }

    #[test]
    fn xoauth2_runs_the_sasl_exchange() {
        let (port, handle, _log) = testserver::smtp(vec![
            Expect::new("EHLO", "250-x\r\n250 AUTH XOAUTH2\r\n".into()),
            Expect::new("AUTH XOAUTH2", "334 \r\n".into()),
            // XOAUTH2 for user=jane token=tok, precomputed base64.
            Expect::new("dXNlcj1qYW5lAWF1dGg9QmVhcmVyIHRvawEB", "235 ok\r\n".into()),
            Expect::new("MAIL FROM:<jane@example.com>", "250 ok\r\n".into()),
            Expect::new("RCPT TO:<bob@example.org>", "250 ok\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let account = crate::config::Account {
            auth: Some("xoauth2".into()),
            ..oauth_test_account(port)
        };
        send(
            &account,
            "tok",
            "jane@example.com",
            &["bob@example.org".into()],
            b"Subject: hi\n\nbody\n",
            &Envelope::default(),
        )
        .unwrap();
        handle.join().unwrap();
    }

    #[test]
    fn envelope_sender_and_dsn_when_offered() {
        let (port, handle, log) = testserver::smtp(vec![
            Expect::new("EHLO", "250-x\r\n250-DSN\r\n250 AUTH PLAIN\r\n".into()),
            Expect::new("AUTH PLAIN", "235 ok\r\n".into()),
            Expect::new("MAIL FROM:<bounces@example.com>", "250 ok\r\n".into()),
            Expect::new("RCPT TO:<bob@example.org>", "250 ok\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let envelope = Envelope {
            sender: Some("bounces@example.com".into()),
            notify: Some("failure,delay".into()),
            ret: Some("hdrs".into()),
        };
        send(
            &oauth_test_account(port),
            "secret",
            "jane@example.com",
            &["bob@example.org".into()],
            b"Subject: hi\n\nbody\n",
            &envelope,
        )
        .unwrap();
        handle.join().unwrap();
        let log = log.lock().unwrap();
        assert!(log.contains(&"MAIL FROM:<bounces@example.com> RET=HDRS".to_string()));
        assert!(log.contains(&"RCPT TO:<bob@example.org> NOTIFY=FAILURE,DELAY".to_string()));
    }

    #[test]
    fn no_dsn_parameters_to_a_server_without_it() {
        let (port, handle, log) = testserver::smtp(vec![
            Expect::new("EHLO", "250-x\r\n250 AUTH PLAIN\r\n".into()),
            Expect::new("AUTH PLAIN", "235 ok\r\n".into()),
            Expect::new("MAIL FROM:<jane@example.com>", "250 ok\r\n".into()),
            Expect::new("RCPT TO:<bob@example.org>", "250 ok\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let envelope = Envelope {
            sender: None,
            notify: Some("never".into()),
            ret: Some("full".into()),
        };
        send(
            &oauth_test_account(port),
            "secret",
            "jane@example.com",
            &["bob@example.org".into()],
            b"Subject: hi\n\nbody\n",
            &envelope,
        )
        .unwrap();
        handle.join().unwrap();
        let log = log.lock().unwrap();
        assert!(log.contains(&"MAIL FROM:<jane@example.com>".to_string()));
        assert!(log.contains(&"RCPT TO:<bob@example.org>".to_string()));
    }

    fn oauth_test_account(port: u16) -> crate::config::Account {
        crate::config::Account {
            name: "t".into(),
            user: "jane".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
        }
    }

    #[test]
    fn falls_back_to_auth_login() {
        let (port, handle, _log) = testserver::smtp(vec![
            Expect::new("EHLO", "250-fake\r\n250 AUTH LOGIN\r\n".into()),
            Expect::new("AUTH LOGIN", "334 VXNlcm5hbWU6\r\n".into()),
            Expect::new(b64_static(b"jane"), "334 UGFzc3dvcmQ6\r\n".into()),
            Expect::new(b64_static(b"secret"), "235 ok\r\n".into()),
            Expect::new("MAIL FROM", "250 ok\r\n".into()),
            Expect::new("RCPT TO", "250 ok\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let account = crate::config::Account {
            name: "t".into(),
            user: "jane".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
        };
        send(
            &account,
            "secret",
            "jane@x",
            &["bob@y".into()],
            b"hi\n",
            &Envelope::default(),
        )
        .unwrap();
        handle.join().unwrap();
    }

    // Leak a b64 value so Expect's &'static str signature is satisfied.
    fn b64_static(input: &[u8]) -> &'static str {
        Box::leak(b64(input).into_boxed_str())
    }

    #[test]
    fn rejected_recipient_is_an_error() {
        let (port, handle, _log) = testserver::smtp(vec![
            Expect::new("EHLO", "250 test.example\r\n".into()),
            Expect::new("AUTH PLAIN", "235 ok\r\n".into()),
            Expect::new("MAIL FROM", "250 ok\r\n".into()),
            Expect::new("RCPT TO", "550 no such user\r\n".into()),
        ]);
        let account = crate::config::Account {
            name: "t".into(),
            user: "jane".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            auth: None,
            token_command: None,
            sent_folder: "Sent".into(),
            identity: None,
        };
        let err = send(
            &account,
            "s",
            "jane@x",
            &["bob@y".into()],
            b"hi",
            &Envelope::default(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("no such user"));
        handle.join().unwrap();
    }
}
