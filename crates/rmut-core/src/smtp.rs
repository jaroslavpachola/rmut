//! Minimal SMTP submission client: EHLO, STARTTLS or implicit TLS,
//! AUTH PLAIN, then MAIL/RCPT/DATA with dot-stuffing. An alternative
//! to handing mail to sendmail(1).

use anyhow::{Context, Result, ensure};

use crate::config::Account;
use crate::maildir;
use crate::net::{self, Conn};

/// Submit `body` (any line endings; normalized to CRLF on the wire)
/// for delivery to `rcpts`, authenticating as the account's user.
pub fn send(
    account: &Account,
    password: &str,
    from: &str,
    rcpts: &[String],
    body: &[u8],
) -> Result<()> {
    let host = account
        .smtp_host
        .as_deref()
        .with_context(|| format!("account {} has no smtp_host", account.name))?;
    ensure!(!rcpts.is_empty(), "no recipients");
    // Port 465 is TLS from the first byte; anything else negotiates
    // STARTTLS (unless smtp_tls = false, for tests).
    let implicit_tls = account.smtp_tls && account.smtp_port == 465;
    let mut conn = Conn::new(net::connect(host, account.smtp_port, implicit_tls)?);
    expect(&mut conn, 220).context("SMTP greeting")?;
    let mut caps = ehlo(&mut conn)?;
    if account.smtp_tls && !implicit_tls {
        command(&mut conn, "STARTTLS", 220)?;
        let tcp = conn.into_stream().into_tcp()?;
        conn = Conn::new(net::wrap_tls(tcp, host)?);
        caps = ehlo(&mut conn)?;
    }
    authenticate(&mut conn, &caps, &account.user, password).context("SMTP authentication")?;
    command(&mut conn, &format!("MAIL FROM:<{from}>"), 250)?;
    for rcpt in rcpts {
        command(&mut conn, &format!("RCPT TO:<{rcpt}>"), 250)
            .with_context(|| format!("recipient {rcpt}"))?;
    }
    command(&mut conn, "DATA", 354)?;
    conn.write_all(&dot_stuff(body))?;
    expect(&mut conn, 250).context("message rejected after DATA")?;
    let _ = conn.write_all(b"QUIT\r\n");
    Ok(())
}

fn ehlo(conn: &mut Conn) -> Result<String> {
    command(conn, &format!("EHLO {}", maildir::hostname()), 250)
}

/// AUTH PLAIN, unless the server's EHLO offered only LOGIN.
fn authenticate(conn: &mut Conn, caps: &str, user: &str, password: &str) -> Result<()> {
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
        command(conn, &b64(password.as_bytes()), 235)?;
    } else {
        let token = b64(format!("\0{user}\0{password}").as_bytes());
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

fn b64(input: &[u8]) -> String {
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
            password_command: "unused".into(),
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            sent_folder: "Sent".into(),
        };
        send(
            &account,
            "secret",
            "jane@example.com",
            &["bob@example.org".into(), "carol@example.org".into()],
            b"Subject: hi\n\n.leading dot\nbye\n",
        )
        .unwrap();
        handle.join().unwrap();
        let log = log.lock().unwrap();
        let payload = log.iter().find(|l| l.contains("Subject")).unwrap();
        assert!(payload.contains("..leading dot"));
    }

    #[test]
    fn falls_back_to_auth_login() {
        let (port, handle, _log) = testserver::smtp(vec![
            Expect::new("EHLO", "250-fake\r\n250 AUTH LOGIN\r\n".into()),
            Expect::new("AUTH LOGIN", "334 VXNlcm5hbWU6\r\n".into()),
            Expect::new(&b64_static(b"jane"), "334 UGFzc3dvcmQ6\r\n".into()),
            Expect::new(&b64_static(b"secret"), "235 ok\r\n".into()),
            Expect::new("MAIL FROM", "250 ok\r\n".into()),
            Expect::new("RCPT TO", "250 ok\r\n".into()),
            Expect::new("DATA", "354 go\r\n".into()),
            Expect::new("QUIT", "221 bye\r\n".into()),
        ]);
        let account = crate::config::Account {
            name: "t".into(),
            user: "jane".into(),
            password_command: "unused".into(),
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            sent_folder: "Sent".into(),
        };
        send(&account, "secret", "jane@x", &["bob@y".into()], b"hi\n").unwrap();
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
            password_command: "unused".into(),
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: Some("127.0.0.1".into()),
            smtp_port: port,
            smtp_tls: false,
            sent_folder: "Sent".into(),
        };
        let err = send(&account, "s", "jane@x", &["bob@y".into()], b"hi").unwrap_err();
        assert!(format!("{err:#}").contains("no such user"));
        handle.join().unwrap();
    }
}
