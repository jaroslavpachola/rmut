//! One index row: the `$index_format` text and the style its marks
//! and the `[[color_index]]` rules give it. Both front ends draw the
//! mailbox from here; only the selection marking is their own.

use std::collections::HashMap;

use rmut_core::format::{self, IndexFields};
use rmut_core::pattern::{self, Pattern};
use rmut_core::{message, pattern::Position};
use rmut_session::Session;

use crate::pager::humanize_size;
use crate::style::Style;
use crate::theme::Theme;

/// How often each Message-ID occurs, for the `~=` (duplicate)
/// pattern in a color rule. Counted once per draw.
pub fn id_counts(session: &Session) -> HashMap<&str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for m in &session.msgs {
        if let Some(id) = m.env.msg_id.as_deref() {
            *counts.entry(id).or_default() += 1;
        }
    }
    counts
}

/// The text and style of the row showing `visible[vi]` (message
/// `mi`), unpadded and unselected: the front end lays the cursor
/// over it.
pub fn row(
    session: &Session,
    theme: &Theme,
    rules: &[(Vec<Pattern>, Style)],
    id_counts: &HashMap<&str, usize>,
    vi: usize,
    mi: usize,
) -> (String, Style) {
    let msg = &session.msgs[mi];
    let env = &msg.env;
    let status = env.file.flags.status_char(env.file.is_new);
    let flagged = if env.file.flags.flagged { '!' } else { ' ' };
    // Third %Z slot, mutt-style: tag mark, or mutt's $to_chars
    // (" +TCFL") for how the mail relates to me. Precedence is
    // mutt's: sent by me, then To (alone, or among others), then
    // Cc, then a subscribed list.
    let me = session.me();
    let mark = if env.tagged {
        '*'
    } else if me.wrote(&env.from_full) {
        'F'
    } else if me.any(&env.to) {
        // mutt's '+' means sole recipient: one To and no Cc.
        if env.to.len() == 1 && env.cc.is_empty() {
            '+'
        } else {
            'T'
        }
    } else if me.any(&env.cc) {
        'C'
    } else if env
        .to
        .iter()
        .chain(&env.cc)
        .any(|a| session.subscribed.iter().any(|m| m.is_match(a)))
    {
        'L'
    } else {
        ' '
    };
    let (depth, hidden) = session.thread_info(mi);
    let fmt = session
        .config
        .index
        .format
        .as_deref()
        .unwrap_or(format::DEFAULT_FORMAT);
    // mutt's $hide_thread_subject: a reply repeating its parent's
    // subject shows only the tree arrow.
    let mut subject = if session.subject_hidden(mi) {
        String::new()
    } else {
        env.subject.clone()
    };
    // The hidden count rides on the subject unless the format
    // places it itself with %M.
    if let Some(n) = hidden
        && !fmt.contains("%M")
        && !fmt.contains("?M?")
    {
        subject += &format!(" ({n} hidden)");
    }
    if depth > 0 {
        // mutt stars the arrow of a message the subject fallback
        // placed, so a thread says which of it the senders meant.
        let arrow = if session.subject_threaded(mi) {
            "└*"
        } else {
            "└>"
        };
        subject = format!("{}{arrow}{subject}", "  ".repeat(depth - 1));
    }
    let text = format::render(
        fmt,
        &IndexFields {
            number: vi + 1,
            status,
            flag: flagged,
            mark,
            date: &message::format_index_date_with(
                env.date,
                session.config.index.date_format.as_deref(),
            ),
            from: &env.from,
            size: &humanize_size(env.file.size),
            lines: env.lines,
            list: env.list.as_deref(),
            label: env.label.as_deref(),
            hidden,
            subject: &subject,
        },
    );
    let mut style = Style::new();
    if status == 'N' || status == 'O' {
        style = style.bold();
    }
    if status == 'D' {
        style = style.fg(theme.deleted);
    } else if env.tagged {
        style = style.fg(theme.tagged);
    } else if env.file.flags.flagged {
        style = style.fg(theme.flagged);
    }
    // The first matching [[color_index]] rule wins over the
    // built-in slot colors.
    let pos = Position {
        number: vi + 1,
        current: session.sel + 1,
        last: session.visible.len(),
        duplicate: env
            .msg_id
            .as_deref()
            .is_some_and(|id| id_counts.get(id).copied().unwrap_or(0) > 1),
    };
    if let Some((_, rule)) = rules
        .iter()
        .find(|(patterns, _)| pattern::matches_in(patterns, env, session.scope(pos), None))
    {
        style = style.patch(*rule);
    }
    (text, style)
}
