//! Point rmut's parser at a real mailbox and report what it makes of
//! it. The test suite runs on mail written to exercise a particular
//! branch; this runs on whatever actually arrived over the years, and
//! answers a different question: does anything in a real corpus make
//! rmut panic, fail to parse, or render a line it should not?
//!
//! Every message goes through `message::envelope`, then the whole
//! folder through `thread::thread`, then every envelope through
//! `format::render` with the index format from the config. Each step
//! runs inside `catch_unwind`, so a message that panics is counted and
//! named rather than ending the run.
//!
//! ```sh
//! cargo run --release -p rmut-core --example corpus            # ~/.cache/rmut
//! cargo run --release -p rmut-core --example corpus -- ~/Mail/inbox
//! ```
//!
//! Read-only throughout: it opens files and never writes one.

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmut_core::format::{self, IndexFields};
use rmut_core::maildir;
use rmut_core::message::{self, Envelope};
use rmut_core::thread;

/// mutt's own default, so a run without a config still renders
/// something with every awkward specifier in it.
const DEFAULT_INDEX_FORMAT: &str = "%4C %Z %{%b %d} %-15.15L (%?l?%4l&%4c?) %s";

/// How many offending paths to print per category; the rest are
/// counted only, since a corpus can disagree thousands of times over
/// the same one cause.
const EXAMPLES: usize = 5;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs = if args.is_empty() {
        default_dirs()
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    if dirs.is_empty() {
        eprintln!("no maildirs found; pass one as an argument");
        std::process::exit(2);
    }
    let fmt = index_format();
    println!("index format: {fmt}\n");

    // A panic here is data, not a crash: swallow the default hook's
    // output and keep the payload for the report.
    let panics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&panics);
    panic::set_hook(Box::new(move |info| {
        sink.lock().unwrap().push(info.to_string());
    }));

    let mut total = Report::default();
    for dir in &dirs {
        let mut report = Report::default();
        sweep(dir, &fmt, &panics, &mut report);
        report.print(&dir.display().to_string());
        total.absorb(report);
    }
    let _ = panic::take_hook();
    if dirs.len() > 1 {
        total.print("all folders");
    }
    // Only a panic or an outright parse failure is a verdict; the
    // rest of the report is for eyeballing.
    let fatal = total.panicked.len() + total.unparsed.len() + total.lost.len();
    println!(
        "\n{}",
        if fatal == 0 {
            format!("{} messages, no panics and no parse failures", total.seen)
        } else {
            format!("{fatal} of {} messages need looking at", total.seen)
        }
    );
    std::process::exit(if fatal == 0 { 0 } else { 1 });
}

/// Every maildir under the IMAP cache, which is the largest corpus on
/// a machine that has been running rmut: one directory per account
/// per folder.
fn default_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let cache = Path::new(&home).join(".cache/rmut/imap");
    let mut out = Vec::new();
    let Ok(accounts) = cache.read_dir() else {
        return out;
    };
    for account in accounts.flatten() {
        let Ok(folders) = account.path().read_dir() else {
            continue;
        };
        for folder in folders.flatten() {
            let path = folder.path();
            if path.join("cur").is_dir() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The `[index] format` line out of the config, read as text rather
/// than through `Config` so that a config this build cannot parse
/// still leaves the sweep something to render with.
fn index_format() -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return DEFAULT_INDEX_FORMAT.into();
    };
    let path = Path::new(&home).join(".config/rmut/config.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return DEFAULT_INDEX_FORMAT.into();
    };
    let mut in_index = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_index = line == "[index]";
        } else if in_index
            && let Some(value) = line.strip_prefix("format")
            && let Some(value) = value.trim_start().strip_prefix('=')
        {
            return value.trim().trim_matches('"').to_string();
        }
    }
    DEFAULT_INDEX_FORMAT.into()
}

#[derive(Default)]
struct Report {
    seen: usize,
    /// Verdicts: a message rmut cannot handle at all.
    panicked: Vec<String>,
    unparsed: Vec<String>,
    /// A message the threader dropped or duplicated.
    lost: Vec<String>,
    /// Signals: legal, but worth an eye.
    undated: Vec<String>,
    undecoded: Vec<String>,
    replacement: Vec<String>,
    no_msg_id: Vec<String>,
    control_char: Vec<String>,
}

impl Report {
    fn absorb(&mut self, other: Report) {
        self.seen += other.seen;
        for (mine, theirs) in [
            (&mut self.panicked, other.panicked),
            (&mut self.unparsed, other.unparsed),
            (&mut self.lost, other.lost),
            (&mut self.undated, other.undated),
            (&mut self.undecoded, other.undecoded),
            (&mut self.replacement, other.replacement),
            (&mut self.no_msg_id, other.no_msg_id),
            (&mut self.control_char, other.control_char),
        ] {
            mine.extend(theirs);
        }
    }

    fn print(&self, title: &str) {
        println!("{title}: {} messages", self.seen);
        let verdicts = [
            ("PANIC", &self.panicked),
            ("parse failed", &self.unparsed),
            ("lost in threading", &self.lost),
        ];
        let signals = [
            ("no usable Date", &self.undated),
            ("undecoded =?...?=", &self.undecoded),
            ("U+FFFD after decoding", &self.replacement),
            ("no Message-ID", &self.no_msg_id),
            ("control char in index line", &self.control_char),
        ];
        for (label, items) in verdicts.into_iter().chain(signals) {
            if items.is_empty() {
                continue;
            }
            let pct = 100.0 * items.len() as f64 / self.seen.max(1) as f64;
            println!("  {label}: {} ({pct:.2}%)", items.len());
            for item in items.iter().take(EXAMPLES) {
                println!("      {item}");
            }
            if items.len() > EXAMPLES {
                println!("      ... and {} more", items.len() - EXAMPLES);
            }
        }
        println!();
    }
}

fn sweep(dir: &Path, fmt: &str, panics: &Arc<Mutex<Vec<String>>>, report: &mut Report) {
    let files = match maildir::scan(dir) {
        Ok(files) => files,
        Err(err) => {
            eprintln!("{}: {err:#}", dir.display());
            return;
        }
    };
    report.seen = files.len();

    let mut envs: Vec<Envelope> = Vec::with_capacity(files.len());
    for file in files {
        let path = file.path.display().to_string();
        match panic::catch_unwind(AssertUnwindSafe(|| message::envelope(file))) {
            Ok(Ok(env)) => envs.push(env),
            Ok(Err(err)) => report.unparsed.push(format!("{path}: {err:#}")),
            Err(_) => report.panicked.push(format!("{path}: {}", took(panics))),
        }
    }

    for env in &envs {
        let path = env.file.path.display().to_string();
        if env.date == 0 {
            report.undated.push(path.clone());
        }
        if env.msg_id.is_none() {
            report.no_msg_id.push(path.clone());
        }
        // An encoded-word that survived decoding is a word rmut did
        // not recognise; the reader sees the raw =?utf-8?B?...?=.
        if env.subject.contains("=?") || env.from_full.contains("=?") {
            report
                .undecoded
                .push(format!("{path}: {}", squash(&env.subject)));
        }
        if env.subject.contains('\u{fffd}') || env.from.contains('\u{fffd}') {
            report
                .replacement
                .push(format!("{path}: {}", squash(&env.subject)));
        }
    }

    check_threading(&envs, report, panics);
    check_rendering(&envs, fmt, report, panics);
}

/// Threading has to be total: every message it is given comes back
/// exactly once, at some depth. A message that falls out of the tree
/// is one the index would never show.
fn check_threading(envs: &[Envelope], report: &mut Report, panics: &Arc<Mutex<Vec<String>>>) {
    let refs: Vec<&Envelope> = envs.iter().collect();
    let items = match panic::catch_unwind(AssertUnwindSafe(|| thread::thread(&refs))) {
        Ok(items) => items,
        Err(_) => {
            report
                .panicked
                .push(format!("threading the folder: {}", took(panics)));
            return;
        }
    };
    let mut times = vec![0usize; envs.len()];
    for item in &items {
        // An index the threader invented would be a bug of its own,
        // and would panic the slice below.
        match times.get_mut(item.index) {
            Some(slot) => *slot += 1,
            None => report
                .lost
                .push(format!("threading returned index {}", item.index)),
        }
    }
    for (env, n) in envs.iter().zip(times) {
        match n {
            1 => {}
            0 => report
                .lost
                .push(format!("{}: dropped by threading", env.file.path.display())),
            n => report
                .lost
                .push(format!("{}: threaded {n} times", env.file.path.display())),
        }
    }
}

/// The index line itself: what the terminal would be asked to write.
/// A control character here is the bug `one_line` exists to stop, so
/// one reaching this far means a field bypassed it.
fn check_rendering(
    envs: &[Envelope],
    fmt: &str,
    report: &mut Report,
    panics: &Arc<Mutex<Vec<String>>>,
) {
    for (n, env) in envs.iter().enumerate() {
        let date = message::format_index_date(env.date);
        let size = env.file.size.to_string();
        let fields = IndexFields {
            number: n + 1,
            status: env.file.flags.status_char(env.file.is_new),
            flag: ' ',
            mark: ' ',
            date: &date,
            from: &env.from,
            size: &size,
            lines: env.lines,
            list: env.list.as_deref(),
            hidden: None,
            label: env.label.as_deref(),
            subject: &env.subject,
        };
        let line = match panic::catch_unwind(AssertUnwindSafe(|| format::render(fmt, &fields))) {
            Ok(line) => line,
            Err(_) => {
                report.panicked.push(format!(
                    "{}: rendering the index line: {}",
                    env.file.path.display(),
                    took(panics)
                ));
                continue;
            }
        };
        if line.chars().any(|c| c.is_control()) {
            report
                .control_char
                .push(format!("{}: {}", env.file.path.display(), squash(&line)));
        }
    }
}

/// The message from the most recent panic, so the report names a
/// cause rather than only a path.
fn took(panics: &Arc<Mutex<Vec<String>>>) -> String {
    panics
        .lock()
        .unwrap()
        .pop()
        .unwrap_or_else(|| "no panic message".into())
}

/// One short readable line, for a report that has to stay scannable.
fn squash(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { '.' } else { c })
        .collect();
    if flat.chars().count() > 70 {
        format!("{}...", flat.chars().take(70).collect::<String>())
    } else {
        flat
    }
}
