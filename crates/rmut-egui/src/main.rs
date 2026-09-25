//! rmut in a window. The same session, the same keys, the same
//! muttrc; egui instead of a terminal. What needs a terminal (send
//! mode, -z/-Z scripting) stays with `rmut` and is refused here.

use anyhow::{Result, bail};
use rmut_egui::{Boot, Plan, app, boot, rmut_session};

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

fn run() -> Result<()> {
    let cli = parse_args(std::env::args().skip(1))?;
    let (config, config_warning) = boot::load_config();
    let spec = match cli.spec.clone() {
        Some(s) => s,
        None => boot::default_mailbox(&config)?,
    };
    let plan = Plan {
        read_only: cli.read_only,
        commands: cli.commands.clone(),
        postponed: cli.postponed,
        folders: cli.folders,
        config_warning,
    };
    let font = config.gui.font.clone();
    // -z / -Z answer through the exit code, window or no window:
    // they keep the blocking open (progress on stderr, as a script
    // expects). Everything else opens the window first and brings
    // the session up behind it.
    let ready: Option<app::Gui> = if cli.exit_if_empty || cli.exit_unless_new {
        let progress: rmut_egui::rmut_core::remote::Progress =
            Box::new(|msg| eprintln!("rmut-egui: {msg}"));
        let (session, warnings) =
            rmut_session::Session::open_spec(&spec, config.clone(), progress)?;
        if cli.exit_if_empty && session.msgs.is_empty() {
            std::process::exit(1);
        }
        if cli.exit_unless_new && session.new_count() == 0 {
            std::process::exit(1);
        }
        Some(boot::ready_gui(session, warnings, &plan))
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
                None => Boot::open(spec, config, plan, failed, &cc.egui_ctx),
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
