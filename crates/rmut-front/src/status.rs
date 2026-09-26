//! The status bar and the terminal title: mutt's $status_format,
//! $ts_status_format and $pager_format, expanded from the session.
//! Both front ends show the same line.

use rmut_core::config::Config;
use rmut_core::format;
use rmut_core::message::MessageView;
use rmut_session::Session;

use crate::pager::{PagerStyle, RowCache};

/// mutt's $wrap: the pager's text width at a screen width. Positive
/// caps it, negative leaves that margin (never under 20 columns).
pub fn pager_wrap(config: &Config, width: usize) -> usize {
    match config.pager.wrap {
        Some(n) if n > 0 => (n as usize).min(width),
        Some(n) if n < 0 => width.saturating_sub(n.unsigned_abs() as usize).max(20),
        _ => width,
    }
}

/// What the pager's status line needs to know about the open message.
pub struct PagerView<'a> {
    pub view: &'a MessageView,
    /// The front end's rows for this view: the status line counts them.
    pub rows: &'a RowCache,
    pub scroll: usize,
    pub full_headers: bool,
    pub hide_quoted: bool,
}

pub fn index_status(session: &Session, index_offset: usize, width: usize, rows: usize) -> String {
    let fmt = session
        .config
        .ui
        .status_format
        .as_deref()
        .unwrap_or(format::DEFAULT_STATUS_FORMAT);
    format::render_status(fmt, width, &|spec| {
        index_status_field(session, index_offset, spec, rows)
    })
}

/// mutt's $ts_status_format: the terminal title, from the same fields
/// as the status line (a wide width, so %>… padding does not clip).
pub fn index_title(session: &Session, index_offset: usize, rows: usize) -> String {
    let fmt = session
        .config
        .ui
        .title_format
        .as_deref()
        .unwrap_or("rmut: %f");
    format::render_status(fmt, 200, &|spec| {
        index_status_field(session, index_offset, spec, rows)
    })
    .trim_end()
    .to_string()
}

/// One status specifier's value, shared by the bottom bar and the
/// terminal title.
fn index_status_field(session: &Session, index_offset: usize, spec: char, rows: usize) -> String {
    match spec {
        'f' => session.title.clone(),
        'm' => session.msgs.len().to_string(),
        // Shown message count, only when a limit narrows the view.
        'M' => {
            if session.visible.len() != session.msgs.len() {
                session.visible.len().to_string()
            } else {
                String::new()
            }
        }
        'n' => session.new_count().to_string(),
        'u' => session
            .msgs
            .iter()
            .filter(|m| !m.env.file.flags.seen)
            .count()
            .to_string(),
        'd' => session.deleted_count().to_string(),
        'F' => session
            .msgs
            .iter()
            .filter(|m| m.env.file.flags.flagged)
            .count()
            .to_string(),
        't' => session
            .msgs
            .iter()
            .filter(|m| m.env.tagged)
            .count()
            .to_string(),
        's' => format!(
            "{}{}",
            session.sort.name(),
            if session.sort_rev { "-rev" } else { "" }
        ),
        'V' => session
            .limit
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default(),
        'r' => {
            // mutt's $status_chars: [0] unchanged, [1] changed, [2]
            // read-only. Unset keeps rmut's own marks.
            let chars: Option<Vec<char>> = session
                .config
                .ui
                .status_chars
                .as_deref()
                .map(|s| s.chars().collect());
            let pick = |i: usize, default: &str| -> String {
                chars
                    .as_ref()
                    .and_then(|c| c.get(i))
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| default.to_string())
            };
            if session.read_only {
                pick(2, "%")
            } else if session.pending_count() > 0 {
                pick(1, "*")
            } else {
                pick(0, "")
            }
        }
        'v' => env!("CARGO_PKG_VERSION").to_string(),
        // Index scroll position, like mutt's %P.
        'P' => {
            let len = session.visible.len();
            if len <= rows {
                "all".into()
            } else if index_offset == 0 {
                "top".into()
            } else if index_offset + rows >= len {
                "bot".into()
            } else {
                format!("{}%", (index_offset + rows) * 100 / len)
            }
        }
        '%' => "%".to_string(),
        other => format!("%{other}"),
    }
}

/// The classic pager bottom line; override with `[pager] format`.
pub const DEFAULT_PAGER_FORMAT: &str = "---Message %C/%m: %s -- %P";

/// mutt's $pager_format: %C message number, %m count, %n sender,
/// %s subject, %Z status chars, %P percent through the message,
/// %f mailbox, plus the conditional and %> machinery.
pub fn pager_status(
    session: &Session,
    pager: &PagerView,
    content_height: usize,
    width: usize,
) -> String {
    let total = pager
        .rows
        .rows(
            pager.view,
            pager_wrap(&session.config, width),
            pager.full_headers,
            &PagerStyle::of(&session.config, &session.quote_re),
            pager.hide_quoted,
        )
        .len()
        .max(1);
    let shown = (pager.scroll + content_height).min(total);
    let subject = pager
        .view
        .brief
        .iter()
        .find(|(n, _)| n == "Subject" || n == "Content-Type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let msg = session.visible.get(session.sel).map(|&i| &session.msgs[i]);
    let fmt = session
        .config
        .pager
        .format
        .as_deref()
        .unwrap_or(DEFAULT_PAGER_FORMAT);
    format::render_status(fmt, width, &|spec| match spec {
        'C' => (session.sel + 1).to_string(),
        'm' => session.visible.len().to_string(),
        's' => subject.to_string(),
        'n' => msg.map(|m| m.env.from.clone()).unwrap_or_default(),
        'Z' => msg
            .map(|m| {
                let f = &m.env.file;
                format!(
                    "{}{} ",
                    f.flags.status_char(f.is_new),
                    if f.flags.flagged { '!' } else { ' ' }
                )
            })
            .unwrap_or_default(),
        // mutt's pager: "all" when the message fits, "end" once its
        // last line is on screen, a percentage until then.
        'P' => match (pager.scroll, shown >= total) {
            (0, true) => "all".into(),
            (_, true) => "end".into(),
            _ => format!("{}%", shown * 100 / total),
        },
        'f' => session.title.clone(),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    })
}
