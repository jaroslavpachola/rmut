mod app;
mod keymap;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};

use crate::app::App;

const USAGE: &str = "usage: rmut [MAILDIR]   (-V version, -h help)

Opens MAILDIR, the first configured mailbox, $MAIL, or ~/Maildir.
Config: $RMUT_CONFIG or ~/.config/rmut/config.toml.";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rmut: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let mut dir: Option<PathBuf> = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("rmut {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            _ if arg.starts_with('-') => bail!("unknown option {arg}\n{USAGE}"),
            _ if dir.is_none() => dir = Some(PathBuf::from(arg)),
            _ => bail!("too many arguments\n{USAGE}"),
        }
    }
    let (config, config_warning) = rmut_core::config::load_default();
    let dir = match dir {
        Some(d) => d,
        None => default_maildir(&config)?,
    };

    let mut app = App::open(&dir, config)?;
    if let Some(warning) = config_warning {
        app.status = Some(warning);
    }
    let terminal = ratatui::init();
    let result = app.run(terminal);
    ratatui::restore();
    result.map(|()| ExitCode::SUCCESS)
}

fn expand_tilde(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(input)
}

fn default_maildir(config: &rmut_core::config::Config) -> Result<PathBuf> {
    for mailbox in &config.mail.mailboxes {
        let p = expand_tilde(mailbox);
        if p.join("cur").is_dir() {
            return Ok(p);
        }
    }
    if let Ok(mail) = std::env::var("MAIL") {
        let p = PathBuf::from(mail);
        if p.join("cur").is_dir() {
            return Ok(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join("Maildir");
        if p.join("cur").is_dir() {
            return Ok(p);
        }
    }
    bail!("no maildir given and no configured mailbox, $MAIL, or ~/Maildir found\n{USAGE}");
}
