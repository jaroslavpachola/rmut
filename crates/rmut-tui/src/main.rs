mod app;
mod batch;
mod keymap;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};

use crate::app::App;

const USAGE: &str =
    "usage: rmut [-R] [-e CMD]... [-p|-y] [-z|-Z] [-f MAILBOX | MAILBOX | mailto:URL]
       rmut -s SUBJECT [-c CC] [-b BCC] [-a FILE]... [-i FILE] [-e CMD]... -- ADDR...
       rmut --import-muttrc [MUTTRC]   (-V version, -h help)

Reading:
-R  open the mailbox read-only: nothing is written, not even read marks
-f  the mailbox to open (the same as the positional argument)
-p  open the postponed picker
-y  open the mailbox list
-z  exit 1 instead of starting when the mailbox is empty
-Z  exit 1 instead of starting when there is no new mail
-e  run a config command before the first draw, as `:` would; repeatable

Opens the given maildir, mbox, or IMAP folder (INBOX when FOLDER is
omitted; the account comes from [[accounts]] in the config), or falls
back to the first configured mailbox, $MAIL, or ~/Maildir. A mailto:
URL opens a prefilled draft instead, which is what a desktop mail
handler is passed.
Config: $RMUT_CONFIG or ~/.config/rmut/config.toml.

Sending without the TUI (identity, SMTP or sendmail from the config):
-s  subject; giving it (or -c/-b/-a/-i, or a bare --) means send mode
-c  Cc, -b Bcc, -a attach a file (repeatable), -i read the body from a
    file instead of stdin
The body is stdin, the recipients are the remaining arguments, and -e
applies config settings (bind/macro/exec need the TUI and are ignored).
The exit code says whether the message went out.

--import-muttrc translates a muttrc (default ~/.muttrc or
~/.mutt/muttrc) into rmut TOML on stdout, for review and saving as
the config; directives with no rmut equivalent become comments.";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rmut: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Everything the command line can say, before any of it is acted on.
#[derive(Default)]
struct Cli {
    spec: Option<String>,
    read_only: bool,
    import: bool,
    /// `-e`, in the order given.
    commands: Vec<String>,
    subject: Option<String>,
    cc: Option<String>,
    bcc: Option<String>,
    attach: Vec<PathBuf>,
    include: Option<PathBuf>,
    recipients: Vec<String>,
    postponed: bool,
    folders: bool,
    exit_if_empty: bool,
    exit_unless_new: bool,
    /// Set when a positional was a mailto: URL.
    mailto: Option<rmut_core::mailto::Mailto>,
    /// -s/-c/-b/-a/-i, or a bare `--`: send instead of opening a mailbox.
    send_mode: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Cli> {
    let mut cli = Cli::default();
    let mut positional: Vec<String> = Vec::new();
    let mut args = args.peekable();
    let mut only_positional = false;
    while let Some(arg) = args.next() {
        if only_positional {
            positional.push(arg);
            continue;
        }
        // A value-taking option, given as -s SUBJ or -sSUBJ.
        let mut value = |flag: &str| -> Result<String> {
            match arg.strip_prefix(flag).filter(|rest| !rest.is_empty()) {
                Some(rest) => Ok(rest.to_string()),
                None => args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("{flag} needs an argument\n{USAGE}")),
            }
        };
        match arg.as_str() {
            "--" => {
                only_positional = true;
                cli.send_mode = true;
            }
            "-R" | "--read-only" => cli.read_only = true,
            "-p" => cli.postponed = true,
            "-y" => cli.folders = true,
            "-z" => cli.exit_if_empty = true,
            "-Z" => cli.exit_unless_new = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("rmut {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--import-muttrc" => cli.import = true,
            _ if arg.starts_with("-e") => cli.commands.push(value("-e")?),
            _ if arg.starts_with("-f") => cli.spec = Some(value("-f")?),
            _ if arg.starts_with("-s") => {
                cli.subject = Some(value("-s")?);
                cli.send_mode = true;
            }
            _ if arg.starts_with("-c") => {
                cli.cc = Some(value("-c")?);
                cli.send_mode = true;
            }
            _ if arg.starts_with("-b") => {
                cli.bcc = Some(value("-b")?);
                cli.send_mode = true;
            }
            _ if arg.starts_with("-a") => {
                cli.attach.push(expand_tilde(&value("-a")?));
                cli.send_mode = true;
            }
            _ if arg.starts_with("-i") => {
                cli.include = Some(expand_tilde(&value("-i")?));
                cli.send_mode = true;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                bail!("unknown option {arg}\n{USAGE}")
            }
            _ => positional.push(arg),
        }
    }
    // A mailto: URL is a draft to open, not a mailbox and not a
    // recipient list: it is what a desktop handler hands over.
    if let Some(url) = positional.first()
        && let Some(parsed) = rmut_core::mailto::parse(url)
    {
        cli.mailto = Some(parsed);
        return Ok(cli);
    }
    if cli.send_mode {
        cli.recipients = positional;
    } else if let Some(first) = positional.into_iter().next() {
        if cli.spec.is_none() {
            cli.spec = Some(first);
        } else {
            bail!("too many arguments\n{USAGE}");
        }
    }
    Ok(cli)
}

fn run() -> Result<ExitCode> {
    let cli = parse_args(std::env::args().skip(1))?;
    if cli.import {
        return import_muttrc(cli.spec.as_deref());
    }
    let (mut config, config_warning) = rmut_core::config::load_default();
    // +x / =x in any configured mailbox becomes a real name once,
    // here, so nothing downstream has to know about $folder.
    config.expand_folders();
    if cli.send_mode {
        return send_batch(&mut config, &cli);
    }
    let spec = match cli.spec.clone() {
        Some(s) => s,
        None => default_mailbox(&config)?,
    };
    // Weekly at most: drop header caches of vanished maildirs.
    rmut_core::hdrcache::sweep();

    let opened = App::open_spec(&spec, config);
    eprint!("\r\x1b[K"); // clear the leftover progress line
    let mut app = opened?;
    // mutt's -z / -Z: report through the exit code without starting.
    if cli.exit_if_empty && app.msgs.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    if cli.exit_unless_new && app.new_count() == 0 {
        return Ok(ExitCode::FAILURE);
    }
    app.read_only = cli.read_only;
    if let Some(warning) = config_warning {
        app.status = Some(warning);
    }
    // mutt's folder-hook, for the mailbox rmut started on; -e below
    // still has the last word.
    app.run_folder_hooks();
    for command in &cli.commands {
        app.run_startup_command(command);
    }
    if let Some(mailto) = &cli.mailto {
        app.start_mailto(mailto);
    } else if cli.postponed {
        app.open_postponed();
    } else if cli.folders {
        app.open_folders();
    }
    let terminal = ratatui::init();
    app::TUI_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
    let result = app.run(terminal);
    ratatui::restore();
    // Trouble with a message held by $undo_send and sent on the way
    // out: the status line is gone by now, so say it here.
    for note in &app.exit_notes {
        eprintln!("rmut: {note}");
    }
    result.map(|()| ExitCode::SUCCESS)
}

/// `-s`/`-a`/`--`: build the message from the command line and submit
/// it, with no terminal work at all.
fn send_batch(config: &mut rmut_core::config::Config, cli: &Cli) -> Result<ExitCode> {
    // -e still applies, for the config half a send can honor
    // (identity, sendmail, copy); the rest needs a running TUI.
    for line in &cli.commands {
        for cmd in rmut_core::command::parse(line).map_err(|e| anyhow::anyhow!("{e}"))? {
            rmut_core::command::apply(config, &cmd).map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }
    config.expand_folders();
    let out = batch::Outgoing {
        to: cli.recipients.join(", "),
        cc: cli.cc.clone(),
        bcc: cli.bcc.clone(),
        subject: cli.subject.clone().unwrap_or_default(),
        body: batch::read_body(cli.include.as_deref())?,
        attachments: cli.attach.clone(),
    };
    let note = batch::send(config, &out)?;
    eprintln!("rmut: {note}");
    Ok(ExitCode::SUCCESS)
}

/// Translate a muttrc to rmut TOML on stdout (nothing is written).
fn import_muttrc(path: Option<&str>) -> Result<ExitCode> {
    let path = match path {
        Some(p) => expand_tilde(p),
        None => {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
            [home.join(".muttrc"), home.join(".mutt/muttrc")]
                .into_iter()
                .find(|p| p.exists())
                .ok_or_else(|| anyhow::anyhow!("no ~/.muttrc or ~/.mutt/muttrc; pass a path"))?
        }
    };
    let import = rmut_core::muttrc::import_file(&path)?;
    print!("{}", import.toml);
    if !import.aliases.is_empty() {
        eprintln!(
            "rmut: {} alias line(s) found; see the comment block in the output",
            import.aliases.len()
        );
    }
    Ok(ExitCode::SUCCESS)
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
