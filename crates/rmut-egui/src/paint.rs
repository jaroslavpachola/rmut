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
        Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

const FG: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xd8);
const BG: Color32 = Color32::from_rgb(0x10, 0x10, 0x10);

thread_local! {
    /// The frame's canvas pair, set at the top of every draw so the
    /// style mapping reads the configured colors, not the consts.
    static CANVAS: std::cell::Cell<(Color32, Color32)> = const { std::cell::Cell::new((BG, FG)) };
}

/// The window canvas: `[gui] background` / `foreground`, or the
/// built-in dark pair.
fn canvas(gui: &Gui) -> (Color32, Color32) {
    let pick = |name: &Option<String>, fallback| {
        name.as_deref()
            .and_then(rmut_front::style::parse_color)
            .map(|c| color32(c, fallback))
            .unwrap_or(fallback)
    };
    (
        pick(&gui.session.config.gui.background, BG),
        pick(&gui.session.config.gui.foreground, FG),
    )
}

/// A front-end style as egui text: bold is left to the color (egui
/// has no monospace bold face by default), reverse swaps the pair.
fn format(style: Style, size: f32) -> TextFormat {
    format_in(style, size, true)
}

/// The same, choosing the face: the index and headers are always
/// monospace; the body may go proportional ([gui] proportional).
fn format_in(style: Style, size: f32, mono: bool) -> TextFormat {
    let (canvas_bg, canvas_fg) = CANVAS.with(|c| c.get());
    let mut fg = color32(style.fg.unwrap_or(Color::Reset), canvas_fg);
    let mut bg = style
        .bg
        .map(|c| color32(c, canvas_bg))
        .unwrap_or(Color32::TRANSPARENT);
    if style.reversed {
        let solid_bg = if bg == Color32::TRANSPARENT {
            canvas_bg
        } else {
            bg
        };
        (fg, bg) = (solid_bg, fg);
    }
    if style.bold {
        fg = Color32::WHITE.lerp_to_gamma(fg, 0.4);
    }
    TextFormat {
        font_id: if mono {
            FontId::monospace(size)
        } else {
            FontId::proportional(size)
        },
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
    let (canvas_bg, canvas_fg) = canvas(gui);
    CANVAS.with(|c| c.set((canvas_bg, canvas_fg)));
    // The canvas is the whole window's ground: every panel (menu
    // bar, help, status, sidebar) and every modal sits on it, not on
    // egui's stock gray. The widget theme follows its brightness so
    // menus and dialogs stay readable on a light canvas.
    if root.style().visuals.panel_fill != canvas_bg {
        let bright = 0.299 * canvas_bg.r() as f32
            + 0.587 * canvas_bg.g() as f32
            + 0.114 * canvas_bg.b() as f32
            > 140.0;
        let mut visuals = if bright {
            egui::Visuals::light()
        } else {
            egui::Visuals::dark()
        };
        visuals.panel_fill = canvas_bg;
        visuals.window_fill = canvas_bg;
        root.ctx().set_visuals(visuals);
    }
    if gui.prefs.is_some() {
        let mut apply = false;
        let mut save = false;
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("prefs")).show(root.ctx(), |ui| {
            ui.set_width(380.0);
            ui.heading("Preferences");
            ui.add_space(6.0);
            let Some(prefs) = gui.prefs.as_mut() else {
                return;
            };
            egui::Grid::new("prefs-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Text size");
                    ui.add(egui::Slider::new(&mut prefs.size, 8.0..=32.0).suffix(" pt"));
                    ui.end_row();
                    ui.label("Font file");
                    ui.add(
                        egui::TextEdit::singleline(&mut prefs.font)
                            .hint_text("~/.fonts/mono.ttf (empty: built-in)"),
                    );
                    ui.end_row();
                    ui.label("Terminal");
                    ui.add(
                        egui::TextEdit::singleline(&mut prefs.terminal)
                            .hint_text("$TERMINAL, else foot/alacritty/kitty/xterm"),
                    );
                    ui.end_row();
                    ui.label("Background");
                    ui.color_edit_button_srgba(&mut prefs.background);
                    ui.end_row();
                    ui.label("Foreground");
                    ui.color_edit_button_srgba(&mut prefs.foreground);
                    ui.end_row();
                    ui.label("Body");
                    ui.checkbox(&mut prefs.proportional, "proportional face for prose");
                    ui.end_row();
                    ui.label("Editor");
                    egui::ComboBox::from_id_salt("prefs-editor")
                        .selected_text(match prefs.editor {
                            crate::app::EditorMode::External => "$EDITOR in a terminal",
                            crate::app::EditorMode::Builtin => "built-in text box",
                            crate::app::EditorMode::Nvim => "embedded Neovim",
                        })
                        .show_ui(ui, |ui| {
                            for (mode, label) in [
                                (crate::app::EditorMode::External, "$EDITOR in a terminal"),
                                (crate::app::EditorMode::Builtin, "built-in text box"),
                                (crate::app::EditorMode::Nvim, "embedded Neovim"),
                            ] {
                                ui.selectable_value(&mut prefs.editor, mode, label);
                            }
                        });
                    ui.end_row();
                });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    apply = true;
                }
                if ui
                    .button("Save")
                    .on_hover_text("applies, and writes gui.toml next to the config")
                    .clicked()
                {
                    save = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if apply || save {
            gui.apply_prefs(root.ctx());
        }
        if save {
            gui.save_prefs();
        }
        if close || response.should_close() {
            gui.prefs = None;
        }
    }
    if gui.about {
        let response = egui::Modal::new(egui::Id::new("about")).show(root.ctx(), |ui| {
            ui.set_width(360.0);
            ui.vertical_centered(|ui| {
                ui.heading("rmut");
                ui.label(format!("rmut-egui {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(6.0);
                ui.label("mutt's engine in a window: the rmut-session");
                ui.label("mail library under an egui front end.");
                ui.add_space(6.0);
                ui.hyperlink("https://github.com/jaroslavpachola/rmut");
                ui.label("MIT license");
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    gui.about = false;
                }
            });
        });
        if response.should_close() {
            gui.about = false;
        }
    }
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
                Mode::Edit { .. } => EDIT_HELP,
                Mode::NvimEdit => NVIM_HELP,
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
        .frame(egui::Frame::NONE.fill(canvas_bg))
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
                Mode::NvimEdit => {
                    let Some((nvim, _)) = gui.nvim.as_mut() else {
                        return;
                    };
                    nvim.resize(width, rows);
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let to32 = |c: u32| Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8);
                    let (cursor_row, cursor_col) = nvim.grid.cursor;
                    for (y, row) in nvim.grid.cells.iter().enumerate().take(rows) {
                        let mut job = LayoutJob::default();
                        for (x, cell) in row.iter().enumerate() {
                            let attr = nvim.attrs.get(&cell.hl).copied().unwrap_or_default();
                            let mut fg = to32(attr.fg.unwrap_or(nvim.default_fg));
                            let mut bg = to32(attr.bg.unwrap_or(nvim.default_bg));
                            if attr.reverse != (y == cursor_row && x == cursor_col) {
                                std::mem::swap(&mut fg, &mut bg);
                            }
                            if attr.bold {
                                fg = Color32::WHITE.lerp_to_gamma(fg, 0.4);
                            }
                            job.append(
                                &cell.text,
                                0.0,
                                TextFormat {
                                    font_id: FontId::monospace(size),
                                    color: fg,
                                    background: bg,
                                    underline: if attr.underline {
                                        egui::Stroke::new(1.0, fg)
                                    } else {
                                        egui::Stroke::NONE
                                    },
                                    ..Default::default()
                                },
                            );
                        }
                        ui.add(egui::Label::new(job).extend());
                    }
                }
                Mode::Edit { .. } => {
                    // Ctrl+Enter finishes, Esc abandons; consumed
                    // here so the text box never sees them.
                    let done =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter));
                    let abandon =
                        ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                    let mut button_done = false;
                    let mut button_abandon = false;
                    ui.horizontal(|ui| {
                        button_done = ui.button("Done (Ctrl+Enter)").clicked();
                        button_abandon = ui.button("Abandon (Esc)").clicked();
                    });
                    ui.separator();
                    let proportional = gui.session.config.gui.proportional.unwrap_or(false);
                    let Mode::Edit { text, .. } = &mut gui.mode else {
                        return;
                    };
                    let font = if proportional {
                        FontId::proportional(size)
                    } else {
                        FontId::monospace(size)
                    };
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        let edit = egui::TextEdit::multiline(text)
                            .font(font)
                            .desired_width(f32::INFINITY)
                            .desired_rows(rows.saturating_sub(2));
                        let response = ui.add(edit);
                        if ui.memory(|m| m.focused().is_none()) {
                            response.request_focus();
                        }
                    });
                    if done || button_done {
                        gui.finish_edit(true);
                    } else if abandon || button_abandon {
                        gui.finish_edit(false);
                    }
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
        ui.label(
            RichText::new(text)
                .monospace()
                .color(CANVAS.with(|c| c.get().1)),
        );
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
    gui.follow_tail(rows);
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
        // as rows even where the columns run together: the canvas
        // itself, nudged a few percent toward the other pole, so it
        // shades whatever ground the config picked.
        if vi % 2 == 1 && fmt.background == Color32::TRANSPARENT {
            let (bg, _) = CANVAS.with(|c| c.get());
            let bright =
                0.299 * bg.r() as f32 + 0.587 * bg.g() as f32 + 0.114 * bg.b() as f32 > 140.0;
            let pole = if bright {
                Color32::BLACK
            } else {
                Color32::WHITE
            };
            fmt.background = pole.lerp_to_gamma(bg, 0.94);
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
    let mut about = false;
    let mut prefs = false;
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
            item(&mut fire, ui, "Mark all read", "<alt+a>");
            ui.separator();
            // Window chrome, like About: no key to queue.
            if ui.button("Preferences…").clicked() {
                prefs = true;
            }
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
            ui.separator();
            // Window chrome, not a mail function: no key to queue.
            if ui.button("About rmut-egui").clicked() {
                about = true;
            }
        });
    });
    if let Some(keys) = fire {
        gui.click(keys);
    }
    if about {
        gui.about = true;
    }
    if prefs {
        gui.prefs = Some(crate::app::Prefs::from_config(&gui.session.config));
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
    let proportional = gui.session.config.gui.proportional.unwrap_or(false);
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut job = LayoutJob::default();
    let flush = |ui: &mut egui::Ui, job: &mut LayoutJob| {
        if !job.text.is_empty() {
            ui.add(egui::Label::new(std::mem::take(job)).extend());
        }
    };
    for row in all.iter().skip(pager.scroll).take(rows) {
        // The face: prose may go proportional; indented lines read
        // as preformatted and keep the grid.
        let mono = !proportional || row.text.starts_with([' ', '\t']);
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
                body_row(gui, ui, &mut job, flush, &row.text, style, size, mono);
            }
            RowKind::Text => body_row(
                gui,
                ui,
                &mut job,
                flush,
                &row.text,
                Style::new(),
                size,
                mono,
            ),
        }
    }
    flush(ui, &mut job);
}

/// One body row: straight into the running job, unless it carries a
/// URL - then the job flushes and the row lays out as segments with
/// real hyperlinks (eframe opens them in the browser).
#[allow(clippy::too_many_arguments)]
fn body_row(
    gui: &Gui,
    ui: &mut egui::Ui,
    job: &mut LayoutJob,
    flush: impl Fn(&mut egui::Ui, &mut LayoutJob),
    text: &str,
    base: Style,
    size: f32,
    mono: bool,
) {
    if !text.contains("http") {
        body_line(gui, job, text, base, size, mono);
        return;
    }
    let spans = rmut_front::pager::link_spans(text);
    flush(ui, job);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (piece, url) in spans {
            match url {
                Some(url) => {
                    let font = if mono {
                        FontId::monospace(size)
                    } else {
                        FontId::proportional(size)
                    };
                    // xdg-open, spawned by hand: eframe's own opener
                    // needs a webbrowser release crates.io no longer
                    // resolves, and this is one line anyway.
                    if ui.link(RichText::new(piece).font(font)).clicked() {
                        let _ = std::process::Command::new("xdg-open")
                            .arg(&url)
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                }
                None => {
                    let mut seg = LayoutJob::default();
                    // Link rows keep the base coloring; the body
                    // rules and search highlight stay on plain rows.
                    seg.append(&piece, 0.0, format_in(base, size, mono));
                    ui.add(egui::Label::new(seg).extend());
                }
            }
        }
    });
}

/// [[color_body]] spans laid over the line, the TUI's `body_line`.
fn body_line(gui: &Gui, job: &mut LayoutJob, text: &str, base: Style, size: f32, mono: bool) {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.is_empty() {
        job.append("\n", 0.0, format_in(base, size, mono));
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
            job.append(&cur, 0.0, format_in(cur_style, size, mono));
            cur.clear();
            cur_style = styles[i];
        }
        cur.push(*c);
    }
    cur.push('\n');
    job.append(&cur, 0.0, format_in(cur_style, size, mono));
}

pub const INDEX_HELP: &str = "q:Quit Enter:View m:New r:Reply f:Fwd t:Tag s:Save o:Sort l:Limit /:Find c:Mbox y:Fldrs ?:Help";
const PAGER_HELP: &str = "q:Back Enter/Bksp:Scroll Space:Page j/k:Next/Prev h:Headers T:Quoted";
const HELP_HELP: &str = "q:Back j/k:Scroll Space/-:Page";
const FOLDERS_HELP: &str = "q:Back j/k:Move Enter:Open";
const ATTACH_HELP: &str = "q:Back j/k:Move Enter:View m:Mailcap T:Text s:Save |:Pipe p:Print";
const COMPOSE_HELP: &str = "y:Send e:Edit Enter:View t:To c:Cc b:Bcc s:Subj a:Attach n:New D:Detach d:Desc f:Fcc p:PGP P:Postpone q:Quit";
const POSTPONED_HELP: &str = "q:Back j/k:Move Enter:Recall";
const EDIT_HELP: &str = "Ctrl+Enter:Done Esc:Abandon (the draft is kept)";
const NVIM_HELP: &str = "nvim owns the keyboard - :wq finishes, :q! abandons";
const QUERY_HELP: &str = "q:Back j/k:Move Enter:Compose";
const IMAGE_HELP: &str = "q:Back";
