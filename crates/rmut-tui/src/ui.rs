use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rmut_core::format::{self, IndexFields};
use rmut_core::message::{self, MessageView, Part};

use crate::app::{App, Mode, Pager, Prompt};

const INDEX_HELP: &str = "?:Help q:Quit Enter:View m:New r:Reply g:Grp f:Fwd d:Del u:Undel F:Flag t:Tag s:Save o:Sort l:Limit /:Find c:Mbox y:Fldrs v:Parts p:Print $:Sync";
const PAGER_HELP: &str = "?:Help q:Back Enter:Scroll Space/-:Page j/k:Msg /:Find r:Reply f:Fwd d:Del s:Save h:Hdrs v:Parts p:Print";
const ATTACH_HELP: &str = "q:Back j/k:Move Enter:View s:Save |:Pipe p:Print";
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open c:Browse C:Create";
const COMPOSE_HELP: &str = "y:Send e:Edit Enter:View t:To c:Cc b:Bcc s:Subj a:Attach D:Detach d:Desc ^T:Type f:Fcc p:PGP P:Postpone q:Quit";
const HELP_HELP: &str = "q:Back j/k:Scroll Space/-:Page";
const POSTPONED_HELP: &str = "q:Back j/k:Move Enter:Recall";
const QUERY_HELP: &str = "q:Back j/k:Move Enter:Compose";

pub fn draw(frame: &mut Frame, app: &mut App) {
    // mutt's four regions: the help bar, the mailbox or message, the
    // status bar, and the message line under it. The message line is
    // always there, empty when there is nothing to say, so a note
    // never crowds the status bar out and the content never jumps.
    let [help_area, content_area, status_area, message_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let help = match app.mode {
        Mode::Index => INDEX_HELP,
        Mode::Pager(_) => PAGER_HELP,
        Mode::Attach { .. } => ATTACH_HELP,
        Mode::Compose { .. } => COMPOSE_HELP,
        Mode::Folders { .. } => FOLDERS_HELP,
        Mode::Postponed { .. } => POSTPONED_HELP,
        Mode::Query { .. } => QUERY_HELP,
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
            draw_pager(frame, pager_area, app, pager);
        }
    } else {
        // The sidebar takes a left slice of the index view.
        let content_area = if matches!(app.mode, Mode::Index) && app.sidebar_visible {
            let width = app
                .config
                .sidebar
                .width
                .clamp(10, (content_area.width / 2).max(10));
            let [side_area, rest] =
                Layout::horizontal([Constraint::Length(width), Constraint::Min(1)])
                    .areas(content_area);
            draw_sidebar(frame, side_area, app);
            rest
        } else {
            content_area
        };
        match &app.mode {
            Mode::Index => draw_index(frame, content_area, app),
            Mode::Pager(_) => unreachable!(),
            Mode::Compose { sel } => draw_compose(frame, content_area, app, *sel),
            Mode::Attach { parts, sel, .. } => draw_attach(frame, content_area, parts, *sel),
            Mode::Folders { dirs, sel, .. } => draw_folders(frame, content_area, dirs, *sel),
            Mode::Postponed { drafts, sel } => draw_postponed(frame, content_area, drafts, *sel),
            Mode::Query { results, sel } => draw_list(frame, content_area, results, *sel),
            Mode::Help { lines, scroll } => draw_help(frame, content_area, lines, *scroll),
        }
    }
    draw_status_bar(frame, status_area, app, content_area.height);
    draw_message_line(frame, message_area, app);
}

/// Mutt's compose menu: header lines, then the attachment table with
/// the selection highlighted.
fn draw_compose(frame: &mut Frame, area: Rect, app: &App, sel: usize) {
    let mut lines: Vec<Line> = Vec::new();
    let header_color = app.theme.header;
    for (name, value) in app.compose_header_lines() {
        lines.push(Line::from(vec![
            Span::styled(format!("{name:>9}: "), Style::new().fg(header_color).bold()),
            Span::raw(value),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        "-- Attachments",
        Style::new().fg(header_color).bold(),
    ));
    for (i, entry) in app.compose_entries().into_iter().enumerate() {
        let text = format!(" {:>2} {entry}", i + 1);
        lines.push(if i == sel {
            Line::styled(text, Style::new().add_modifier(Modifier::REVERSED))
        } else {
            Line::raw(text)
        });
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// ---- sidebar ----

/// A short label for a sidebar entry: the folder part of an `imap:`
/// spec, the last path component of a local maildir.
fn sidebar_label(spec: &str) -> &str {
    if let Some(rest) = spec.strip_prefix("imap:") {
        return rest;
    }
    spec.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(spec)
}

fn draw_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    for (i, (spec, new)) in app.sidebar.iter().enumerate().take(area.height as usize) {
        let open = app.sidebar_open == Some(i);
        let mut text = format!("{}{}", if open { ">" } else { " " }, sidebar_label(spec));
        if *new > 0 {
            text += &format!(" ({new})");
        }
        let mut chars: Vec<char> = text.chars().collect();
        chars.truncate(width.saturating_sub(1));
        let text: String = chars.into_iter().collect();
        let mut style = Style::new();
        if i == app.sidebar_sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if *new > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(
            format!("{:<w$}\u{2502}", text, w = width.saturating_sub(1)),
            style,
        )));
    }
    // Continue the separator down the empty rows.
    for _ in lines.len()..area.height as usize {
        lines.push(Line::from(format!(
            "{:<w$}\u{2502}",
            "",
            w = width.saturating_sub(1)
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
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
    // `~m` / `~=` in a [[color_index]] rule need the numbering on
    // screen and the repeated Message-IDs, counted once per draw.
    let mut id_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for m in &app.msgs {
        if let Some(id) = m.env.msg_id.as_deref() {
            *id_counts.entry(id).or_default() += 1;
        }
    }
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
        // Third %Z slot, mutt-style: tag mark, or mutt's $to_chars
        // (" +TCFL") for how the mail relates to me. Precedence is
        // mutt's: sent by me, then To (alone, or among others), then
        // Cc, then a subscribed list.
        let me = app.me();
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
            .any(|a| app.subscribed.iter().any(|m| m.is_match(a)))
        {
            'L'
        } else {
            ' '
        };
        let (depth, hidden) = app.thread_info(mi);
        let fmt = app
            .config
            .index
            .format
            .as_deref()
            .unwrap_or(format::DEFAULT_FORMAT);
        let mut subject = env.subject.clone();
        // The hidden count rides on the subject unless the format
        // places it itself with %M.
        if let Some(n) = hidden
            && !fmt.contains("%M")
            && !fmt.contains("?M?")
        {
            subject += &format!(" ({n} hidden)");
        }
        if depth > 0 {
            subject = format!("{}└>{subject}", "  ".repeat(depth - 1));
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
                    app.config.index.date_format.as_deref(),
                ),
                from: &env.from,
                size: &humanize_size(env.file.size),
                lines: env.lines,
                list: env.list.as_deref(),
                hidden,
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
        // The first matching [[color_index]] rule wins over the
        // built-in slot colors.
        let pos = rmut_core::pattern::Position {
            number: vi + 1,
            current: app.sel + 1,
            last: app.visible.len(),
            duplicate: env
                .msg_id
                .as_deref()
                .is_some_and(|id| id_counts.get(id).copied().unwrap_or(0) > 1),
        };
        if let Some((_, rule)) = app.index_rules.iter().find(|(patterns, _)| {
            rmut_core::pattern::matches_in(patterns, env, app.scope(pos), None)
        }) {
            style = style.patch(*rule);
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

/// How one pager display row gets colored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RowKind {
    Header,
    /// `[-- ... --]` notices (PGP verdicts, missing parts).
    Marker,
    /// Quoted body text, 1-based nesting depth.
    Quoted(usize),
    Text,
}

pub struct Row {
    pub text: String,
    pub kind: RowKind,
}

/// Quote depth of a body line under $quote_regexp: the number of
/// quote characters in the prefix match, 0 for unquoted text.
pub fn quote_depth(line: &str, re: &regex_lite::Regex) -> usize {
    match re.find(line) {
        Some(m) if m.start() == 0 => m
            .as_str()
            .chars()
            .filter(|c| !c.is_whitespace())
            .count()
            .max(1),
        _ => 0,
    }
}

/// The pager display: header block, separator, wrapped body (with
/// mutt's `+` continuation markers), each row classified for
/// coloring. The scroll math, the body search, and the drawing all
/// share this; T (hide_quoted) drops quoted rows here, so every
/// consumer agrees on what a line number means.
pub fn pager_rows(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    quote_re: &regex_lite::Regex,
    hide_quoted: bool,
) -> Vec<Row> {
    let headers = if full_headers { &view.all } else { &view.brief };
    let mut rows: Vec<Row> = headers
        .iter()
        .map(|(name, value)| Row {
            text: format!("{name}: {value}"),
            kind: RowKind::Header,
        })
        .collect();
    rows.push(Row {
        text: String::new(),
        kind: RowKind::Text,
    });
    for line in view.body.lines() {
        // Marker lines like the PGP verdict get the header treatment.
        let marker = line.starts_with("[-- ") && line.ends_with(" --]");
        let depth = if marker {
            0
        } else {
            quote_depth(line, quote_re)
        };
        if hide_quoted && depth > 0 {
            continue;
        }
        let kind = if marker {
            RowKind::Marker
        } else if depth > 0 {
            RowKind::Quoted(depth)
        } else {
            RowKind::Text
        };
        for (i, wrapped) in wrap_line(line, width.saturating_sub(1))
            .into_iter()
            .enumerate()
        {
            let text = if i > 0 {
                format!("+{wrapped}")
            } else {
                wrapped
            };
            rows.push(Row { text, kind });
        }
    }
    rows
}

/// The pager's display as plain text: what the body search runs over.
pub fn pager_text_lines(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    quote_re: &regex_lite::Regex,
    hide_quoted: bool,
) -> Vec<String> {
    pager_rows(view, width, full_headers, quote_re, hide_quoted)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

/// Total pager lines at the given width.
pub fn pager_line_count(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    quote_re: &regex_lite::Regex,
    hide_quoted: bool,
) -> usize {
    pager_rows(view, width, full_headers, quote_re, hide_quoted).len()
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

fn draw_pager(frame: &mut Frame, area: Rect, app: &App, pager: &Pager) {
    let rows = pager_rows(
        &pager.view,
        app.pager_wrap(area.width as usize),
        pager.full_headers,
        &app.quote_re,
        pager.hide_quoted,
    );
    let mut visible: Vec<Line> = rows
        .iter()
        .skip(pager.scroll)
        .take(area.height as usize)
        .map(|row| style_row(row, app))
        .collect();
    // mutt's $tilde: mark the void below end-of-message.
    if app.config.pager.tilde {
        while visible.len() < area.height as usize {
            visible.push(Line::raw("~"));
        }
    }
    frame.render_widget(Paragraph::new(visible), area);
}

/// One pager row as styled spans: the base from its kind, then
/// [[color_body]] spans, then search hits on top (body rows only).
fn style_row(row: &Row, app: &App) -> Line<'static> {
    let header_style = Style::new().fg(app.theme.header).bold();
    match row.kind {
        RowKind::Header => match row.text.split_once(": ") {
            Some((name, value)) => Line::from(vec![
                Span::styled(format!("{name}: "), header_style),
                Span::raw(value.to_string()),
            ]),
            None => Line::styled(row.text.clone(), header_style),
        },
        RowKind::Marker => Line::styled(row.text.clone(), header_style),
        RowKind::Quoted(depth) => {
            let base = match app.theme.quoted.len() {
                0 => Style::new(),
                n => Style::new().fg(app.theme.quoted[(depth - 1) % n]),
            };
            body_line(&row.text, base, app)
        }
        RowKind::Text => body_line(&row.text, Style::new(), app),
    }
}

fn body_line(text: &str, base: Style, app: &App) -> Line<'static> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        return Line::raw(String::new());
    }
    let mut styles = vec![base; chars.len()];
    let paint = |styles: &mut Vec<Style>, start: usize, end: usize, patch: Style| {
        for (i, (off, _)) in chars.iter().enumerate() {
            if *off >= start && *off < end {
                styles[i] = styles[i].patch(patch);
            }
        }
    };
    for (re, style) in &app.body_rules {
        for m in re.find_iter(text) {
            paint(&mut styles, m.start(), m.end(), *style);
        }
    }
    if let Some(matcher) = &app.pager_search {
        for (start, end) in matcher.find_ranges(text) {
            paint(&mut styles, start, end, app.theme.search);
        }
    }
    // Group equal-style runs into spans.
    let mut spans: Vec<Span> = Vec::new();
    let mut cur = String::new();
    let mut cur_style = styles[0];
    for (i, (_, c)) in chars.iter().enumerate() {
        if styles[i] != cur_style {
            spans.push(Span::styled(std::mem::take(&mut cur), cur_style));
            cur_style = styles[i];
        }
        cur.push(*c);
    }
    spans.push(Span::styled(cur, cur_style));
    Line::from(spans)
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
    let rows = area.height as usize;
    // Keep the selection on screen in deep directory listings.
    let offset = (sel + 1).saturating_sub(rows);
    let mut lines = Vec::new();
    for (i, (dir, new)) in dirs.iter().enumerate().skip(offset).take(rows) {
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

/// A plain numbered pick-list (query results).
fn draw_list(frame: &mut Frame, area: Rect, items: &[String], sel: usize) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    for (i, item) in items.iter().enumerate().take(area.height as usize) {
        let text = format!("{:>3} {}", i + 1, item);
        let text = format!("{text:<width$}");
        let mut style = Style::new();
        if i == sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_postponed(
    frame: &mut Frame,
    area: Rect,
    drafts: &[(std::path::PathBuf, String)],
    sel: usize,
) {
    let width = area.width as usize;
    let mut lines = Vec::new();
    for (i, (_, label)) in drafts.iter().enumerate().take(area.height as usize) {
        let text = format!("{:>3} {}", i + 1, label);
        let text = format!("{text:<width$}");
        let mut style = Style::new();
        if i == sel {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(text, style)));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// ---- the two bottom lines: status bar, then the message line ----

/// mutt's status bar: what mailbox this is and what is in it. It says
/// the same thing whatever else is going on, so a note or a prompt
/// never costs you the mailbox you are looking at.
fn draw_status_bar(frame: &mut Frame, area: Rect, app: &App, content_height: u16) {
    let text = match &app.mode {
        Mode::Pager(pager) => {
            let height = content_height.saturating_sub(app.config.pager.index_lines);
            pager_status(app, pager, height, area.width as usize)
        }
        Mode::Compose { .. } => format!(
            "---rmut: compose [Atts:{}]",
            app.compose_entries().len().saturating_sub(1)
        ),
        Mode::Attach { parts, .. } => format!("---rmut: attachments [Parts:{}]", parts.len()),
        Mode::Folders { dirs, .. } => format!("---rmut: mailboxes [Found:{}]", dirs.len()),
        Mode::Postponed { drafts, .. } => {
            format!("---rmut: postponed drafts [Found:{}]", drafts.len())
        }
        Mode::Query { results, .. } => format!("---rmut: query results [Found:{}]", results.len()),
        Mode::Help { .. } => "---rmut: help".to_string(),
        Mode::Index => index_status(app, area.width as usize, content_height as usize),
    };
    frame.render_widget(Line::from(text).style(app.theme.bar_style()), area);
}

/// mutt's message line: the prompt you are answering, or the last
/// thing rmut had to say, and empty when it has nothing.
fn draw_message_line(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(prompt) = &app.prompt {
        let mut text = match prompt {
            Prompt::Line {
                label, buf, cursor, ..
            } => {
                // The marker sits at the cursor, not always at the end.
                let i = crate::app::byte_at(buf, *cursor);
                format!("{label}{}\u{2581}{}", &buf[..i], &buf[i..])
            }
            Prompt::Key { label, .. } => label.clone(),
        };
        // Completion hints and the like show behind the input.
        if let Some(notice) = app.notice() {
            text += &format!("  [{}]", notice.text());
        }
        frame.render_widget(Line::from(text), area);
        return;
    }
    let Some(notice) = app.notice() else {
        return;
    };
    let line = Line::from(notice.text());
    let line = match notice.is_error() {
        true => line.style(app.theme.error),
        false => line,
    };
    frame.render_widget(line, area);
}

fn index_status(app: &App, width: usize, rows: usize) -> String {
    let fmt = app
        .config
        .ui
        .status_format
        .as_deref()
        .unwrap_or(format::DEFAULT_STATUS_FORMAT);
    format::render_status(fmt, width, &|spec| match spec {
        'f' => app.title.clone(),
        'm' => app.msgs.len().to_string(),
        // Shown message count, only when a limit narrows the view.
        'M' => {
            if app.visible.len() != app.msgs.len() {
                app.visible.len().to_string()
            } else {
                String::new()
            }
        }
        'n' => app.new_count().to_string(),
        'u' => app
            .msgs
            .iter()
            .filter(|m| !m.env.file.flags.seen)
            .count()
            .to_string(),
        'd' => app.deleted_count().to_string(),
        'F' => app
            .msgs
            .iter()
            .filter(|m| m.env.file.flags.flagged)
            .count()
            .to_string(),
        't' => app.msgs.iter().filter(|m| m.env.tagged).count().to_string(),
        's' => format!(
            "{}{}",
            app.sort.name(),
            if app.sort_rev { "-rev" } else { "" }
        ),
        'V' => app
            .limit
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default(),
        'r' => {
            if app.read_only {
                "%".to_string() // mutt's readonly mark
            } else if app.pending_count() > 0 {
                "*".to_string()
            } else {
                String::new()
            }
        }
        'v' => env!("CARGO_PKG_VERSION").to_string(),
        // Index scroll position, like mutt's %P.
        'P' => {
            let len = app.visible.len();
            if len <= rows {
                "all".into()
            } else if app.index_offset == 0 {
                "top".into()
            } else if app.index_offset + rows >= len {
                "bot".into()
            } else {
                format!("{}%", (app.index_offset + rows) * 100 / len)
            }
        }
        '%' => "%".to_string(),
        other => format!("%{other}"),
    })
}

/// The classic pager bottom line; override with `[pager] format`.
const DEFAULT_PAGER_FORMAT: &str = "---Message %C/%m: %s -- %P";

/// mutt's $pager_format: %C message number, %m count, %n sender,
/// %s subject, %Z status chars, %P percent through the message,
/// %f mailbox, plus the conditional and %> machinery.
fn pager_status(app: &App, pager: &Pager, content_height: u16, width: usize) -> String {
    let total = pager_line_count(
        &pager.view,
        app.pager_wrap(width),
        pager.full_headers,
        &app.quote_re,
        pager.hide_quoted,
    )
    .max(1);
    let shown = (pager.scroll + content_height as usize).min(total);
    let subject = pager
        .view
        .brief
        .iter()
        .find(|(n, _)| n == "Subject" || n == "Content-Type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let msg = app.visible.get(app.sel).map(|&i| &app.msgs[i]);
    let fmt = app
        .config
        .pager
        .format
        .as_deref()
        .unwrap_or(DEFAULT_PAGER_FORMAT);
    format::render_status(fmt, width, &|spec| match spec {
        'C' => (app.sel + 1).to_string(),
        'm' => app.visible.len().to_string(),
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
        'P' => format!("{}%", shown * 100 / total),
        'f' => app.title.clone(),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{RowKind, humanize_size, pager_rows, quote_depth, wrap_line};
    use crate::app::default_quote_re;
    use rmut_core::message::MessageView;

    #[test]
    fn quote_depth_counts_prefix_marks() {
        let re = default_quote_re();
        assert_eq!(quote_depth("plain text", &re), 0);
        assert_eq!(quote_depth("> quoted", &re), 1);
        assert_eq!(quote_depth("> > deeper", &re), 2);
        assert_eq!(quote_depth(">>tight", &re), 2);
        assert_eq!(quote_depth("  | indented pipe", &re), 1);
        // A > later in the line is not a quote.
        assert_eq!(quote_depth("2 > 1", &re), 0);
    }

    #[test]
    fn rows_classify_and_hide_quoted() {
        let view = MessageView {
            brief: vec![("From".into(), "jane@example.com".into())],
            all: vec![("From".into(), "jane@example.com".into())],
            body: "top\n> one\n> > two\n[-- marker --]\ntail".into(),
        };
        let re = default_quote_re();
        let rows = pager_rows(&view, 80, false, &re, false);
        let kinds: Vec<RowKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::Header,
                RowKind::Text, // separator
                RowKind::Text,
                RowKind::Quoted(1),
                RowKind::Quoted(2),
                RowKind::Marker,
                RowKind::Text,
            ]
        );
        // T drops the quoted rows for every consumer at once.
        let hidden = pager_rows(&view, 80, false, &re, true);
        assert_eq!(hidden.len(), rows.len() - 2);
        assert!(hidden.iter().all(|r| !matches!(r.kind, RowKind::Quoted(_))));
    }

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
