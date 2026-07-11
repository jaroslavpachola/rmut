//! Scripted IMAP and SMTP servers on a localhost port, for client
//! tests. Each `Expect` matches one incoming command by substring and
//! plays back a canned reply; a mismatch panics in the server thread
//! and surfaces when the test joins the handle.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub(crate) struct Expect {
    /// Required substring of the received command line.
    cmd: &'static str,
    /// Extra response data sent verbatim before the tagged/final reply
    /// (IMAP untagged lines, with explicit `\r\n`).
    reply: String,
    /// IMAP: complete the command with NO instead of OK.
    fail: Option<&'static str>,
    /// IMAP: send only `reply`, no tagged completion — for IDLE, which
    /// is completed by a later DONE step.
    untagged: bool,
    /// IMAP: drop the connection instead of answering (reconnect
    /// tests); the next step is served on a fresh connection.
    drop_conn: bool,
}

impl Expect {
    pub(crate) fn new(cmd: &'static str, reply: String) -> Expect {
        Expect {
            cmd,
            reply,
            fail: None,
            untagged: false,
            drop_conn: false,
        }
    }

    pub(crate) fn fail(cmd: &'static str, status: &'static str) -> Expect {
        Expect {
            cmd,
            reply: String::new(),
            fail: Some(status),
            untagged: false,
            drop_conn: false,
        }
    }

    pub(crate) fn untagged(cmd: &'static str, reply: String) -> Expect {
        Expect {
            cmd,
            reply,
            fail: None,
            untagged: true,
            drop_conn: false,
        }
    }

    pub(crate) fn drop_conn(cmd: &'static str) -> Expect {
        Expect {
            cmd,
            reply: String::new(),
            fail: None,
            untagged: false,
            drop_conn: true,
        }
    }
}

/// IMAP server; returns the port and the thread to join at test end.
/// When the client disconnects with script steps left, the server
/// accepts a fresh connection (reconnect tests). Writes are
/// best-effort — a LOGOUT reply may race the client's close.
pub(crate) fn imap(script: Vec<Expect>) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut steps = script.into_iter().peekable();
        let mut last_tag = String::from("*");
        while steps.peek().is_some() {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut stream = stream;
            let _ = stream.write_all(b"* OK rmut test server\r\n");
            while steps.peek().is_some() {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break; // client gone; next connection continues
                }
                let mut line = line.trim_end().to_string();
                // Client literals: answer the continuation, swallow the
                // bytes, and keep reading the same logical command.
                while let Some(n) = client_literal_len(&line) {
                    let _ = stream.write_all(b"+ go ahead\r\n");
                    let mut lit = vec![0u8; n];
                    reader.read_exact(&mut lit).unwrap();
                    let mut rest = String::new();
                    reader.read_line(&mut rest).unwrap();
                    line.push_str(String::from_utf8_lossy(&lit).trim_end());
                    line.push_str(rest.trim_end());
                }
                if line.contains("LOGOUT") && steps.peek().is_none_or(|s| s.cmd != "LOGOUT") {
                    // Unscripted logout from a dropped client.
                    break;
                }
                let step = steps.next().unwrap();
                assert!(
                    line.contains(step.cmd),
                    "server expected {:?}, got {line:?}",
                    step.cmd
                );
                if step.drop_conn {
                    break; // hang up; the next step waits on a new connection
                }
                // Untagged client lines (IDLE's DONE, SASL payloads)
                // complete with the tag of the command they belong to.
                // The client's tags are "aN" (and "rmut0" for STARTTLS).
                let first = line.split(' ').next().unwrap_or("*");
                let is_tag = first == "rmut0"
                    || (first.len() > 1
                        && first.starts_with('a')
                        && first[1..].bytes().all(|b| b.is_ascii_digit()));
                let tag = if is_tag {
                    last_tag = first.to_string();
                    last_tag.clone()
                } else {
                    last_tag.clone()
                };
                let _ = stream.write_all(step.reply.as_bytes());
                if !step.untagged {
                    let status = step.fail.unwrap_or("OK done");
                    let _ = stream.write_all(format!("{tag} {status}\r\n").as_bytes());
                }
                if line.contains("LOGOUT") {
                    break;
                }
            }
        }
    });
    (port, handle)
}

fn client_literal_len(line: &str) -> Option<usize> {
    let rest = line.strip_suffix('}')?;
    let open = rest.rfind('{')?;
    rest[open + 1..].parse().ok()
}
/// SMTP server; `log` collects every received command line and, after
/// DATA, the dot-stuffed message payload.
pub(crate) fn smtp(script: Vec<Expect>) -> (u16, JoinHandle<()>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let thread_log = Arc::clone(&log);
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut stream = stream;
        stream.write_all(b"220 rmut test server\r\n").unwrap();
        for step in script {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                panic!("client hung up; still expected {:?}", step.cmd);
            }
            let line = line.trim_end().to_string();
            assert!(
                line.contains(step.cmd),
                "server expected {:?}, got {line:?}",
                step.cmd
            );
            thread_log.lock().unwrap().push(line.clone());
            stream.write_all(step.reply.as_bytes()).unwrap();
            if line.eq_ignore_ascii_case("DATA") {
                let mut payload = String::new();
                loop {
                    let mut data_line = String::new();
                    reader.read_line(&mut data_line).unwrap();
                    if data_line.trim_end() == "." {
                        break;
                    }
                    payload.push_str(&data_line);
                }
                thread_log.lock().unwrap().push(payload);
                stream.write_all(b"250 accepted\r\n").unwrap();
            }
        }
    });
    (port, handle, log)
}
