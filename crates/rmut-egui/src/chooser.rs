//! "Attach files…" through the desktop's own file chooser: the XDG
//! FileChooser portal (org.freedesktop.portal.FileChooser.OpenFile),
//! which GNOME, KDE and the wlroots desktops all answer with their
//! native dialog. Spoken over D-Bus directly with zbus, which the
//! window already carries for accessibility, rather than through a
//! dialog crate and the async runtime it would bring. The call blocks
//! until the dialog closes, so the window runs it on a thread.

use std::path::PathBuf;

/// What the dialog came back with.
#[derive(Debug, PartialEq, Eq)]
pub enum Chosen {
    Files(Vec<PathBuf>),
    /// Closed without a choice.
    Cancelled,
}

/// Open the chooser, several files allowed, and wait for it.
pub fn pick_files(title: &str) -> Result<Chosen, String> {
    #[cfg(target_os = "linux")]
    {
        portal::open_files(title).map_err(|err| format!("no file chooser: {err}"))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = title;
        Err("no file chooser here; Tab or ? at the attach prompt browses".into())
    }
}

/// A `file://` URI as a path, percent escapes decoded byte by byte (a
/// name need not be UTF-8). Anything else is None: the portal hands
/// back local files only for an OpenFile.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // file://host/path: only the local host means a local file.
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) if &rest[..i] == "localhost" => &rest[i..],
        _ => return None,
    };
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&h), Some(&l)) if hex(h).is_some() && hex(l).is_some() => {
                out.push((hex(h)? * 16 + hex(l)?) as u8);
                i += 3;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    use std::os::unix::ffi::OsStringExt as _;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(target_os = "linux")]
mod portal {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use zbus::blocking::{Connection, Proxy, proxy::Builder};
    use zbus::proxy::CacheProperties;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

    use super::{Chosen, uri_path};

    const DESKTOP: &str = "org.freedesktop.portal.Desktop";
    const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

    fn request(conn: &Connection, path: &str) -> zbus::Result<Proxy<'static>> {
        Builder::new(conn)
            .destination(DESKTOP)?
            .path(path.to_string())?
            .interface("org.freedesktop.portal.Request")?
            .cache_properties(CacheProperties::No)
            .build()
    }

    /// The portal's request/response dance: the answer comes as a
    /// Response signal on a Request object whose path the caller can
    /// predict from its handle_token, so the signal is subscribed to
    /// before the call is made and cannot be missed.
    pub fn open_files(title: &str) -> zbus::Result<Chosen> {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let conn = Connection::session()?;
        let token = format!(
            "rmut_{}_{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        let expected = format!("{DESKTOP_PATH}/request/{sender}/{token}");
        let mut responses = request(&conn, &expected)?.receive_signal("Response")?;
        let chooser = Builder::<Proxy>::new(&conn)
            .destination(DESKTOP)?
            .path(DESKTOP_PATH)?
            .interface("org.freedesktop.portal.FileChooser")?
            .cache_properties(CacheProperties::No)
            .build()?;
        let mut options: HashMap<&str, Value> = HashMap::new();
        options.insert("handle_token", Value::from(token.as_str()));
        options.insert("multiple", Value::from(true));
        options.insert("modal", Value::from(true));
        // No parent window handle: the window has no exported
        // xdg-foreign handle to give, so the dialog stands alone.
        let handle: OwnedObjectPath = chooser.call("OpenFile", &("", title, options))?;
        // A portal older than handle_token answers on a path of its
        // own choosing; listen there instead.
        if handle.as_str() != expected {
            responses = request(&conn, handle.as_str())?.receive_signal("Response")?;
        }
        let Some(message) = responses.next() else {
            return Err(zbus::Error::Failure("the portal never answered".into()));
        };
        let (code, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
        // 0 is success, 1 the user cancelled, 2 anything else.
        match code {
            0 => {}
            1 => return Ok(Chosen::Cancelled),
            _ => return Err(zbus::Error::Failure("the file chooser failed".into())),
        }
        let uris = match results.get("uris").map(|v| &**v) {
            Some(Value::Array(uris)) => uris
                .iter()
                .filter_map(|u| match u {
                    Value::Str(s) => uri_path(s.as_str()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(Chosen::Files(uris))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_decode_to_paths() {
        assert_eq!(
            uri_path("file:///home/jane/My%20Report.pdf"),
            Some(PathBuf::from("/home/jane/My Report.pdf"))
        );
        assert_eq!(
            uri_path("file://localhost/tmp/a%2Fb"),
            Some(PathBuf::from("/tmp/a/b"))
        );
        // Not UTF-8 is still a name.
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(
            uri_path("file:///tmp/%FF").unwrap().as_os_str().as_bytes(),
            b"/tmp/\xff"
        );
        // A stray % stays itself.
        assert_eq!(
            uri_path("file:///tmp/100%"),
            Some(PathBuf::from("/tmp/100%"))
        );
        assert_eq!(uri_path("https://example.com/x"), None);
        assert_eq!(uri_path("file://elsewhere/x"), None);
    }

    /// The whole request/response dance against a stand-in portal on
    /// a private bus of the test's own (skipped where dbus-daemon is
    /// missing): the call carries the options, the Response signal on
    /// the predicted request path brings the files back, and a
    /// cancel comes back as one.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_answer_comes_back_as_files() {
        use std::collections::HashMap;
        use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

        let Ok(mut daemon) = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("no dbus-daemon; skipped");
            return;
        };
        let mut address = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(daemon.stdout.take().unwrap()),
            &mut address,
        )
        .unwrap();
        // This test is the only one in the crate that talks to a bus.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim()) };

        struct Fake {
            answer: u32,
        }
        #[zbus::interface(name = "org.freedesktop.portal.FileChooser")]
        impl Fake {
            fn open_file(
                &self,
                #[zbus(connection)] conn: &zbus::Connection,
                #[zbus(header)] header: zbus::message::Header<'_>,
                _parent: &str,
                title: &str,
                options: HashMap<String, OwnedValue>,
            ) -> OwnedObjectPath {
                assert_eq!(title, "Attach files");
                assert_eq!(*options["multiple"], Value::from(true));
                let Value::Str(token) = &*options["handle_token"] else {
                    panic!("a handle_token");
                };
                let sender = header.sender().unwrap().to_string();
                let path = format!(
                    "/org/freedesktop/portal/desktop/request/{}/{token}",
                    sender.trim_start_matches(':').replace('.', "_")
                );
                let conn = zbus::blocking::Connection::from(conn.clone());
                let (answer, at) = (self.answer, path.clone());
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let mut results: HashMap<&str, Value> = HashMap::new();
                    results.insert(
                        "uris",
                        Value::from(vec![
                            "file:///tmp/My%20Report.pdf".to_string(),
                            "file:///tmp/notes.txt".to_string(),
                        ]),
                    );
                    conn.emit_signal(
                        Some(sender.as_str()),
                        at.as_str(),
                        "org.freedesktop.portal.Request",
                        "Response",
                        &(answer, results),
                    )
                    .unwrap();
                });
                OwnedObjectPath::try_from(path).unwrap()
            }
        }

        for (answer, want) in [
            (
                0,
                Chosen::Files(vec![
                    PathBuf::from("/tmp/My Report.pdf"),
                    PathBuf::from("/tmp/notes.txt"),
                ]),
            ),
            (1, Chosen::Cancelled),
        ] {
            let _portal = zbus::blocking::connection::Builder::session()
                .unwrap()
                .name("org.freedesktop.portal.Desktop")
                .unwrap()
                .serve_at("/org/freedesktop/portal/desktop", Fake { answer })
                .unwrap()
                .build()
                .unwrap();
            assert_eq!(pick_files("Attach files"), Ok(want));
        }
        let _ = daemon.kill();
        let _ = daemon.wait();
    }
}
