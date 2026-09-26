//! The window up before the mailbox is: the session opens on its own
//! thread while the frame shows its progress, and the ready
//! [`Gui`] takes over the moment it lands.
//!
//! Hosted or not. `rmut-egui` owns its window and closes it when rmut
//! quits; a host that puts rmut in a part of its own window
//! ([`Boot::frame_hosted`]) is told instead, and decides.

use anyhow::{Result, bail};
use eframe::egui;

use crate::app::Gui;

/// What the opening thread sends the window while it waits.
pub enum OpenEvent {
    Progress(String),
    Done(Box<Result<(rmut_session::Session, Vec<String>)>>),
}

/// The flags that shape the first screen, kept until the session
/// lands so [`ready_gui`] can finish the boot either way.
#[derive(Default)]
pub struct Plan {
    pub read_only: bool,
    pub commands: Vec<String>,
    pub postponed: bool,
    pub folders: bool,
    pub config_warning: Option<String>,
}

/// The session into its window: warnings said, -e commands run,
/// -p / -y opening what they name.
pub fn ready_gui(session: rmut_session::Session, warnings: Vec<String>, plan: &Plan) -> Gui {
    let mut app = Gui::new(session, warnings, plan.read_only);
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

/// The configuration a window reads: the config file with its folders
/// expanded, what the Preferences dialog saved over `[gui]`, and the
/// window's own say over `[pager] html`, so the checkbox can differ
/// from the terminal's setting. The warning is the config file's.
pub fn load_config() -> (rmut_core::config::Config, Option<String>) {
    let (mut config, warning) = rmut_core::config::load_default();
    config.expand_folders();
    crate::app::load_gui_overlay(&mut config);
    if config.gui.html.is_some() {
        config.pager.html = config.gui.html.clone();
    }
    (config, warning)
}

pub enum Boot {
    Opening {
        spec: String,
        rx: std::sync::mpsc::Receiver<OpenEvent>,
        /// The newest progress line, under the spinner.
        last: String,
        plan: Plan,
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        canvas: (egui::Color32, egui::Color32),
    },
    Ready(Box<Gui>),
    /// The open failed: the error stays on screen until the window
    /// closes, and the exit code says failure.
    Failed {
        error: String,
        canvas: (egui::Color32, egui::Color32),
    },
}

impl Boot {
    /// Start opening `spec` on a thread of its own, waking `ctx` with
    /// each piece of progress. `failed` is set if the open fails, for
    /// an exit code to say so after the window is gone.
    pub fn open(
        spec: String,
        config: rmut_core::config::Config,
        plan: Plan,
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        ctx: &egui::Context,
    ) -> Boot {
        let canvas = boot_canvas(&config);
        let (tx, rx) = std::sync::mpsc::channel();
        let progress_tx = tx.clone();
        let wake = ctx.clone();
        let progress_wake = wake.clone();
        let spec_for_thread = spec.clone();
        std::thread::spawn(move || {
            let progress: rmut_core::remote::Progress = Box::new(move |msg| {
                let _ = progress_tx.send(OpenEvent::Progress(msg.to_string()));
                progress_wake.request_repaint();
            });
            let result = rmut_session::Session::open_spec(&spec_for_thread, config, progress);
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

    /// One frame of a window of rmut's own, closed when rmut is done.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        if self.step(ui, None) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// One frame inside a host's window. The host says which keys are
    /// rmut's - `events` is empty when rmut does not have its focus -
    /// and keeps the window-wide things for itself: zoom, egui's focus,
    /// the visuals. `true` means rmut is done and asks to be closed.
    pub fn frame_hosted(&mut self, ui: &mut egui::Ui, events: &[egui::Event]) -> bool {
        self.step(ui, Some(events))
    }

    /// The session, once there is one.
    pub fn gui(&self) -> Option<&Gui> {
        match self {
            Boot::Ready(gui) => Some(gui),
            _ => None,
        }
    }

    pub fn gui_mut(&mut self) -> Option<&mut Gui> {
        match self {
            Boot::Ready(gui) => Some(gui),
            _ => None,
        }
    }

    /// On the way out: whatever is still inside its `$undo_send`
    /// window goes now, since quitting is not cancelling. The window is
    /// closing, so trouble lands on stderr, as the TUI's exit does.
    pub fn flush(&mut self) {
        if let Boot::Ready(gui) = self {
            for note in gui.session.flush_on_exit() {
                eprintln!("rmut-egui: {note}");
            }
        }
    }

    fn step(&mut self, ui: &mut egui::Ui, hosted: Option<&[egui::Event]>) -> bool {
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
                        let mut gui = ready_gui(session, warnings, &plan);
                        if hosted.is_some() {
                            gui.set_hosted();
                        }
                        *self = Boot::Ready(Box::new(gui));
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
        // A key that closes the boot screen: the window's own, or the
        // ones the host handed over.
        let pressed = |ui: &egui::Ui, keys: &[egui::Key]| {
            match hosted {
            Some(events) => events.iter().any(|event| {
                matches!(event, egui::Event::Key { key, pressed: true, .. } if keys.contains(key))
            }),
            None => ui.input(|i| keys.iter().any(|key| i.key_pressed(*key))),
        }
        };
        match self {
            Boot::Ready(gui) => {
                match hosted {
                    Some(events) => gui.frame_hosted(ui, events),
                    None => gui.frame(ui),
                }
                if gui.quit {
                    self.flush();
                    return true;
                }
                false
            }
            Boot::Opening {
                spec,
                last,
                canvas: (bg, fg),
                ..
            } => {
                let (bg, fg) = (*bg, *fg);
                let close = pressed(ui, &[egui::Key::Escape, egui::Key::Q]);
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
                close
            }
            Boot::Failed {
                error,
                canvas: (bg, _),
            } => {
                let (bg, error) = (*bg, error.clone());
                let close = pressed(ui, &[egui::Key::Escape, egui::Key::Q, egui::Key::Enter]);
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
                close
            }
        }
    }
}

impl eframe::App for Boot {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

/// The boot screen's ground: the `[gui]` canvas pair where it is
/// `#rrggbb`, the built-in dark pair otherwise (named colors wait
/// for the real theme, seconds away).
pub fn boot_canvas(config: &rmut_core::config::Config) -> (egui::Color32, egui::Color32) {
    let pick = |name: &Option<String>, fallback| {
        name.as_deref()
            .and_then(rmut_front::style::parse_color)
            .and_then(|c| match c {
                rmut_front::style::Color::Rgb(r, g, b) => Some(egui::Color32::from_rgb(r, g, b)),
                _ => None,
            })
            .unwrap_or(fallback)
    };
    (
        pick(
            &config.gui.background,
            egui::Color32::from_rgb(0x10, 0x10, 0x10),
        ),
        pick(
            &config.gui.foreground,
            egui::Color32::from_rgb(0xd8, 0xd8, 0xd8),
        ),
    )
}

/// The mailbox to open when none is named: $folder-aware, like the
/// TUI's, but without the $MAIL environment archaeology.
pub fn default_mailbox(config: &rmut_core::config::Config) -> Result<String> {
    if let Some(first) = config.mail.mailboxes.first() {
        return Ok(first.clone());
    }
    if let Some(folder) = config.mail.folder.as_deref() {
        return Ok(folder.to_string());
    }
    bail!("no mailbox configured (set [mail] mailboxes or pass one)")
}
