//! The window, painted: mutt's four regions in a monospace grid.
//! Everything shown comes from `rmut-front`; this file only turns
//! its rows and styles into egui text.

use eframe::egui::{self, Color32, FontId, RichText, TextFormat, text::LayoutJob};
use rmut_front::pager::{Menu, PagerStyle, RowKind, pager_rows, recenter};
use rmut_front::status;
use rmut_front::style::{Color, Style};

use crate::app::{Gui, Mode, Prompt};

/// The terminal palette on a dark window, xterm's shades.
fn color32(c: Color, fallback: Color32) -> Color32 {
    match c {
        Color::Reset => fallback,
        Color::Black => Color32::from_rgb(0x00, 0x00, 0x00),
        Color::Red => Color32::from_rgb(0xcd, 0x00, 0x00),
        Color::Green => Color32::from_rgb(0x00, 0xcd, 0x00),
        Color::Yellow => Color32::from_rgb(0xcd, 0xcd, 0x00),
        Color::Blue => Color32::from_rgb(0x4c, 0x6c, 0xff),
        Color::Magenta => Color32::from_rgb(0xcd, 0x00, 0xcd),
        Color::Cyan => Color32::from_rgb(0x00, 0xcd, 0xcd),
        Color::White => Color32::from_rgb(0xe5, 0xe5, 0xe5),
        Color::DarkGray => Color32::from_rgb(0x7f, 0x7f, 0x7f),
        Color::LightRed => Color32::from_rgb(0xff, 0x5c, 0x5c),
        Color::LightGreen => Color32::from_rgb(0x5c, 0xff, 0x5c),
        Color::LightYellow => Color32::from_rgb(0xff, 0xff, 0x5c),
        Color::LightBlue => Color32::from_rgb(0x8c, 0xa8, 0xff),
        Color::LightMagenta => Color32::from_rgb(0xff, 0x5c, 0xff),
        Color::LightCyan => Color32::from_rgb(0x5c, 0xff, 0xff),
    }
}

const FG: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xd8);
const BG: Color32 = Color32::from_rgb(0x10, 0x10, 0x10);

/// A front-end style as egui text: bold is left to the color (egui
/// has no monospace bold face by default), reverse swaps the pair.
fn format(style: Style, size: f32) -> TextFormat {
    let mut fg = color32(style.fg.unwrap_or(Color::Reset), FG);
    let mut bg = style
        .bg
        .map(|c| color32(c, BG))
        .unwrap_or(Color32::TRANSPARENT);
    if style.reversed {
        let solid_bg = if bg == Color32::TRANSPARENT { BG } else { bg };
        (fg, bg) = (solid_bg, fg);
    }
    if style.bold {
        fg = Color32::WHITE.lerp_to_gamma(fg, 0.4);
    }
    TextFormat {
        font_id: FontId::monospace(size),
        color: fg,
        background: bg,
        underline: if style.underline {
            egui::Stroke::new(1.0, fg)
        } else {
            egui::Stroke::NONE
        },
        ..Default::default()
    }
}

fn mono_line(job: &mut LayoutJob, text: &str, style: Style, size: f32) {
    job.append(text, 0.0, format(style, size));
    job.append("\n", 0.0, format(Style::new(), size));
}

pub fn draw(gui: &mut Gui, root: &mut egui::Ui) {
    let size = gui.session.config.gui.size.unwrap_or(14.0).clamp(6.0, 40.0);
    let ctx = root.ctx().clone();
    let char_w = ctx.fonts_mut(|f| f.glyph_width(&FontId::monospace(size), ' '));
    let row_h = ctx.fonts_mut(|f| f.row_height(&FontId::monospace(size))) + 2.0;
    let bar_style = gui.theme.bar_style();

    egui::Panel::top("menubar").show(root, |ui| {
        menu_bar(gui, ui);
    });

    // The help bar (mutt's $help) across the top.
    if gui.session.config.ui.help.unwrap_or(true) {
        egui::Panel::top("help").show(root, |ui| {
            let help = match gui.mode {
                Mode::Index => crate::paint::INDEX_HELP,
                Mode::Pager(_) => PAGER_HELP,
                Mode::Help { .. } => HELP_HELP,
                Mode::Folders { .. } => FOLDERS_HELP,
                Mode::Attach { .. } => ATTACH_HELP,
                Mode::Image { .. } => IMAGE_HELP,
                Mode::Compose { .. } => COMPOSE_HELP,
                Mode::Postponed { .. } => POSTPONED_HELP,
                Mode::Query { .. } => QUERY_HELP,
            };
            let mut job = LayoutJob::default();
            job.append(help, 0.0, format(bar_style, size));
            ui.add(egui::Label::new(job).truncate());
        });
    }

    // The message line at the very bottom, the status bar above it.
    egui::Panel::bottom("message").show(root, |ui| {
        let text = match &gui.prompt {
            Some(Prompt::Line { label, edit, .. }) => {
                let i = rmut_front::editor::byte_at(&edit.buf, edit.cursor);
                format!("{label}{}\u{2581}{}", &edit.buf[..i], &edit.buf[i..])
            }
            Some(Prompt::Key { label, .. }) => label.clone(),
            None => gui
                .notice()
                .map(|n| n.text().to_string())
                .unwrap_or_default(),
        };
        let style = match gui.notice() {
            Some(n) if n.is_error() && gui.prompt.is_none() => gui.theme.error,
            _ => Style::new(),
        };
        let mut job = LayoutJob::default();
        job.append(
            if text.is_empty() { " " } else { &text },
            0.0,
            format(style, size),
        );
        ui.add(egui::Label::new(job).truncate());
    });
    egui::Panel::bottom("status").show(root, |ui| {
        let width = (ui.available_width() / char_w) as usize;
        let rows = gui.view_size.0;
        let text = match &gui.mode {
            Mode::Pager(pager) => status::pager_status(
                &gui.session,
                &status::PagerView {
                    view: &pager.view,
                    scroll: pager.scroll,
                    full_headers: pager.full_headers,
                    hide_quoted: pager.hide_quoted,
                },
                rows,
                width,
            ),
            _ => status::index_status(&gui.session, gui.index_offset, width, rows),
        };
        let mut job = LayoutJob::default();
        job.append(&text, 0.0, format(bar_style, size));
        ui.add(egui::Label::new(job).truncate());
    });

    // The sidebar, a left slice of the index view.
    if matches!(gui.mode, Mode::Index) && gui.sidebar_visible {
        egui::Panel::left("sidebar")
            .resizable(false)
            .show(root, |ui| {
                ui.set_width(char_w * gui.session.config.sidebar.width.clamp(10, 40) as f32);
                ui.spacing_mut().item_spacing.y = 0.0;
                let mut open = None;
                for (i, (spec, count)) in gui.sidebar.iter().enumerate() {
                    let marker = if Some(i) == gui.sidebar_open {
                        ">"
                    } else {
                        " "
                    };
                    let label = status_label(spec);
                    let text = if *count > 0 {
                        format!("{marker}{label} ({count})")
                    } else {
                        format!("{marker}{label}")
                    };
                    let style = if i == gui.sidebar_sel {
                        Style::new().reversed()
                    } else {
                        Style::new()
                    };
                    let mut job = LayoutJob::default();
                    job.append(&text, 0.0, format(style, size));
                    let response =
                        ui.add(egui::Label::new(job).truncate().sense(egui::Sense::click()));
                    hover(ui, &response);
                    if response.clicked() {
                        open = Some((i, spec.clone()));
                    }
                }
                // The stripe opens what it names, the way every mail
                // window's folder list does.
                if let Some((i, spec)) = open {
                    gui.sidebar_sel = i;
                    gui.open_mailbox_spec(&spec);
                }
            });
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(BG))
        .show(root, |ui| {
            let rows = (ui.available_height() / row_h).max(1.0) as usize;
            let width = ((ui.available_width() / char_w) as usize).max(20);
            gui.view_size = (rows, width);
            match &gui.mode {
                Mode::Index => draw_index(gui, ui, rows, width, size),
                Mode::Pager(_) => {
                    // mutt's $pager_index_lines: a slice of the index
                    // stays above the message.
                    let il =
                        (gui.session.config.pager.index_lines as usize).min(rows.saturating_sub(1));
                    if il > 0 {
                        draw_index(gui, ui, il, width, size);
                    }
                    draw_pager(gui, ui, rows - il, width, size);
                }
                Mode::Help { .. } => {
                    let wheel = wheel_rows(gui, ui, size);
                    let Mode::Help { lines, scroll } = &mut gui.mode else {
                        return;
                    };
                    let max = lines.len().saturating_sub(rows);
                    *scroll = (*scroll as i64 + wheel).clamp(0, max as i64) as usize;
                    let mut job = LayoutJob::default();
                    for line in lines.iter().skip(*scroll).take(rows) {
                        mono_line(&mut job, line, Style::new(), size);
                    }
                    ui.add(egui::Label::new(job).extend());
                }
                Mode::Compose { .. } => {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let Mode::Compose { sel } = gui.mode else {
                        return;
                    };
                    let header_style = Style::new().fg(gui.theme.header).bold();
                    let mut job = LayoutJob::default();
                    for (name, value) in gui.compose_header_lines() {
                        job.append(&format!("{name:>9}: "), 0.0, format(header_style, size));
                        job.append(&value, 0.0, format(Style::new(), size));
                        job.append("\n", 0.0, format(Style::new(), size));
                    }
                    mono_line(&mut job, "", Style::new(), size);
                    mono_line(&mut job, "-- Attachments", header_style, size);
                    ui.add(egui::Label::new(job).extend());
                    let entries = gui.compose_entries();
                    let mut clicked = None;
                    for (i, entry) in entries.iter().enumerate() {
                        let style = if i == sel {
                            Style::new().reversed()
                        } else {
                            Style::new()
                        };
                        let mut job = LayoutJob::default();
                        job.append(&format!(" {:>2} {entry}", i + 1), 0.0, format(style, size));
                        let response =
                            ui.add(egui::Label::new(job).extend().sense(egui::Sense::click()));
                        hover(ui, &response);
                        if response.clicked() {
                            clicked = Some((i, response.double_clicked()));
                        }
                    }
                    if let Some((i, open)) = clicked {
                        if let Mode::Compose { sel } = &mut gui.mode {
                            *sel = i;
                        }
                        if open {
                            gui.click("<enter>");
                        }
                    }
                }
                Mode::Postponed { .. } | Mode::Query { .. } => {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let (rows_text, sel) = match &gui.mode {
                        Mode::Postponed { drafts, sel } => {
                            (drafts.iter().map(|d| d.1.clone()).collect::<Vec<_>>(), *sel)
                        }
                        Mode::Query { results, sel } => (results.clone(), *sel),
                        _ => return,
                    };
                    let mut clicked = None;
                    for (i, text) in rows_text.iter().enumerate().take(rows) {
                        let style = if i == sel {
                            Style::new().reversed()
                        } else {
                            Style::new()
                        };
                        let mut job = LayoutJob::default();
                        job.append(text, 0.0, format(style, size));
                        let response =
                            ui.add(egui::Label::new(job).extend().sense(egui::Sense::click()));
                        hover(ui, &response);
                        if response.clicked() {
                            clicked = Some((i, response.double_clicked()));
                        }
                    }
                    if let Some((i, open)) = clicked {
                        match &mut gui.mode {
                            Mode::Postponed { sel, .. } => *sel = i,
                            Mode::Query { sel, .. } => *sel = i,
                            _ => {}
                        }
                        if open {
                            gui.click("<enter>");
                        }
                    }
                }
                Mode::Attach { .. } => {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let Mode::Attach { parts, sel, .. } = &gui.mode else {
                        return;
                    };
                    let sel = *sel;
                    let mut rowinfo = Vec::new();
                    for (i, part) in parts.iter().enumerate().take(rows) {
                        rowinfo.push(format!(
                            "{:>3} [{:<24}] {:>6}  {}",
                            i + 1,
                            part.mimetype,
                            rmut_front::pager::humanize_size(part.size as u64),
                            part.filename.as_deref().unwrap_or("(inline)"),
                        ));
                    }
                    let mut clicked = None;
                    for (i, text) in rowinfo.iter().enumerate() {
                        let style = if i == sel {
                            Style::new().reversed()
                        } else {
                            Style::new()
                        };
                        let mut job = LayoutJob::default();
                        job.append(text, 0.0, format(style, size));
                        let response =
                            ui.add(egui::Label::new(job).extend().sense(egui::Sense::click()));
                        hover(ui, &response);
                        if response.clicked() {
                            clicked = Some((i, response.double_clicked()));
                        }
                    }
                    if let Some((i, open)) = clicked {
                        if let Mode::Attach { sel, .. } = &mut gui.mode {
                            *sel = i;
                        }
                        if open {
                            gui.click("<enter>");
                        }
                    }
                }
                Mode::Image { uri, bytes, .. } => {
                    let image = egui::Image::from_bytes(
                        uri.clone(),
                        egui::load::Bytes::Shared(bytes.clone()),
                    )
                    .max_size(ui.available_size())
                    .shrink_to_fit();
                    ui.centered_and_justified(|ui| {
                        ui.add(image);
                    });
                }
                Mode::Folders { .. } => {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let Mode::Folders { dirs, sel } = &gui.mode else {
                        return;
                    };
                    let (dirs, sel) = (dirs.clone(), *sel);
                    let mut open = None;
                    for (i, (spec, count)) in dirs.iter().enumerate().take(rows) {
                        let style = if i == sel {
                            Style::new().reversed()
                        } else {
                            Style::new()
                        };
                        let mut job = LayoutJob::default();
                        job.append(
                            &format!("{spec:<40} {count:>5} new"),
                            0.0,
                            format(style, size),
                        );
                        let response =
                            ui.add(egui::Label::new(job).extend().sense(egui::Sense::click()));
                        hover(ui, &response);
                        if response.clicked() {
                            open = Some(spec.clone());
                        }
                    }
                    if let Some(spec) = open {
                        gui.open_mailbox_spec(&spec);
                    }
                }
            }
        });
}

fn status_label(spec: &str) -> &str {
    spec.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(spec)
}

fn draw_index(gui: &mut Gui, ui: &mut egui::Ui, rows: usize, width: usize, size: f32) {
    if gui.session.visible.is_empty() {
        let text = if gui.session.limit.is_some() {
            "No messages match the limit (l clears it)."
        } else {
            "No mail in mailbox."
        };
        ui.label(RichText::new(text).monospace().color(FG));
        return;
    }
    let cfg = &gui.session.config.ui;
    // Key motion keeps mutt's recentering; the wheel moves the view
    // without dragging the cursor.
    if gui.keys_this_frame {
        gui.index_offset = recenter(
            gui.index_offset,
            gui.session.sel,
            rows,
            gui.session.visible.len(),
            Menu {
                scroll: cfg.menu_scroll.unwrap_or(true),
                context: cfg.menu_context,
                move_off: cfg.menu_move_off.unwrap_or(true),
            },
        );
    }
    let max_offset = gui.session.visible.len().saturating_sub(rows);
    gui.index_offset =
        (gui.index_offset as i64 + wheel_rows(gui, ui, size)).clamp(0, max_offset as i64) as usize;
    let id_counts = rmut_front::index::id_counts(&gui.session);
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut clicked: Option<(usize, bool)> = None;
    let mut ctx_fire: Option<(usize, &str)> = None;
    for (vi, &mi) in gui
        .session
        .visible
        .iter()
        .enumerate()
        .skip(gui.index_offset)
        .take(rows)
        .collect::<Vec<_>>()
    {
        let (text, mut style) = rmut_front::index::row(
            &gui.session,
            &gui.theme,
            &gui.index_rules,
            &id_counts,
            vi,
            mi,
        );
        if vi == gui.session.sel {
            style = style.reversed();
        }
        let mut fmt = format(style, size);
        // A faint zebra under the unselected rows, so the list reads
        // as rows even where the columns run together.
        if vi % 2 == 1 && fmt.background == Color32::TRANSPARENT {
            fmt.background = Color32::from_gray(0x1a);
        }
        let mut job = LayoutJob::default();
        job.append(&format!("{text:<width$}"), 0.0, fmt);
        let response = ui.add(egui::Label::new(job).extend().sense(egui::Sense::click()));
        hover(ui, &response);
        if response.clicked() || response.secondary_clicked() {
            clicked = Some((vi, response.double_clicked()));
        }
        response.context_menu(|ui| {
            for (label, keys) in CONTEXT_ITEMS {
                if menu_item(ui, label, keys) {
                    ctx_fire = Some((vi, keys));
                }
            }
        });
    }
    if let Some((vi, open)) = clicked {
        gui.click_row(vi);
        if open {
            gui.click("<enter>");
        }
    }
    if let Some((vi, keys)) = ctx_fire {
        gui.click_row(vi);
        gui.click(keys);
    }
}

/// The row's operations, each item firing the keys it stands for.
const CONTEXT_ITEMS: &[(&str, &str)] = &[
    ("Open", "<enter>"),
    ("Reply", "r"),
    ("Reply to all", "g"),
    ("Forward", "f"),
    ("Tag", "t"),
    ("Flag", "F"),
    ("Delete", "d"),
    ("Undelete", "u"),
    ("Save…", "s"),
    ("Collapse thread", "<alt+v>"),
];

/// Wheel motion as whole rows, the remainder kept for next frame.
fn wheel_rows(gui: &mut Gui, ui: &egui::Ui, size: f32) -> i64 {
    let row_h = ui
        .ctx()
        .fonts_mut(|f| f.row_height(&FontId::monospace(size)))
        + 2.0;
    let delta = ui.input(|i| i.smooth_scroll_delta.y);
    gui.scroll_px += delta;
    let rows = (gui.scroll_px / row_h) as i64;
    gui.scroll_px -= rows as f32 * row_h;
    // Scrolling up (positive delta) moves the view up the list.
    -rows
}

/// A button labelled with the key it queues, so the pointer and the
/// keyboard can never disagree.
fn menu_item(ui: &mut egui::Ui, label: &str, keys: &str) -> bool {
    let shortcut = match keys.strip_prefix('<').and_then(|k| k.strip_suffix('>')) {
        Some(name) => rmut_front::parse_key(name)
            .map(|p| p.display())
            .unwrap_or_default(),
        None => keys.to_string(),
    };
    ui.add(egui::Button::new(label).shortcut_text(shortcut))
        .clicked()
}

/// The menu bar: every item fires by queueing its bound keys, so the
/// menus are the keymap made clickable, never a second list of what
/// rmut can do.
fn menu_bar(gui: &mut Gui, ui: &mut egui::Ui) {
    let mut fire: Option<&'static str> = None;
    let item = |target: &mut Option<&'static str>, ui: &mut egui::Ui, label, keys| {
        if menu_item(ui, label, keys) {
            *target = Some(keys);
        }
    };
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button("Mailbox", |ui| {
            item(&mut fire, ui, "Open…", "c");
            item(&mut fire, ui, "Browse folders", "y");
            item(&mut fire, ui, "Limit…", "l");
            ui.separator();
            item(&mut fire, ui, "Quit", "q");
        });
        ui.menu_button("Message", |ui| {
            item(&mut fire, ui, "Open", "<enter>");
            item(&mut fire, ui, "Reply", "r");
            item(&mut fire, ui, "Reply to all", "g");
            item(&mut fire, ui, "Forward", "f");
            ui.separator();
            item(&mut fire, ui, "Tag", "t");
            item(&mut fire, ui, "Flag", "F");
            item(&mut fire, ui, "Delete", "d");
            item(&mut fire, ui, "Undelete", "u");
            item(&mut fire, ui, "Save…", "s");
        });
        ui.menu_button("Thread", |ui| {
            item(&mut fire, ui, "Collapse", "<alt+v>");
            item(&mut fire, ui, "Collapse all", "<alt+V>");
            item(&mut fire, ui, "Next", "<alt+n>");
            item(&mut fire, ui, "Previous", "<alt+p>");
            item(&mut fire, ui, "Parent message", "P");
        });
        ui.menu_button("Sort", |ui| {
            item(&mut fire, ui, "Date", "od");
            item(&mut fire, ui, "From", "of");
            item(&mut fire, ui, "Subject", "os");
            item(&mut fire, ui, "Size", "oz");
            item(&mut fire, ui, "Threads", "ot");
            item(&mut fire, ui, "Label", "oy");
            ui.separator();
            item(&mut fire, ui, "Reverse date", "oD");
        });
        ui.menu_button("Help", |ui| {
            item(&mut fire, ui, "Keys", "?");
            item(&mut fire, ui, "Version", "V");
        });
    });
    if let Some(keys) = fire {
        gui.click(keys);
    }
}

/// A translucent wash over a hovered row: the pointer's own
/// highlight, under the selection's full-strength reverse.
fn hover(ui: &egui::Ui, response: &egui::Response) {
    if response.hovered() {
        ui.painter()
            .rect_filled(response.rect, 0.0, Color32::from_white_alpha(10));
    }
}

fn draw_pager(gui: &mut Gui, ui: &mut egui::Ui, rows: usize, width: usize, size: f32) {
    let wheel = wheel_rows(gui, ui, size);
    let total = gui.pager_line_total(width);
    if let Mode::Pager(pager) = &mut gui.mode {
        let max = total.saturating_sub(rows);
        pager.scroll = (pager.scroll as i64 + wheel).clamp(0, max as i64) as usize;
    }
    let Mode::Pager(pager) = &gui.mode else {
        return;
    };
    let wrap = status::pager_wrap(&gui.session.config, width);
    let all = pager_rows(
        &pager.view,
        wrap,
        pager.full_headers,
        &PagerStyle::of(&gui.session.config, &gui.session.quote_re),
        pager.hide_quoted,
    );
    let header_style = Style::new().fg(gui.theme.header).bold();
    let mut job = LayoutJob::default();
    for row in all.iter().skip(pager.scroll).take(rows) {
        match row.kind {
            RowKind::Header => match row.text.split_once(": ") {
                Some((name, value)) => {
                    job.append(&format!("{name}: "), 0.0, format(header_style, size));
                    job.append(value, 0.0, format(Style::new(), size));
                    job.append("\n", 0.0, format(Style::new(), size));
                }
                None => mono_line(&mut job, &row.text, header_style, size),
            },
            RowKind::Marker => mono_line(&mut job, &row.text, header_style, size),
            RowKind::Quoted(depth) => {
                let style = match gui.theme.quoted.len() {
                    0 => Style::new(),
                    n => Style::new().fg(gui.theme.quoted[(depth - 1) % n]),
                };
                body_line(gui, &mut job, &row.text, style, size);
            }
            RowKind::Text => body_line(gui, &mut job, &row.text, Style::new(), size),
        }
    }
    ui.add(egui::Label::new(job).extend());
}

/// [[color_body]] spans laid over the line, the TUI's `body_line`.
fn body_line(gui: &Gui, job: &mut LayoutJob, text: &str, base: Style, size: f32) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        job.append("\n", 0.0, format(base, size));
        return;
    }
    let mut styles = vec![base; chars.len()];
    let paint = |styles: &mut Vec<Style>, start: usize, end: usize, patch: Style| {
        for (i, (off, _)) in chars.iter().enumerate() {
            if *off >= start && *off < end {
                styles[i] = styles[i].patch(patch);
            }
        }
    };
    for (re, style) in &gui.body_rules {
        for m in re.find_iter(text) {
            paint(&mut styles, m.start(), m.end(), *style);
        }
    }
    if !gui.pager_search_off
        && let Some(matcher) = &gui.pager_search
    {
        for (start, end) in matcher.find_ranges(text) {
            paint(&mut styles, start, end, gui.theme.search);
        }
    }
    let mut cur = String::new();
    let mut cur_style = styles[0];
    for (i, (_, c)) in chars.iter().enumerate() {
        if styles[i] != cur_style {
            job.append(&cur, 0.0, format(cur_style, size));
            cur.clear();
            cur_style = styles[i];
        }
        cur.push(*c);
    }
    cur.push('\n');
    job.append(&cur, 0.0, format(cur_style, size));
}

pub const INDEX_HELP: &str = "q:Quit Enter:View m:New r:Reply f:Fwd t:Tag s:Save o:Sort l:Limit /:Find c:Mbox y:Fldrs ?:Help";
const PAGER_HELP: &str = "q:Back Enter/Bksp:Scroll Space:Page j/k:Next/Prev h:Headers T:Quoted";
const HELP_HELP: &str = "q:Back j/k:Scroll Space/-:Page";
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open";
const ATTACH_HELP: &str = "q:Back j/k:Move Enter:View s:Save |:Pipe p:Print";
const COMPOSE_HELP: &str = "y:Send e:Edit Enter:View t:To c:Cc b:Bcc s:Subj a:Attach n:New D:Detach d:Desc f:Fcc p:PGP P:Postpone q:Quit";
const POSTPONED_HELP: &str = "q:Back j/k:Move Enter:Recall";
const QUERY_HELP: &str = "q:Back j/k:Move Enter:Compose";
const IMAGE_HELP: &str = "q:Back";
