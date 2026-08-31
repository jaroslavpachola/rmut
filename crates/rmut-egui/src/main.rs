//! rmut in a window. The same session, the same keys, the same
//! muttrc; egui instead of a terminal. What needs a terminal (send
//! mode, -z/-Z scripting) stays with `rmut` and is refused here.

mod app;
mod input;
mod paint;
#[cfg(test)]
mod tests;

use anyhow::{Result, bail};

const USAGE: &str = "usage: rmut-egui [-R] [-y] [mailbox]";

struct Cli {
    spec: Option<String>,
    read_only: bool,
    folders: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli {
        spec: None,
        read_only: false,
        folders: false,
    };
    for arg in args {
        match arg.as_str() {
            "-R" | "--read-only" => cli.read_only = true,
            "-y" => cli.folders = true,
            "-h" | "--help" => bail!("{USAGE}"),
            "-s" | "-a" | "-c" | "-b" | "-i" | "--" => {
                bail!("send mode needs a terminal: use rmut {arg} ...")
            }
            "-z" | "-Z" | "-p" | "-e" | "-f" => bail!("{arg}: not in the GUI yet (use rmut)"),
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
    let mut app = app::Gui::new(session, warnings, cli.read_only);
    if let Some(warning) = config_warning {
        app.note(warning);
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
    eframe::run_native("rmut", options, Box::new(|_cc| Ok(Box::new(app))))
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
