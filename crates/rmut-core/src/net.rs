//! Shared plumbing for the IMAP and SMTP clients: a stream that is
//! either plain TCP or TLS (rustls), plus a buffered line reader whose
//! writes go straight through to the socket.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};

/// Seconds to wait for a connection, and for data on one. Process
/// wide, because the connections are made from threads (IDLE, the
/// backfill) that have the account but not the config; the session
/// sets them from the config at startup and after a `:set`.
static CONNECT_SECS: AtomicU64 = AtomicU64::new(10);
static IO_SECS: AtomicU64 = AtomicU64::new(30);

/// Anything shorter than this is not a timeout, it is a way to make
/// slow servers unusable. IDLE also ticks on the io timeout, so it
/// can never be off.
const MIN_IO_SECS: u64 = 5;

/// Set both, in seconds; a connect timeout of 0 waits as long as the
/// OS does.
pub fn set_timeouts(connect_secs: u64, io_secs: u64) {
    CONNECT_SECS.store(connect_secs, Ordering::Relaxed);
    IO_SECS.store(io_secs.max(MIN_IO_SECS), Ordering::Relaxed);
}

/// How long a read or a write waits before giving up.
pub fn io_timeout() -> Duration {
    Duration::from_secs(IO_SECS.load(Ordering::Relaxed).max(MIN_IO_SECS))
}

fn connect_timeout() -> Option<Duration> {
    match CONNECT_SECS.load(Ordering::Relaxed) {
        0 => None,
        secs => Some(Duration::from_secs(secs)),
    }
}

/// Whether an io error is a timeout, so the message can say so.
fn timed_out(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

/// A way to cut a connection short from another thread: shutting the
/// socket down makes whatever is blocked on it fail at once, which is
/// how mutt's Ctrl+G gets its abort.
#[derive(Clone, Default)]
pub struct Cutoff(Arc<CutoffState>);

#[derive(Default)]
struct CutoffState {
    socket: Mutex<Option<TcpStream>>,
    /// Set by `cut`, so the failure it causes is told apart from a
    /// connection that died on its own and should be retried.
    on_purpose: AtomicBool,
}

impl Cutoff {
    fn hold(&self, tcp: &TcpStream) {
        self.0.on_purpose.store(false, Ordering::Relaxed);
        if let (Ok(mut slot), Ok(clone)) = (self.0.socket.lock(), tcp.try_clone()) {
            *slot = Some(clone);
        }
    }

    /// Cut it. Whatever the connection was doing fails at once; the
    /// next job reconnects.
    pub fn cut(&self) {
        self.0.on_purpose.store(true, Ordering::Relaxed);
        if let Ok(slot) = self.0.socket.lock()
            && let Some(tcp) = slot.as_ref()
        {
            let _ = tcp.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Whether the last failure was this cut, and not the network
    /// letting go. Reading it clears it: one cut, one abort.
    pub fn was_cut(&self) -> bool {
        self.0.on_purpose.swap(false, Ordering::Relaxed)
    }
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

pub(crate) fn connect(host: &str, port: u16, tls: bool, cutoff: &Cutoff) -> Result<Stream> {
    let tcp = connect_tcp(host, port)?;
    tcp.set_read_timeout(Some(io_timeout()))?;
    tcp.set_write_timeout(Some(io_timeout()))?;
    cutoff.hold(&tcp);
    if tls {
        wrap_tls(tcp, host)
    } else {
        Ok(Stream::Plain(tcp))
    }
}

/// Connect, with a timeout of our own rather than the OS's.
///
/// `TcpStream::connect` waits out the kernel's SYN retries, which is
/// about two minutes with nothing on screen; a name that resolves to
/// several addresses is tried in turn, each getting the full wait.
fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let Some(timeout) = connect_timeout() else {
        return TcpStream::connect((host, port))
            .with_context(|| format!("connecting to {host}:{port}"));
    };
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .collect();
    ensure!(!addrs.is_empty(), "{host} resolves to nothing");
    let mut last = None;
    for addr in &addrs {
        match TcpStream::connect_timeout(addr, timeout) {
            Ok(tcp) => return Ok(tcp),
            Err(err) => last = Some(err),
        }
    }
    let err = last.expect("at least one address was tried");
    let secs = timeout.as_secs();
    let said = match timed_out(&err) {
        true => format!("connecting to {host}:{port} timed out after {secs}s"),
        false => format!("connecting to {host}:{port}"),
    };
    Err(anyhow::Error::from(err).context(said))
}

/// Extra trust set beyond the built-in Mozilla roots: whether to add
/// the OS trust store (mutt's $ssl_usesystemcerts) and a PEM file of
/// extra roots (mutt's $certificate_file). Process-wide, because the
/// TLS handshake happens on connection threads that carry an account
/// but not a config, exactly like the timeouts.
static TRUST: Mutex<Trust> = Mutex::new(Trust {
    system: true,
    extra_pem: None,
});

struct Trust {
    system: bool,
    extra_pem: Option<PathBuf>,
}

/// Install the trust settings from the config, once, before any
/// connection. Only ever adds anchors to the Mozilla baseline; it
/// cannot take the default roots away.
pub fn set_trust(system: bool, certificate_file: Option<PathBuf>) {
    let mut trust = TRUST.lock().unwrap();
    trust.system = system;
    trust.extra_pem = certificate_file;
}

/// The root store: the Mozilla roots always, then (opt-in) the OS
/// trust store and a PEM file of extra roots. A cert that will not
/// parse is skipped, not fatal: one bad line in a bundle should not
/// drop every good root with it.
fn root_store() -> Result<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let trust = TRUST.lock().unwrap();
    if trust.system {
        // native-certs reads the OS store; per-cert parse errors are
        // returned in `errors`, which we ignore, keeping the rest.
        let loaded = rustls_native_certs::load_native_certs();
        for cert in loaded.certs {
            let _ = roots.add(cert);
        }
    }
    if let Some(path) = &trust.extra_pem {
        let pem = std::fs::read(path)
            .with_context(|| format!("reading certificate_file {}", path.display()))?;
        let (added, _) = roots.add_parsable_certificates(parse_pem_certs(&pem)?);
        if added == 0 {
            anyhow::bail!("no certificates in {}", path.display());
        }
    }
    Ok(roots)
}

/// The DER certificates in a PEM blob (a `certificate_file`).
fn parse_pem_certs(pem: &[u8]) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let mut cursor = std::io::Cursor::new(pem);
    rustls_pemfile::certs(&mut cursor)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parsing certificate_file PEM")
}

pub(crate) fn wrap_tls(tcp: TcpStream, host: &str) -> Result<Stream> {
    let roots = root_store()?;
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
    /// Who is on the other end, for anything that goes wrong.
    peer: String,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl Conn {
    pub(crate) fn new(stream: Stream, peer: impl Into<String>) -> Conn {
        Conn {
            stream,
            peer: peer.into(),
            buf: vec![0; 8192],
            start: 0,
            end: 0,
        }
    }

    /// "imap.example.com:993 timed out after 30s while reading", or
    /// the plain error with the server named.
    fn io_error(&self, err: std::io::Error, doing: &str) -> anyhow::Error {
        let peer = &self.peer;
        // The io error stays in the chain: `is_timeout` and
        // `is_connection_error` read it, and IDLE ticks on it.
        let said = match timed_out(&err) {
            true => format!(
                "{peer} timed out after {}s while {doing}",
                io_timeout().as_secs()
            ),
            false => format!("{doing} {peer}"),
        };
        anyhow::Error::from(err).context(said)
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
                Ok(n) => break n,
                Err(err) => return Err(self.io_error(err, "reading from")),
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
        self.stream
            .write_all(bytes)
            .map_err(|err| self.io_error(err, "writing to"))?;
        self.stream
            .flush()
            .map_err(|err| self.io_error(err, "writing to"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_are_never_off_and_never_too_short() {
        // A read timeout is IDLE's heartbeat as well as its patience,
        // so it is clamped rather than honoured as given.
        set_timeouts(10, 1);
        assert_eq!(io_timeout().as_secs(), MIN_IO_SECS);
        assert_eq!(connect_timeout(), Some(Duration::from_secs(10)));
        // A connect timeout of zero is mutt's "wait for the OS".
        set_timeouts(0, 45);
        assert_eq!(connect_timeout(), None);
        assert_eq!(io_timeout().as_secs(), 45);
        set_timeouts(10, 30);
    }

    #[test]
    fn the_baseline_roots_are_always_there() {
        // Even with the OS store off and no extra file, the Mozilla
        // roots make a non-empty store; the trust settings only add.
        set_trust(false, None);
        let store = root_store().unwrap();
        assert!(store.len() > 50, "webpki roots present: {}", store.len());
    }

    #[test]
    fn a_certificate_file_that_is_not_pem_is_an_error() {
        let dir = std::env::temp_dir().join(format!("rmut-net-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("garbage.pem");
        std::fs::write(&path, b"not a certificate\n").unwrap();
        set_trust(false, Some(path.clone()));
        let err = root_store().unwrap_err().to_string();
        assert!(err.contains("no certificates"), "{err}");
        // Put the trust back so other tests in the process are clean.
        set_trust(true, None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
