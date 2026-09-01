//! rmut in a window. The same session, the same keys, the same
//! muttrc; egui instead of a terminal. What needs a terminal (send
//! mode, -z/-Z scripting) stays with `rmut` and is refused here.

mod app;
mod input;
mod paint;
#[cfg(test)]
mod tests;

use anyhow::{Result, bail};

const USAGE: &str = "usage: rmut-egui [-R] [-y] [-z|-Z] [-e CMD]... [-f MAILBOX | mailbox]";

struct Cli {
    spec: Option<String>,
    read_only: bool,
    folders: bool,
    exit_if_empty: bool,
    exit_unless_new: bool,
    commands: Vec<String>,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli {
        spec: None,
        read_only: false,
        folders: false,
        exit_if_empty: false,
        exit_unless_new: false,
        commands: Vec::new(),
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
            "-p" => bail!("-p: not in the GUI yet (use rmut)"),
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

fn run() -> Result<()> {
    let cli = parse_args(std::env::args().skip(1))?;
    let (mut config, config_warning) = rmut_core::config::load_default();
    config.expand_folders();
    let spec = match cli.spec.clone() {
        Some(s) => s,
        None => default_mailbox(&config)?,
    };
    let progress: rmut_core::remote::Progress = Box::new(|msg| eprintln!("rmut-egui: {msg}"));
    let (session, warnings) = rmut_session::Session::open_spec(&spec, config, progress)?;
    let session = session;
    // -z / -Z: report through the exit code without a window.
    if cli.exit_if_empty && session.msgs.is_empty() {
        std::process::exit(1);
    }
    if cli.exit_unless_new && session.new_count() == 0 {
        std::process::exit(1);
    }
    let mut app = app::Gui::new(session, warnings, cli.read_only);
    if let Some(warning) = config_warning {
        app.note(warning);
    }
    for command in &cli.commands {
        app.run_startup_command(command);
    }
    if cli.folders {
        app.open_folder_browser();
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_app_id("rmut-egui"),
        ..Default::default()
    };
    let font = app.session.config.gui.font.clone();
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
                install_font(&cc.egui_ctx, &path);
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
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

/// `[gui] font`: the file's face becomes the monospace family (and
/// the fallback for everything), egui's built-ins behind it. A file
/// that cannot be read is said and skipped, never fatal.
fn install_font(ctx: &eframe::egui::Context, path: &str) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};
    let expanded = rmut_session::expand_tilde(path);
    let bytes = match std::fs::read(&expanded) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!(
                "rmut-egui: cannot read [gui] font {}: {err}",
                expanded.display()
            );
            return;
        }
    };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("gui.font".to_string(), FontData::from_owned(bytes).into());
    for family in [FontFamily::Monospace, FontFamily::Proportional] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "gui.font".to_string());
    }
    ctx.set_fonts(fonts);
}
