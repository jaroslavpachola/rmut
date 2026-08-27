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
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open c:Browse C:Create d:Del r:Rename s/u:Sub";
const COMPOSE_HELP: &str = "y:Send e:Edit Enter:View t:To c:Cc b:Bcc s:Subj a:Attach D:Detach d:Desc ^T:Type f:Fcc p:PGP P:Postpone q:Quit";
const HELP_HELP: &str = "q:Back j/k:Scroll Space/-:Page";
const POSTPONED_HELP: &str = "q:Back j/k:Move Enter:Recall";
const QUERY_HELP: &str = "q:Back j/k:Move Enter:Compose";

pub fn draw(frame: &mut Frame, app: &mut App) {
    // mutt's four regions: the help bar, the mailbox or message, the
    // status bar, and the message line under it. The message line is
    // always there, empty when there is nothing to say, so a note
    // never crowds the status bar out and the content never jumps.
    // mutt's $status_on_top: the status and message rows go just under
    // the help bar rather than at the bottom.
    let on_top = app.session.config.ui.status_on_top.unwrap_or(false);
    // mutt's $help: without it the top line goes to the content.
    let help_rows = if app.session.config.ui.help.unwrap_or(true) {
        1
    } else {
        0
    };
    let areas: [Rect; 4] = if on_top {
        Layout::vertical([
            Constraint::Length(help_rows),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .areas(frame.area())
    } else {
        Layout::vertical([
            Constraint::Length(help_rows),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area())
    };
    let (help_area, content_area, status_area, message_area) = if on_top {
        (areas[0], areas[3], areas[1], areas[2])
    } else {
        (areas[0], areas[1], areas[2], areas[3])
    };

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
    if help_rows > 0 {
        frame.render_widget(Line::from(help).style(app.theme.bar_style()), help_area);
    }

    if matches!(app.mode, Mode::Pager(_)) {
        // Optionally keep a slice of the index visible above the pager
        // (mutt's pager_index_lines).
        let index_lines = app
            .session
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
                .session
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
    if app.session.visible.is_empty() {
        let text = if app.session.limit.is_some() {
            "No messages match the limit (l clears it)."
        } else {
            "No mail in mailbox."
        };
        frame.render_widget(Paragraph::new(text).dim(), area);
        return;
    }
    let rows = area.height as usize;
    let ui = &app.session.config.ui;
    let arrow = ui.arrow_cursor.unwrap_or(false);
    // Keep the selection visible, the way mutt's menu does.
    app.index_offset = recenter(
        app.index_offset,
        app.session.sel,
        rows,
        app.session.visible.len(),
        Menu {
            scroll: ui.menu_scroll.unwrap_or(true),
            context: ui.menu_context,
            move_off: ui.menu_move_off.unwrap_or(true),
        },
    );
    let width = area.width as usize;
    let mut lines = Vec::with_capacity(rows);
    // `~m` / `~=` in a [[color_index]] rule need the numbering on
    // screen and the repeated Message-IDs, counted once per draw.
    let mut id_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for m in &app.session.msgs {
        if let Some(id) = m.env.msg_id.as_deref() {
            *id_counts.entry(id).or_default() += 1;
        }
    }
    for (vi, &mi) in app
        .session
        .visible
        .iter()
        .enumerate()
        .skip(app.index_offset)
        .take(rows)
    {
        let msg = &app.session.msgs[mi];
        let env = &msg.env;
        let status = env.file.flags.status_char(env.file.is_new);
        let flagged = if env.file.flags.flagged { '!' } else { ' ' };
        // Third %Z slot, mutt-style: tag mark, or mutt's $to_chars
        // (" +TCFL") for how the mail relates to me. Precedence is
        // mutt's: sent by me, then To (alone, or among others), then
        // Cc, then a subscribed list.
        let me = app.session.me();
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
            .any(|a| app.session.subscribed.iter().any(|m| m.is_match(a)))
        {
            'L'
        } else {
            ' '
        };
        let (depth, hidden) = app.session.thread_info(mi);
        let fmt = app
            .session
            .config
            .index
            .format
            .as_deref()
            .unwrap_or(format::DEFAULT_FORMAT);
        // mutt's $hide_thread_subject: a reply repeating its parent's
        // subject shows only the tree arrow.
        let mut subject = if app.session.subject_hidden(mi) {
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
                    app.session.config.index.date_format.as_deref(),
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
            current: app.session.sel + 1,
            last: app.session.visible.len(),
            duplicate: env
                .msg_id
                .as_deref()
                .is_some_and(|id| id_counts.get(id).copied().unwrap_or(0) > 1),
        };
        if let Some((_, rule)) = app.index_rules.iter().find(|(patterns, _)| {
            rmut_core::pattern::matches_in(patterns, env, app.session.scope(pos), None)
        }) {
            style = style.patch(*rule);
        }
        // mutt's $arrow_cursor: an arrow marks the selection instead
        // of reverse video.
        let text = if arrow {
            format!("{}{text}", if vi == app.session.sel { "->" } else { "  " })
        } else {
            if vi == app.session.sel {
                style = style.add_modifier(Modifier::REVERSED);
            }
            text
        };
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
/// What the config says about drawing a message: which lines count as
/// quoted, whether a wrapped line is marked, and whether it breaks at
/// a word.
pub struct PagerStyle<'a> {
    pub quote_re: &'a regex_lite::Regex,
    /// mutt's $markers.
    pub markers: bool,
    /// mutt's $smart_wrap.
    pub smart_wrap: bool,
}

impl<'a> PagerStyle<'a> {
    pub fn of(config: &'a rmut_core::config::Config, quote_re: &'a regex_lite::Regex) -> Self {
        PagerStyle {
            quote_re,
            markers: config.pager.markers.unwrap_or(true),
            smart_wrap: config.pager.smart_wrap.unwrap_or(true),
        }
    }
}

pub fn pager_rows(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    style: &PagerStyle,
    hide_quoted: bool,
) -> Vec<Row> {
    let quote_re = style.quote_re;
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
        for (i, wrapped) in wrap_line_with(line, width.saturating_sub(1), style.smart_wrap)
            .into_iter()
            .enumerate()
        {
            // mutt's $markers: a wrapped line says it is one.
            let text = match i > 0 && style.markers {
                true => format!("+{wrapped}"),
                false => wrapped,
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
    style: &PagerStyle,
    hide_quoted: bool,
) -> Vec<String> {
    pager_rows(view, width, full_headers, style, hide_quoted)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

/// Total pager lines at the given width.
pub fn pager_line_count(
    view: &MessageView,
    width: usize,
    full_headers: bool,
    style: &PagerStyle,
    hide_quoted: bool,
) -> usize {
    pager_rows(view, width, full_headers, style, hide_quoted).len()
}

/// Word-wrap one body line to `width` columns (hard break when a single
/// word is longer than the line). Tabs are expanded first.
/// The same, with mutt's $smart_wrap: without it a long line breaks
/// at the column rather than at the last space before it.
pub fn wrap_line_with(line: &str, width: usize, smart: bool) -> Vec<String> {
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
        let brk = match smart {
            true => (start + 1..window_end)
                .rev()
                .find(|&i| chars[i] == ' ')
                .unwrap_or(window_end),
            false => window_end,
        };
        out.push(chars[start..brk].iter().collect());
        start = if chars.get(brk) == Some(&' ') {
            brk + 1
        } else {
            brk
        };
    }
    out
}

/// mutt's $menu_scroll, $menu_context and $menu_move_off, as the
/// index's recentering reads them.
#[derive(Clone, Copy)]
pub struct Menu {
    pub scroll: bool,
    pub context: usize,
    pub move_off: bool,
}

/// Where the index's first row goes so the cursor stays on screen:
/// mutt's menu_check_recenter, line for line. `top` is the row now at
/// the top, `sel` the cursor, `rows` the screen, `max` the entries.
/// With `scroll` the view moves just far enough (keeping `context`
/// lines beyond the cursor); without it a whole page turns. Unless
/// `move_off`, the last entry never scrolls up past the bottom.
pub fn recenter(top: usize, sel: usize, rows: usize, max: usize, menu: Menu) -> usize {
    let (mut top, sel, rows, max) = (top as i64, sel as i64, rows as i64, max as i64);
    let c = (menu.context as i64).min(rows / 2);
    if !menu.move_off && max <= rows {
        top = 0;
    } else if menu.scroll || rows <= 0 || c < menu.context as i64 {
        if sel < top + c {
            top = sel - c;
        } else if sel >= top + rows - c {
            top = sel - rows + c + 1;
        }
    } else if sel < top + c {
        top -= (rows - c) * ((top + rows - 1 - sel) / (rows - c)) - c;
    } else if sel >= top + rows - c {
        top += (rows - c) * ((sel - top) / (rows - c)) - c;
    }
    if !menu.move_off {
        top = top.min(max - rows);
    }
    top.max(0) as usize
}

fn draw_pager(frame: &mut Frame, area: Rect, app: &App, pager: &Pager) {
    let rows = pager_rows(
        &pager.view,
        app.pager_wrap(area.width as usize),
        pager.full_headers,
        &PagerStyle::of(&app.session.config, &app.session.quote_re),
        pager.hide_quoted,
    );
    let mut visible: Vec<Line> = rows
        .iter()
        .skip(pager.scroll)
        .take(area.height as usize)
        .map(|row| style_row(row, app))
        .collect();
    // mutt's $tilde: mark the void below end-of-message.
    if app.session.config.pager.tilde {
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
    if !app.pager_search_off
        && let Some(matcher) = &app.pager_search
    {
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
            let height = content_height.saturating_sub(app.session.config.pager.index_lines);
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
        .session
        .config
        .ui
        .status_format
        .as_deref()
        .unwrap_or(format::DEFAULT_STATUS_FORMAT);
    format::render_status(fmt, width, &|spec| index_status_field(app, spec, rows))
}

/// mutt's $ts_status_format: the terminal title, from the same fields
/// as the status line (a wide width, so %>… padding does not clip).
pub fn index_title(app: &App, rows: usize) -> String {
    let fmt = app
        .session
        .config
        .ui
        .title_format
        .as_deref()
        .unwrap_or("rmut: %f");
    format::render_status(fmt, 200, &|spec| index_status_field(app, spec, rows))
        .trim_end()
        .to_string()
}

/// One status specifier's value, shared by the bottom bar and the
/// terminal title.
fn index_status_field(app: &App, spec: char, rows: usize) -> String {
    match spec {
        'f' => app.session.title.clone(),
        'm' => app.session.msgs.len().to_string(),
        // Shown message count, only when a limit narrows the view.
        'M' => {
            if app.session.visible.len() != app.session.msgs.len() {
                app.session.visible.len().to_string()
            } else {
                String::new()
            }
        }
        'n' => app.session.new_count().to_string(),
        'u' => app
            .session
            .msgs
            .iter()
            .filter(|m| !m.env.file.flags.seen)
            .count()
            .to_string(),
        'd' => app.session.deleted_count().to_string(),
        'F' => app
            .session
            .msgs
            .iter()
            .filter(|m| m.env.file.flags.flagged)
            .count()
            .to_string(),
        't' => app
            .session
            .msgs
            .iter()
            .filter(|m| m.env.tagged)
            .count()
            .to_string(),
        's' => format!(
            "{}{}",
            app.session.sort.name(),
            if app.session.sort_rev { "-rev" } else { "" }
        ),
        'V' => app
            .session
            .limit
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default(),
        'r' => {
            // mutt's $status_chars: [0] unchanged, [1] changed, [2]
            // read-only. Unset keeps rmut's own marks.
            let chars: Option<Vec<char>> = app
                .session
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
            if app.session.read_only {
                pick(2, "%")
            } else if app.session.pending_count() > 0 {
                pick(1, "*")
            } else {
                pick(0, "")
            }
        }
        'v' => env!("CARGO_PKG_VERSION").to_string(),
        // Index scroll position, like mutt's %P.
        'P' => {
            let len = app.session.visible.len();
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
    }
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
        &PagerStyle::of(&app.session.config, &app.session.quote_re),
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
    let msg = app
        .session
        .visible
        .get(app.session.sel)
        .map(|&i| &app.session.msgs[i]);
    let fmt = app
        .session
        .config
        .pager
        .format
        .as_deref()
        .unwrap_or(DEFAULT_PAGER_FORMAT);
    format::render_status(fmt, width, &|spec| match spec {
        'C' => (app.session.sel + 1).to_string(),
        'm' => app.session.visible.len().to_string(),
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
        'f' => app.session.title.clone(),
        '%' => "%".to_string(),
        other => format!("%{other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Menu, PagerStyle, RowKind, humanize_size, pager_rows, quote_depth, recenter, wrap_line_with,
    };
    use rmut_core::message::MessageView;
    use rmut_session::default_quote_re;

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
        let style = PagerStyle {
            quote_re: &re,
            markers: true,
            smart_wrap: true,
        };
        let rows = pager_rows(&view, 80, false, &style, false);
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
        let hidden = pager_rows(&view, 80, false, &style, true);
        assert_eq!(hidden.len(), rows.len() - 2);
        assert!(hidden.iter().all(|r| !matches!(r.kind, RowKind::Quoted(_))));
    }

    #[test]
    fn wrap_short_line_untouched() {
        assert_eq!(wrap_line_with("hello", 10, true), vec!["hello"]);
        assert_eq!(wrap_line_with("", 10, true), vec![""]);
    }

    #[test]
    fn wrap_breaks_at_word_boundary() {
        assert_eq!(
            wrap_line_with("the quick brown fox", 10, true),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn without_smart_wrap_a_line_breaks_at_the_column() {
        // mutt's $smart_wrap off: the break lands on the width, not
        // on the last space before it.
        assert_eq!(
            wrap_line_with("alpha beta gamma", 10, false),
            vec!["alpha beta", "gamma"]
        );
        assert_eq!(
            wrap_line_with("alpha beta gamma", 10, true),
            vec!["alpha", "beta gamma"]
        );
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(
            wrap_line_with("abcdefghij", 4, true),
            vec!["abcd", "efgh", "ij"]
        );
    }

    #[test]
    fn humanize_size_ranges() {
        assert_eq!(humanize_size(0), "0");
        assert_eq!(humanize_size(999), "999");
        assert_eq!(humanize_size(2048), "2.0K");
        assert_eq!(humanize_size(204800), "200K");
        assert_eq!(humanize_size(2 * 1024 * 1024), "2.0M");
    }

    #[test]
    fn recenter_scrolls_a_line_or_turns_a_page() {
        let scroll = Menu {
            scroll: true,
            context: 0,
            move_off: true,
        };
        let page = Menu {
            scroll: false,
            context: 0,
            move_off: true,
        };
        // Moving down off a 10-row screen: scrolling shows one more
        // line, paging turns the whole page (mutt's default).
        assert_eq!(recenter(0, 10, 10, 100, scroll), 1);
        assert_eq!(recenter(0, 10, 10, 100, page), 10);
        // Moving up off the top is symmetrical.
        assert_eq!(recenter(20, 19, 10, 100, scroll), 19);
        assert_eq!(recenter(20, 19, 10, 100, page), 10);
        // On screen already: nothing moves.
        assert_eq!(recenter(20, 25, 10, 100, scroll), 20);
        assert_eq!(recenter(20, 25, 10, 100, page), 20);
    }

    #[test]
    fn recenter_keeps_context_lines() {
        let m = Menu {
            scroll: true,
            context: 3,
            move_off: true,
        };
        // The cursor stays three rows clear of the bottom edge.
        assert_eq!(recenter(0, 7, 10, 100, m), 1);
        // And of the top edge.
        assert_eq!(recenter(20, 22, 10, 100, m), 19);
        // Context is capped at half the screen (a 4-row screen: 2).
        let big = Menu {
            scroll: true,
            context: 9,
            move_off: true,
        };
        assert_eq!(recenter(0, 2, 4, 100, big), 1);
    }

    #[test]
    fn recenter_move_off_pins_the_bottom() {
        let stuck = Menu {
            scroll: true,
            context: 0,
            move_off: false,
        };
        // Fewer entries than rows: the top is always the top.
        assert_eq!(recenter(3, 4, 10, 5, stuck), 0);
        // The last page stays full: top never passes max - rows.
        assert_eq!(recenter(95, 99, 10, 100, stuck), 90);
        // With move_off (the default) it may.
        let free = Menu {
            scroll: true,
            context: 0,
            move_off: true,
        };
        assert_eq!(recenter(95, 99, 10, 100, free), 95);
    }
}
