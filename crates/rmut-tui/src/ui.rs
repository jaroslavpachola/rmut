use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rmut_core::format::{self, IndexFields};
use rmut_core::message::{self, MessageView, Part};

use crate::app::{App, Mode, Pager, Prompt};

const INDEX_HELP: &str = "?:Help q:Quit Enter:View m:New r:Reply g:Grp f:Fwd d:Del u:Undel F:Flag t:Tag s:Save o:Sort l:Limit /:Find c:Mbox y:Fldrs v:Parts p:Print $:Sync";
const PAGER_HELP: &str = "?:Help q:Back j/k:Scroll Space/-:Page J/K:Msg r:Reply f:Fwd d:Del s:Save h:Hdrs v:Parts p:Print";
const ATTACH_HELP: &str = "q:Back j/k:Move Enter:View s:Save";
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open";
const HELP_HELP: &str = "q:Back j/k:Scroll Space/-:Page";

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [help_area, content_area, status_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let help = match app.mode {
        Mode::Index => INDEX_HELP,
        Mode::Pager(_) => PAGER_HELP,
        Mode::Attach { .. } => ATTACH_HELP,
        Mode::Folders { .. } => FOLDERS_HELP,
        Mode::Help { .. } => HELP_HELP,
    };
    frame.render_widget(Line::from(help).style(app.theme.bar_style()), help_area);

    if matches!(app.mode, Mode::Pager(_)) {
        // Optionally keep a slice of the index visible above the pager
        // (mutt's pager_index_lines).
        let index_lines = app
            .config
            .pager
            .index_lines
            .min(content_area.height.saturating_sub(1));
        let pager_area = if index_lines > 0 {
            let [index_area, pager_area] =
                Layout::vertical([Constraint::Length(index_lines), Constraint::Min(1)])
                    .areas(content_area);
            draw_index(frame, index_area, app);
            pager_area
        } else {
            content_area
        };
        if let Mode::Pager(pager) = &app.mode {
            draw_pager(frame, pager_area, pager, app.theme.header);
        }
    } else {
        match &app.mode {
            Mode::Index => draw_index(frame, content_area, app),
            Mode::Pager(_) => unreachable!(),
            Mode::Attach { parts, sel, .. } => draw_attach(frame, content_area, parts, *sel),
            Mode::Folders { dirs, sel } => draw_folders(frame, content_area, dirs, *sel),
            Mode::Help { lines, scroll } => draw_help(frame, content_area, lines, *scroll),
        }
    }
    draw_bottom_line(frame, status_area, app, content_area.height);
}

// ---- index ----

fn draw_index(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.visible.is_empty() {
        let text = if app.limit.is_some() {
            "No messages match the limit (l clears it)."
        } else {
            "No mail in mailbox."
        };
        frame.render_widget(Paragraph::new(text).dim(), area);
        return;
    }
    let rows = area.height as usize;
    // Keep the selection visible.
    if app.sel < app.index_offset {
        app.index_offset = app.sel;
    } else if app.sel >= app.index_offset + rows {
        app.index_offset = app.sel + 1 - rows;
    }
    let width = area.width as usize;
    let mut lines = Vec::with_capacity(rows);
    for (vi, &mi) in app
        .visible
        .iter()
        .enumerate()
        .skip(app.index_offset)
        .take(rows)
    {
        let msg = &app.msgs[mi];
        let env = &msg.env;
        let status = env.file.flags.status_char(env.file.is_new);
        let flagged = if env.file.flags.flagged { '!' } else { ' ' };
        // Third %Z slot, mutt-style: tag mark or how the mail
        // addresses me.
        let mark = if env.tagged {
            '*'
        } else if env.to.iter().any(|a| app.me.contains(a)) {
            if env.to.len() == 1 { '+' } else { 'T' }
        } else if env.cc.iter().any(|a| app.me.contains(a)) {
            'C'
        } else {
            ' '
        };
        let (depth, hidden) = app.thread_info(mi);
        let mut subject = env.subject.clone();
        if let Some(n) = hidden {
            subject += &format!(" ({n} hidden)");
        }
        if depth > 0 {
            subject = format!("{}└>{subject}", "  ".repeat(depth - 1));
        }
        let fmt = app
            .config
            .index
            .format
            .as_deref()
            .unwrap_or(format::DEFAULT_FORMAT);
        let text = format::render(
            fmt,
            &IndexFields {
                number: vi + 1,
                status,
                flag: flagged,
                mark,
                date: &message::format_index_date_with(
                    env.date,
                    app.config.index.date_format.as_deref(),
                ),
                from: &env.from,
                size: &humanize_size(env.file.size),
                lines: env.lines,
                list: env.list.as_deref(),
                subject: &subject,
            },
        );
        let text = format!("{text:<width$}");
        let mut style = Style::new();
        if status == 'N' || status == 'O' {
            style = style.add_modifier(Modifier::BOLD);
        }
        if status == 'D' {
            style = style.fg(app.theme.deleted);
        } else if env.tagged {
            style = style.fg(app.theme.tagged);
        } else if env.file.flags.flagged {
            style = style.fg(app.theme.flagged);
        }
        if vi == app.sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

pub fn humanize_size(bytes: u64) -> String {
    match bytes {
        0..=999 => format!("{bytes}"),
        1000..=10_239 => format!("{:.1}K", bytes as f64 / 1024.0),
        10_240..=1_048_575 => format!("{}K", bytes / 1024),
        1_048_576..=10_485_759 => format!("{:.1}M", bytes as f64 / 1_048_576.0),
        _ => format!("{}M", bytes / 1_048_576),
    }
}

// ---- pager ----

fn pager_lines(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    header_color: Color,
) -> Vec<Line<'static>> {
    let headers = if full_headers { &view.all } else { &view.brief };
    let mut lines: Vec<Line> = Vec::new();
    for (name, value) in headers {
        lines.push(Line::from(vec![
            Span::styled(format!("{name}: "), Style::new().fg(header_color).bold()),
            Span::raw(value.clone()),
        ]));
    }
    lines.push(Line::raw(""));
    for line in view.body.lines() {
        // Marker lines like the PGP verdict get the header treatment.
        let marker = line.starts_with("[-- ") && line.ends_with(" --]");
        for wrapped in wrap_line(line, width) {
            lines.push(if marker {
                Line::styled(wrapped, Style::new().fg(header_color).bold())
            } else {
                Line::raw(wrapped)
            });
        }
    }
    lines
}

/// Total pager lines at the given width: header block + separator + body.
pub fn pager_line_count(view: &MessageView, width: usize, full_headers: bool) -> usize {
    let headers = if full_headers {
        view.all.len()
    } else {
        view.brief.len()
    };
    headers
        + 1
        + view
            .body
            .lines()
            .map(|l| wrap_line(l, width).len())
            .sum::<usize>()
}

/// Word-wrap one body line to `width` columns (hard break when a single
/// word is longer than the line). Tabs are expanded first.
pub fn wrap_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(4);
    let expanded = line.replace('\t', "    ");
    let chars: Vec<char> = expanded.chars().collect();
    if chars.len() <= width {
        return vec![expanded];
    }
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        if chars.len() - start <= width {
            out.push(chars[start..].iter().collect());
            break;
        }
        let window_end = start + width;
        let brk = (start + 1..window_end)
            .rev()
            .find(|&i| chars[i] == ' ')
            .unwrap_or(window_end);
        out.push(chars[start..brk].iter().collect());
        start = if chars.get(brk) == Some(&' ') {
            brk + 1
        } else {
            brk
        };
    }
    out
}

fn draw_pager(frame: &mut Frame, area: Rect, pager: &Pager, header_color: Color) {
    let visible: Vec<Line> = pager_lines(
        &pager.view,
        area.width as usize,
        pager.full_headers,
        header_color,
    )
    .into_iter()
    .skip(pager.scroll)
    .take(area.height as usize)
    .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

// ---- help ----

fn draw_help(frame: &mut Frame, area: Rect, lines: &[String], scroll: usize) {
    let visible: Vec<Line> = lines
        .iter()
        .skip(scroll)
        .take(area.height as usize)
        .map(|l| Line::raw(l.as_str()))
        .collect();
    frame.render_widget(Paragraph::new(visible), area);
}

// ---- attachments ----

fn draw_attach(frame: &mut Frame, area: Rect, parts: &[Part], sel: usize) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    for (i, part) in parts.iter().enumerate().take(area.height as usize) {
        let text = format!(
            "{:>3} [{:<24}] {:>6}  {}",
            i + 1,
            part.mimetype,
            humanize_size(part.size as u64),
            part.filename.as_deref().unwrap_or("(inline)"),
        );
        let text = format!("{text:<width$}");
        let mut style = Style::new();
        if i == sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// ---- folders ----

fn draw_folders(frame: &mut Frame, area: Rect, dirs: &[(String, usize)], sel: usize) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    for (i, (dir, new)) in dirs.iter().enumerate().take(area.height as usize) {
        let mut text = format!("{:>3} {}", i + 1, dir);
        if *new > 0 {
            text += &format!(" ({new} new)");
        }
        let text = format!("{text:<width$}");
        let mut style = Style::new();
        if i == sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if *new > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// ---- bottom line: prompt or status ----

fn draw_bottom_line(frame: &mut Frame, area: Rect, app: &App, content_height: u16) {
    if let Some(prompt) = &app.prompt {
        let mut text = match prompt {
            Prompt::Line { label, buf, .. } => format!("{label}{buf}\u{2581}"),
            Prompt::Key { label, .. } => label.clone(),
        };
        // Completion hints and the like show behind the input.
        if let Some(msg) = &app.status {
            text += &format!("  [{msg}]");
        }
        frame.render_widget(Line::from(text), area);
        return;
    }
    let text = match &app.mode {
        Mode::Pager(pager) => {
            let height = content_height.saturating_sub(app.config.pager.index_lines);
            pager_status(app, pager, height, area.width as usize)
        }
        Mode::Attach { parts, .. } => format!("---rmut: attachments [Parts:{}]", parts.len()),
        Mode::Folders { dirs, .. } => format!("---rmut: mailboxes [Found:{}]", dirs.len()),
        Mode::Help { .. } => "---rmut: help".to_string(),
        Mode::Index => index_status(app),
    };
    let text = match &app.status {
        Some(msg) => format!("{text} -- {msg}"),
        None => text,
    };
    frame.render_widget(Line::from(text).style(app.theme.bar_style()), area);
}

fn index_status(app: &App) -> String {
    let mut text = format!("---rmut: {} [Msgs:{}", app.title, app.visible.len());
    if app.visible.len() != app.msgs.len() {
        text += &format!("/{}", app.msgs.len());
    }
    text += &format!(" New:{}", app.new_count());
    let deleted = app.deleted_count();
    if deleted > 0 {
        text += &format!(" Del:{deleted}");
    }
    text += &format!(
        "] (sort:{}{})",
        app.sort.name(),
        if app.sort_rev { "-rev" } else { "" }
    );
    if let Some((limit, _)) = &app.limit {
        text += &format!(" (limit:{limit})");
    }
    text
}

fn pager_status(app: &App, pager: &Pager, content_height: u16, width: usize) -> String {
    let total = pager_line_count(&pager.view, width, pager.full_headers).max(1);
    let shown = (pager.scroll + content_height as usize).min(total);
    let subject = pager
        .view
        .brief
        .iter()
        .find(|(n, _)| n == "Subject" || n == "Content-Type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    format!(
        "---Message {}/{}: {} -- {}%",
        app.sel + 1,
        app.visible.len(),
        subject,
        shown * 100 / total,
    )
}

#[cfg(test)]
mod tests {
    use super::{humanize_size, wrap_line};

    #[test]
    fn wrap_short_line_untouched() {
        assert_eq!(wrap_line("hello", 10), vec!["hello"]);
        assert_eq!(wrap_line("", 10), vec![""]);
    }

    #[test]
    fn wrap_breaks_at_word_boundary() {
        assert_eq!(
            wrap_line("the quick brown fox", 10),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(wrap_line("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn humanize_size_ranges() {
        assert_eq!(humanize_size(0), "0");
        assert_eq!(humanize_size(999), "999");
        assert_eq!(humanize_size(2048), "2.0K");
        assert_eq!(humanize_size(204800), "200K");
        assert_eq!(humanize_size(2 * 1024 * 1024), "2.0M");
    }
}
