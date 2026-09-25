use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use rmut_core::message::Part;

use rmut_front::pager::{Menu, PagerStyle, Row, RowKind, humanize_size, pager_rows, recenter};
use rmut_front::status;

use crate::app::{App, Mode, Pager, Prompt};
use crate::theme;

const INDEX_HELP: &str = "?:Help q:Quit Enter:View m:New r:Reply g:Grp f:Fwd d:Del u:Undel F:Flag t:Tag s:Save o:Sort l:Limit /:Find c:Mbox y:Fldrs v:Parts p:Print $:Sync";
const PAGER_HELP: &str = "?:Help q:Back Enter:Scroll Space/-:Page j/k:Msg /:Find r:Reply f:Fwd d:Del s:Save h:Hdrs v:Parts p:Print";
const ATTACH_HELP: &str =
    "q:Back j/k:Move Enter:View m:Mailcap T:Text R:Render s:Save |:Pipe p:Print f:Fwd";
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open c:Browse C:Create d:Del r:Rename s/u:Sub";
const COMPOSE_HELP: &str = "y:Send e:Edit Enter:View t:To c:Cc b:Bcc s:Subj a:Attach A:AttMsg n:New D:Detach d:Desc ^T:Type ^O:Name u:Unlink K/J:Move w:Write i:Spell f:Fcc p:PGP P:Postpone q:Quit";
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
        frame.render_widget(
            Line::from(help).style(theme::style(app.theme.bar_style())),
            help_area,
        );
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
    let header_color = theme::color(app.theme.header);
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
    let id_counts = rmut_front::index::id_counts(&app.session);
    for (vi, &mi) in app
        .session
        .visible
        .iter()
        .enumerate()
        .skip(app.index_offset)
        .take(rows)
    {
        let (text, style) = rmut_front::index::row(
            &app.session,
            &app.theme,
            &app.index_rules,
            &id_counts,
            vi,
            mi,
        );
        let text = format!("{text:<width$}");
        let mut style = theme::style(style);
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
    let header_style = Style::new().fg(theme::color(app.theme.header)).bold();
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
                n => Style::new().fg(theme::color(app.theme.quoted[(depth - 1) % n])),
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
            paint(&mut styles, m.start(), m.end(), theme::style(*style));
        }
    }
    if !app.pager_search_off
        && let Some(matcher) = &app.pager_search
    {
        for (start, end) in matcher.find_ranges(text) {
            paint(&mut styles, start, end, theme::style(app.theme.search));
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
            status::pager_status(
                &app.session,
                &status::PagerView {
                    view: &pager.view,
                    scroll: pager.scroll,
                    full_headers: pager.full_headers,
                    hide_quoted: pager.hide_quoted,
                },
                height as usize,
                area.width as usize,
            )
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
        Mode::Index => status::index_status(
            &app.session,
            app.index_offset,
            area.width as usize,
            content_height as usize,
        ),
    };
    frame.render_widget(
        Line::from(text).style(theme::style(app.theme.bar_style())),
        area,
    );
}

/// mutt's message line: the prompt you are answering, or the last
/// thing rmut had to say, and empty when it has nothing.
fn draw_message_line(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(prompt) = &app.prompt {
        let mut text = match prompt {
            Prompt::Line { label, edit, .. } => {
                // The marker sits at the cursor, not always at the end.
                let i = rmut_front::editor::byte_at(&edit.buf, edit.cursor);
                format!("{label}{}\u{2581}{}", &edit.buf[..i], &edit.buf[i..])
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
        true => line.style(theme::style(app.theme.error)),
        false => line,
    };
    frame.render_widget(line, area);
}
