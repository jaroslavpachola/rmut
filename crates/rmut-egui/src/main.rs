//! rmut in a window. The same session, the same keys, the same
//! muttrc; egui instead of a terminal. What needs a terminal (send
//! mode, -z/-Z scripting) stays with `rmut` and is refused here.

mod app;
mod input;
mod nvim;
mod paint;
#[cfg(test)]
mod tests;

use anyhow::{Result, bail};

const USAGE: &str = "usage: rmut-egui [-R] [-y] [-p] [-z|-Z] [-e CMD]... [-f MAILBOX | mailbox]";

struct Cli {
    spec: Option<String>,
    read_only: bool,
    folders: bool,
    exit_if_empty: bool,
    exit_unless_new: bool,
    commands: Vec<String>,
    postponed: bool,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli {
        spec: None,
        read_only: false,
        folders: false,
        exit_if_empty: false,
        exit_unless_new: false,
        commands: Vec::new(),
        postponed: false,
    };
    while let Some(arg) = args.next() {
        // A value-taking option, given as -f BOX or -fBOX.
        let mut value = |flag: &str, arg: &str| -> Result<String> {
            match arg.strip_prefix(flag).filter(|rest| !rest.is_empty()) {
                Some(rest) => Ok(rest.to_string()),
                None => args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("{flag} needs an argument\n{USAGE}")),
            }
        };
        match arg.as_str() {
            "-R" | "--read-only" => cli.read_only = true,
            "-y" => cli.folders = true,
            "-z" => cli.exit_if_empty = true,
            "-Z" => cli.exit_unless_new = true,
            "-h" | "--help" => bail!("{USAGE}"),
            "-V" | "--version" => {
                println!("rmut-egui {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-s" | "-a" | "-c" | "-b" | "-i" | "--" => {
                bail!("send mode needs a terminal: use rmut {arg} ...")
            }
            "-p" => cli.postponed = true,
            _ if arg.starts_with("-e") => cli.commands.push(value("-e", &arg)?),
            _ if arg.starts_with("-f") => cli.spec = Some(value("-f", &arg)?),
            other if !other.starts_with('-') => cli.spec = Some(other.to_string()),
            other => bail!("unknown option {other}\n{USAGE}"),
        }
    }
    Ok(cli)
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rmut-egui: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// What the opening thread sends the window while it waits.
pub enum OpenEvent {
    Progress(String),
    Done(Box<Result<(rmut_session::Session, Vec<String>)>>),
}

/// The flags that shape the first screen, kept until the session
/// lands so [`ready_gui`] can finish the boot either way.
pub struct Plan {
    read_only: bool,
    commands: Vec<String>,
    postponed: bool,
    folders: bool,
    config_warning: Option<String>,
}

/// The session into its window: warnings said, -e commands run,
/// -p / -y opening what they name.
fn ready_gui(session: rmut_session::Session, warnings: Vec<String>, plan: &Plan) -> app::Gui {
    let mut app = app::Gui::new(session, warnings, plan.read_only);
    if let Some(warning) = &plan.config_warning {
        app.note(warning.clone());
    }
    for command in &plan.commands {
        app.run_startup_command(command);
    }
    if plan.postponed {
        app.open_postponed();
    } else if plan.folders {
        app.open_folder_browser();
    }
    app
}

/// The window, up before the mailbox is: while the session opens on
/// its own thread the frame shows the progress that used to go to
/// stderr, and the ready [`app::Gui`] takes over the moment it lands.
pub enum Boot {
    Opening {
        spec: String,
        rx: std::sync::mpsc::Receiver<OpenEvent>,
        /// The newest progress line, under the spinner.
        last: String,
        plan: Plan,
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        canvas: (eframe::egui::Color32, eframe::egui::Color32),
    },
    Ready(Box<app::Gui>),
    /// The open failed: the error stays on screen until the window
    /// closes, and the exit code says failure.
    Failed {
        error: String,
        canvas: (eframe::egui::Color32, eframe::egui::Color32),
    },
}

impl Boot {
    pub fn frame(&mut self, ui: &mut eframe::egui::Ui) {
        use eframe::egui;
        // Whatever the opening thread reported since last frame.
        let mut done = None;
        if let Boot::Opening { rx, last, .. } = &mut *self {
            while let Ok(event) = rx.try_recv() {
                match event {
                    OpenEvent::Progress(msg) => *last = msg,
                    OpenEvent::Done(result) => {
                        done = Some(*result);
                        break;
                    }
                }
            }
        }
        if let Some(result) = done {
            let husk = Boot::Failed {
                error: String::new(),
                canvas: (egui::Color32::BLACK, egui::Color32::WHITE),
            };
            if let Boot::Opening {
                plan,
                failed,
                canvas,
                ..
            } = std::mem::replace(self, husk)
            {
                match result {
                    Ok((session, warnings)) => {
                        *self = Boot::Ready(Box::new(ready_gui(session, warnings, &plan)));
                    }
                    Err(err) => {
                        eprintln!("rmut-egui: {err:#}");
                        failed.store(true, std::sync::atomic::Ordering::Relaxed);
                        *self = Boot::Failed {
                            error: format!("{err:#}"),
                            canvas,
                        };
                    }
                }
            }
        }
        match self {
            Boot::Ready(gui) => {
                gui.frame(ui);
                if gui.quit {
                    // Whatever is still inside its $undo_send window
                    // goes out now: quitting is not cancelling. The
                    // window is closing, so trouble lands on stderr,
                    // as the TUI's exit does.
                    for note in gui.session.flush_outbox() {
                        eprintln!("rmut-egui: {note}");
                    }
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Boot::Opening {
                spec,
                last,
                canvas: (bg, fg),
                ..
            } => {
                let (bg, fg) = (*bg, *fg);
                let close =
                    ui.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Q));
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(bg))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.4);
                            ui.spinner();
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(format!("opening {spec}"))
                                    .monospace()
                                    .color(fg),
                            );
                            if !last.is_empty() {
                                ui.label(egui::RichText::new(last.as_str()).monospace().color(fg));
                            }
                        });
                    });
                // Keep the spinner turning between the thread's wakes.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
                if close {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Boot::Failed {
                error,
                canvas: (bg, _),
            } => {
                let (bg, error) = (*bg, error.clone());
                let close = ui.input(|i| {
                    i.key_pressed(egui::Key::Escape)
                        || i.key_pressed(egui::Key::Q)
                        || i.key_pressed(egui::Key::Enter)
                });
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(bg))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.4);
                            ui.label(
                                egui::RichText::new(format!("rmut-egui: {error}"))
                                    .monospace()
                                    .color(egui::Color32::from_rgb(0xff, 0x5c, 0x5c)),
                            );
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("q closes")
                                    .monospace()
                                    .color(egui::Color32::from_rgb(0x7f, 0x7f, 0x7f)),
                            );
                        });
                    });
                if close {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }
}

impl eframe::App for Boot {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

/// The boot screen's ground: the `[gui]` canvas pair where it is
/// `#rrggbb`, the built-in dark pair otherwise (named colors wait
/// for the real theme, seconds away).
fn boot_canvas(
    config: &rmut_core::config::Config,
) -> (eframe::egui::Color32, eframe::egui::Color32) {
    let pick = |name: &Option<String>, fallback| {
        name.as_deref()
            .and_then(rmut_front::style::parse_color)
            .and_then(|c| match c {
                rmut_front::style::Color::Rgb(r, g, b) => {
                    Some(eframe::egui::Color32::from_rgb(r, g, b))
                }
                _ => None,
            })
            .unwrap_or(fallback)
    };
    (
        pick(
            &config.gui.background,
            eframe::egui::Color32::from_rgb(0x10, 0x10, 0x10),
        ),
        pick(
            &config.gui.foreground,
            eframe::egui::Color32::from_rgb(0xd8, 0xd8, 0xd8),
        ),
    )
}

fn run() -> Result<()> {
    let cli = parse_args(std::env::args().skip(1))?;
    let (mut config, config_warning) = rmut_core::config::load_default();
    config.expand_folders();
    // What the Preferences dialog saved, over the [gui] section.
    app::load_gui_overlay(&mut config);
    // The window's own say over [pager] html, so the checkbox can
    // differ from the terminal's setting.
    if config.gui.html.is_some() {
        config.pager.html = config.gui.html.clone();
    }
    let spec = match cli.spec.clone() {
        Some(s) => s,
        None => default_mailbox(&config)?,
    };
    let plan = Plan {
        read_only: cli.read_only,
        commands: cli.commands.clone(),
        postponed: cli.postponed,
        folders: cli.folders,
        config_warning,
    };
    let canvas = boot_canvas(&config);
    let font = config.gui.font.clone();
    // -z / -Z answer through the exit code, window or no window:
    // they keep the blocking open (progress on stderr, as a script
    // expects). Everything else opens the window first and brings
    // the session up behind it.
    let ready: Option<app::Gui> = if cli.exit_if_empty || cli.exit_unless_new {
        let progress: rmut_core::remote::Progress = Box::new(|msg| eprintln!("rmut-egui: {msg}"));
        let (session, warnings) =
            rmut_session::Session::open_spec(&spec, config.clone(), progress)?;
        if cli.exit_if_empty && session.msgs.is_empty() {
            std::process::exit(1);
        }
        if cli.exit_unless_new && session.new_count() == 0 {
            std::process::exit(1);
        }
        Some(ready_gui(session, warnings, &plan))
    } else {
        None
    };
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failed_after = failed.clone();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_app_id("rmut-egui"),
        ..Default::default()
    };
    eframe::run_native(
        "rmut",
        options,
        Box::new(move |cc| {
            // Ctrl+= / Ctrl+- / Ctrl+0: egui's own zoom, made sure of.
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = true);
            // The zoom the last run settled on.
            if let Some(zoom) = app::saved_zoom() {
                cc.egui_ctx.set_zoom_factor(zoom);
            }
            // image/* parts decode through egui's loaders.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            if let Some(path) = font {
                app::install_font(&cc.egui_ctx, &path);
            }
            let boot = match ready {
                Some(gui) => Boot::Ready(Box::new(gui)),
                None => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let progress_tx = tx.clone();
                    let wake = cc.egui_ctx.clone();
                    let progress_wake = wake.clone();
                    let spec_for_thread = spec.clone();
                    std::thread::spawn(move || {
                        let progress: rmut_core::remote::Progress = Box::new(move |msg| {
                            let _ = progress_tx.send(OpenEvent::Progress(msg.to_string()));
                            progress_wake.request_repaint();
                        });
                        let result =
                            rmut_session::Session::open_spec(&spec_for_thread, config, progress);
                        let _ = tx.send(OpenEvent::Done(Box::new(result)));
                        wake.request_repaint();
                    });
                    Boot::Opening {
                        spec,
                        rx,
                        last: String::new(),
                        plan,
                        failed,
                        canvas,
                    }
                }
            };
            Ok(Box::new(boot))
        }),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))?;
    // The open failed inside the window: the error was shown there
    // and printed already; only the exit code is left to say.
    if failed_after.load(std::sync::atomic::Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

/// The mailbox to open when none is named: $folder-aware, like the
/// TUI's, but without the $MAIL environment archaeology.
fn default_mailbox(config: &rmut_core::config::Config) -> Result<String> {
    if let Some(first) = config.mail.mailboxes.first() {
        return Ok(first.clone());
    }
    if let Some(folder) = config.mail.folder.as_deref() {
        return Ok(folder.to_string());
    }
    bail!("no mailbox configured (set [mail] mailboxes or pass one)")
}
