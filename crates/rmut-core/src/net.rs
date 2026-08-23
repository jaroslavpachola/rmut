//! Shared plumbing for the IMAP and SMTP clients: a stream that is
//! either plain TCP or TLS (rustls), plus a buffered line reader whose
//! writes go straight through to the socket.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    /// Recover the TCP stream to upgrade it (STARTTLS).
    pub(crate) fn into_tcp(self) -> Result<TcpStream> {
        match self {
            Stream::Plain(tcp) => Ok(tcp),
            Stream::Tls(_) => bail!("connection is already TLS"),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// True when the error means the connection itself died (dropped
/// socket, EOF) rather than the server saying NO: the caller may
/// reconnect and retry the command once.
pub(crate) fn is_connection_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some()
        || err
            .chain()
            .any(|c| c.to_string().contains("server closed the connection"))
}

/// True when the error is the socket's read timeout firing (the IDLE
/// wait uses it as a tick to check its stop flag).
pub(crate) fn is_timeout(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>().is_some_and(|e| {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    })
}

pub(crate) fn connect(host: &str, port: u16, tls: bool) -> Result<Stream> {
    let tcp =
        TcpStream::connect((host, port)).with_context(|| format!("connecting to {host}:{port}"))?;
    tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
    tcp.set_write_timeout(Some(Duration::from_secs(60)))?;
    if tls {
        wrap_tls(tcp, host)
    } else {
        Ok(Stream::Plain(tcp))
    }
}

pub(crate) fn wrap_tls(tcp: TcpStream, host: &str) -> Result<Stream> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .with_context(|| format!("invalid server name {host}"))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), name)
        .with_context(|| format!("setting up TLS to {host}"))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
}

/// Buffered reader over a `Stream`; writes bypass the read buffer.
pub(crate) struct Conn {
    stream: Stream,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl Conn {
    pub(crate) fn new(stream: Stream) -> Conn {
        Conn {
            stream,
            buf: vec![0; 8192],
            start: 0,
            end: 0,
        }
    }

    /// Give the stream back (STARTTLS); the read buffer must be empty.
    pub(crate) fn into_stream(self) -> Stream {
        self.stream
    }

    fn fill(&mut self) -> Result<()> {
        self.start = 0;
        self.end = loop {
            match self.stream.read(&mut self.buf) {
                // A signal (e.g. SIGCHLD from a gpg child) can interrupt
                // the read; that is not an error, try again.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                other => break other.context("reading from server")?,
            }
        };
        if self.end == 0 {
            bail!("server closed the connection");
        }
        Ok(())
    }

    pub(crate) fn read_byte(&mut self) -> Result<u8> {
        if self.start == self.end {
            self.fill()?;
        }
        let b = self.buf[self.start];
        self.start += 1;
        Ok(b)
    }

    /// Append exactly `n` bytes to `out`.
    pub(crate) fn read_exact_to(&mut self, out: &mut Vec<u8>, n: usize) -> Result<()> {
        let mut left = n;
        while left > 0 {
            if self.start == self.end {
                self.fill()?;
            }
            let take = left.min(self.end - self.start);
            out.extend_from_slice(&self.buf[self.start..self.start + take]);
            self.start += take;
            left -= take;
        }
        Ok(())
    }

    /// One CRLF- (or bare LF-) terminated line, without the terminator.
    pub(crate) fn read_text_line(&mut self) -> Result<String> {
        let mut bytes = Vec::new();
        loop {
            let b = self.read_byte()?;
            if b == b'\n' {
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                break;
            }
            bytes.push(b);
            ensure!(bytes.len() <= 1 << 20, "response line too long");
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub(crate) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes).context("writing to server")?;
        self.stream.flush().context("writing to server")?;
        Ok(())
    }
}
