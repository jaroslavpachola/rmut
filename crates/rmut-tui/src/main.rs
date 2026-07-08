mod app;
mod keymap;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};

use crate::app::App;

const USAGE: &str = "usage: rmut [MAILDIR | imap:ACCOUNT[/FOLDER]]   (-V version, -h help)

Opens the given maildir or IMAP folder (INBOX when FOLDER is omitted;
the account comes from [[accounts]] in the config), or falls back to
the first configured mailbox, $MAIL, or ~/Maildir.
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
    let mut spec: Option<String> = None;
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
            _ if spec.is_none() => spec = Some(arg),
            _ => bail!("too many arguments\n{USAGE}"),
        }
    }
    let (config, config_warning) = rmut_core::config::load_default();
    let spec = match spec {
        Some(s) => s,
        None => default_mailbox(&config)?,
    };

    let mut app = App::open_spec(&spec, config)?;
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

fn default_mailbox(config: &rmut_core::config::Config) -> Result<String> {
    for mailbox in &config.mail.mailboxes {
        if mailbox.starts_with("imap:") || expand_tilde(mailbox).join("cur").is_dir() {
            return Ok(mailbox.clone());
        }
    }
    if let Ok(mail) = std::env::var("MAIL")
        && PathBuf::from(&mail).join("cur").is_dir()
    {
        return Ok(mail);
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join("Maildir");
        if p.join("cur").is_dir() {
            return Ok(p.display().to_string());
        }
    }
    bail!("no maildir given and no configured mailbox, $MAIL, or ~/Maildir found\n{USAGE}");
}
