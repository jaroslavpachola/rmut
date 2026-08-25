mod app;
mod batch;
mod keymap;
mod theme;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};

use crate::app::App;

const USAGE: &str =
    "usage: rmut [-R] [-e CMD]... [-p|-y] [-z|-Z] [-f MAILBOX | MAILBOX | mailto:URL]
       rmut -s SUBJECT [-c CC] [-b BCC] [-a FILE]... [-i FILE] [-e CMD]... -- ADDR...
       rmut --import-muttrc [-w] [MUTTRC]   (-V version, -h help)

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
the config; directives with no rmut equivalent become comments.
-w saves it straight to ~/.config/rmut/config.toml instead, creating
the directory and refusing to overwrite what is already there.";

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
    /// `-w` with --import-muttrc: save it instead of printing it.
    write: bool,
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
            "-w" | "--write" => cli.write = true,
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
        return import_muttrc(cli.spec.as_deref(), cli.write);
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
    app.read_only_session = cli.read_only;
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
    // ratatui::init() panics without one; say it plainly instead.
    if unsafe { libc::isatty(0) } == 0 || unsafe { libc::isatty(1) } == 0 {
        bail!("rmut needs a terminal (use -s ... to send from a script)");
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
fn import_muttrc(path: Option<&str>, write: bool) -> Result<ExitCode> {
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
    let alias_path = rmut_core::alias::default_path();
    if write {
        let target = rmut_core::config::path()
            .ok_or_else(|| anyhow::anyhow!("no $HOME, so no config path to write to"))?;
        let aliases = alias_path.clone().filter(|_| !import.aliases.is_empty());
        // Both targets are checked before either is written, so a
        // half-done import cannot happen.
        for t in [Some(&target), aliases.as_ref()].into_iter().flatten() {
            if t.exists() {
                bail!(
                    "{} already exists: move it aside, or redirect the output instead",
                    t.display()
                );
            }
            if let Some(dir) = t.parent() {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
            }
        }
        std::fs::write(&target, &import.toml)
            .with_context(|| format!("writing {}", target.display()))?;
        eprintln!("rmut: wrote {}", target.display());
        if let Some(aliases) = aliases {
            // mutt's own format, so the lines go across as they are.
            let text = import.aliases.join("\n") + "\n";
            std::fs::write(&aliases, text)
                .with_context(|| format!("writing {}", aliases.display()))?;
            eprintln!(
                "rmut: wrote {} ({} alias(es))",
                aliases.display(),
                import.aliases.len()
            );
        }
    } else {
        print!("{}", import.toml);
        // Printed, not written: the aliases come along as a
        // comment block naming the file they belong in.
        if let Some(target) = &alias_path {
            print!(
                "{}",
                rmut_core::muttrc::alias_block(&import.aliases, target)
            );
        }
        // Printed at a terminal, so nothing was captured: say where it
        // was meant to go rather than leaving it scrolled off.
        if unsafe { libc::isatty(1) } == 1 {
            let target = rmut_core::config::path().unwrap_or_default();
            eprintln!(
                "rmut: nothing written. `rmut --import-muttrc -w` saves it to {}, \
                 or redirect the output yourself.",
                target.display()
            );
        }
    }
    if !write && !import.aliases.is_empty() {
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

/// Where a mailbox may be found when none was given, in the order
/// rmut tries them. Split out from the environment so it can be
/// tested against a directory of its own.
struct MailEnv {
    home: Option<PathBuf>,
    mail: Option<String>,
    user: Option<String>,
}

impl MailEnv {
    fn current() -> MailEnv {
        MailEnv {
            home: std::env::var("HOME").ok().map(PathBuf::from),
            mail: std::env::var("MAIL").ok().filter(|m| !m.is_empty()),
            user: std::env::var("USER").ok().filter(|u| !u.is_empty()),
        }
    }
}

fn default_mailbox(config: &rmut_core::config::Config) -> Result<String> {
    find_default_mailbox(config, &MailEnv::current())
}

/// The mailbox rmut opens with nothing on the command line: the
/// configured ones first, then where mutt looks. A maildir is one
/// with `cur/`; a directory of maildirs answers with its inbox, since
/// that is what "~/Mail" usually is; an mbox is any existing file,
/// which is what $MAIL and /var/mail/$USER classically are.
fn find_default_mailbox(config: &rmut_core::config::Config, env: &MailEnv) -> Result<String> {
    let mut tried: Vec<String> = Vec::new();
    let maildir = |path: PathBuf, tried: &mut Vec<String>| -> Option<String> {
        if path.join("cur").is_dir() {
            return Some(path.display().to_string());
        }
        // A directory of maildirs (mutt's $folder): take its inbox.
        for name in ["inbox", "INBOX", "Inbox"] {
            let inbox = path.join(name);
            if inbox.join("cur").is_dir() {
                return Some(inbox.display().to_string());
            }
        }
        tried.push(path.display().to_string());
        None
    };
    for mailbox in &config.mail.mailboxes {
        if mailbox.starts_with("imap:") {
            return Ok(mailbox.clone());
        }
        if let Some(found) = maildir(expand_tilde(mailbox), &mut tried) {
            return Ok(found);
        }
    }
    if let Some(folder) = &config.mail.folder {
        if folder.starts_with("imap:") {
            return Ok(folder.clone());
        }
        if let Some(found) = maildir(expand_tilde(folder), &mut tried) {
            return Ok(found);
        }
    }
    // $MAIL is classically an mbox file, and rmut reads those too.
    if let Some(mail) = &env.mail {
        let path = expand_tilde(mail);
        if path.is_file() {
            return Ok(mail.clone());
        }
        if let Some(found) = maildir(path, &mut tried) {
            return Ok(found);
        }
    }
    if let Some(home) = &env.home {
        for name in ["Maildir", "Mail", "mail"] {
            if let Some(found) = maildir(home.join(name), &mut tried) {
                return Ok(found);
            }
        }
    }
    if let Some(user) = &env.user {
        for dir in ["/var/mail", "/var/spool/mail"] {
            let spool = PathBuf::from(dir).join(user);
            if spool.is_file() {
                return Ok(spool.display().to_string());
            }
            tried.push(spool.display().to_string());
        }
    }
    bail!(
        "no mailbox found. Give one (rmut ~/Mail/inbox), set [mail] mailboxes \
         in ~/.config/rmut/config.toml, or point $MAIL at one.\n\
         Looked in: {}\n\
         A maildir is a directory with cur/, new/ and tmp/ in it:\n\
         \tmkdir -p ~/Mail/inbox/{{cur,new,tmp}} && rmut ~/Mail/inbox\n{USAGE}",
        tried.join(", ")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maildir(at: &std::path::Path) {
        for sub in ["cur", "new", "tmp"] {
            std::fs::create_dir_all(at.join(sub)).unwrap();
        }
    }

    #[test]
    fn default_mailbox_looks_where_mutt_looks() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let env = |mail: Option<&str>| MailEnv {
            home: Some(home.clone()),
            mail: mail.map(String::from),
            user: Some("nobody".into()),
        };
        let cfg = rmut_core::config::Config::default();

        // Nothing at all: an error naming what was tried.
        let err = find_default_mailbox(&cfg, &env(None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no mailbox found"), "{err}");
        assert!(err.contains("Maildir"), "{err}");

        // ~/Mail is a directory of maildirs (mutt's $folder): its
        // inbox is the answer.
        maildir(&home.join("Mail/inbox"));
        assert_eq!(
            find_default_mailbox(&cfg, &env(None)).unwrap(),
            home.join("Mail/inbox").display().to_string()
        );

        // ~/Maildir, being a maildir itself, comes first.
        maildir(&home.join("Maildir"));
        assert_eq!(
            find_default_mailbox(&cfg, &env(None)).unwrap(),
            home.join("Maildir").display().to_string()
        );

        // $MAIL wins over both, and may be an mbox file.
        let spool = tmp.path().join("spool");
        std::fs::write(&spool, "From x\n").unwrap();
        let spool = spool.display().to_string();
        assert_eq!(
            find_default_mailbox(&cfg, &env(Some(&spool))).unwrap(),
            spool
        );

        // A configured mailbox wins over everything, imap: included.
        let mut cfg = rmut_core::config::Config::default();
        cfg.mail.mailboxes = vec!["imap:work/INBOX".into()];
        assert_eq!(
            find_default_mailbox(&cfg, &env(Some(&spool))).unwrap(),
            "imap:work/INBOX"
        );
    }
}
