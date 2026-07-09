//! TOML configuration from $RMUT_CONFIG or ~/.config/rmut/config.toml.
//!
//! ```toml
//! [identity]
//! name = "Jane Doe"
//! email = "jane@example.com"
//!
//! [mail]
//! mailboxes = ["~/Maildir", "~/Maildir/.Sent"]
//! sent = "~/Maildir/.Sent"
//! postponed = "~/Maildir/.Drafts"
//! sendmail = "/usr/sbin/sendmail"
//! editor = "vim"
//! poll_seconds = 5
//!
//! [index]
//! format = "%4C %Z %-6d %-20.20F %5c %s"
//!
//! [ui]
//! theme = "default"   # or "mono"
//!
//! [colors]            # overrides: status_fg status_bg deleted flagged header
//! deleted = "red"
//!
//! [keys.index]        # action = key, e.g. sync = "w", delete = "ctrl+d"
//! [keys.pager]
//!
//! [[accounts]]        # remote account, opened as imap:name/FOLDER
//! name = "work"
//! user = "jane@example.com"
//! password_command = "pass show mail/work"   # or: password = "..."
//! imap_host = "imap.example.com"   # imap_port = 993, imap_tls = true
//! smtp_host = "smtp.example.com"   # smtp_port = 587, smtp_tls = true
//! sent_folder = "Sent"             # Fcc target via IMAP APPEND
//!
//! [pgp]
//! command = "gpg"              # runs via $PATH; passphrases come from
//! sign_key = "jane@example.com"  # the gpg agent, never from rmut
//! sign_by_default = false
//! encrypt_by_default = false
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub identity: Identity,
    pub mail: Mail,
    pub index: Index,
    pub ui: Ui,
    pub colors: HashMap<String, String>,
    pub keys: Keys,
    pub accounts: Vec<Account>,
    pub pgp: Pgp,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Mail {
    pub mailboxes: Vec<String>,
    pub sent: Option<String>,
    pub postponed: Option<String>,
    pub sendmail: Option<String>,
    pub editor: Option<String>,
    pub poll_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Index {
    pub format: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub theme: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub index: HashMap<String, String>,
    pub pager: HashMap<String, String>,
}

/// One remote account: IMAP for reading, SMTP for sending. The
/// password comes from `password_command` (preferred) or, when you
/// accept a secret sitting in the config file, a literal `password`.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub name: String,
    pub user: String,
    /// Shell command whose first stdout line is the password
    /// (pass(1)-style). Wins over `password` when both are set.
    pub password_command: Option<String>,
    /// Plaintext password. Convenient, but anyone who can read the
    /// config can read your mail — keep the file at mode 600.
    pub password: Option<String>,
    pub imap_host: Option<String>,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// Encrypt IMAP (default): TLS from the first byte on port 993,
    /// STARTTLS on any other port. Disabling is for tests only.
    #[serde(default = "default_true")]
    pub imap_tls: bool,
    pub smtp_host: Option<String>,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Encrypt SMTP (default): implicit TLS on port 465, STARTTLS
    /// otherwise. Disabling is for tests only.
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    /// IMAP folder that receives the Fcc copy of sent mail.
    #[serde(default = "default_sent_folder")]
    pub sent_folder: String,
}

/// PGP via gpg(1). Decrypt/verify happens automatically when a viewed
/// message is PGP; signing and encrypting are chosen at the send
/// prompt. Passphrases are gpg-agent's business — rmut never sees them.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Pgp {
    /// The gpg executable (a name looked up in $PATH or a full path).
    pub command: String,
    /// Signing key for --local-user; gpg's default key when unset.
    pub sign_key: Option<String>,
    /// Preselect signing / encrypting for new drafts (the send prompt's
    /// security menu can still change it per message).
    pub sign_by_default: bool,
    pub encrypt_by_default: bool,
}

impl Default for Pgp {
    fn default() -> Self {
        Pgp {
            command: "gpg".into(),
            sign_key: None,
            sign_by_default: false,
            encrypt_by_default: false,
        }
    }
}

fn default_imap_port() -> u16 {
    993
}

fn default_smtp_port() -> u16 {
    587
}

fn default_true() -> bool {
    true
}

fn default_sent_folder() -> String {
    "Sent".into()
}

impl Account {
    /// First stdout line of `password_command`, or the stored
    /// `password` when no command is configured.
    pub fn password(&self) -> Result<String> {
        let Some(command) = &self.password_command else {
            return self
                .password
                .clone()
                .filter(|p| !p.is_empty())
                .with_context(|| {
                    format!(
                        "account {} has neither password_command nor password",
                        self.name
                    )
                });
        };
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .with_context(|| format!("running password command for account {}", self.name))?;
        ensure!(
            out.status.success(),
            "password command for account {} exited with {}",
            self.name,
            out.status
        );
        let pass = String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        ensure!(
            !pass.is_empty(),
            "password command for account {} printed nothing",
            self.name
        );
        Ok(pass)
    }
}

impl Config {
    pub fn account(&self, name: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.name == name)
    }
}

pub fn path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RMUT_CONFIG") {
        return Some(PathBuf::from(p));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/rmut/config.toml"))
}

/// Load the config; a missing file is fine (defaults), a broken file
/// returns defaults plus a warning to show the user.
pub fn load_default() -> (Config, Option<String>) {
    let Some(p) = path() else {
        return (Config::default(), None);
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return (Config::default(), None);
    };
    match toml::from_str(&text) {
        Ok(cfg) => (cfg, None),
        Err(err) => {
            let first = err
                .to_string()
                .lines()
                .next()
                .unwrap_or("parse error")
                .to_string();
            (
                Config::default(),
                Some(format!("config ignored ({}): {first}", p.display())),
            )
        }
    }
}

impl Identity {
    /// "Name <email>" / "email" for the From header, if configured.
    pub fn from_line(&self) -> Option<String> {
        match (&self.name, &self.email) {
            (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
            (None, Some(e)) => Some(e.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let cfg: Config = toml::from_str(
            r#"
            [identity]
            name = "Jane"
            email = "jane@x"
            [mail]
            mailboxes = ["~/Maildir"]
            sendmail = "/bin/true"
            [keys.index]
            sync = "w"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.identity.from_line().as_deref(), Some("Jane <jane@x>"));
        assert_eq!(cfg.mail.mailboxes, vec!["~/Maildir"]);
        assert_eq!(cfg.mail.sendmail.as_deref(), Some("/bin/true"));
        assert_eq!(cfg.keys.index.get("sync").map(String::as_str), Some("w"));
        assert!(cfg.ui.theme.is_none());
    }

    #[test]
    fn empty_and_unknown_keys_are_fine() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.identity.from_line().is_none());
        let cfg: Config = toml::from_str("[future]\nx = 1\n").unwrap();
        assert!(cfg.mail.mailboxes.is_empty());
        assert!(cfg.accounts.is_empty());
    }

    #[test]
    fn parses_accounts_with_defaults() {
        let cfg: Config = toml::from_str(
            r#"
            [[accounts]]
            name = "work"
            user = "jane@example.com"
            password_command = "pass show mail/work"
            imap_host = "imap.example.com"
            smtp_host = "smtp.example.com"

            [[accounts]]
            name = "test"
            user = "u"
            password_command = "true"
            imap_host = "localhost"
            imap_port = 10143
            imap_tls = false
            smtp_port = 465
            sent_folder = "INBOX/Sent"
            "#,
        )
        .unwrap();
        let work = cfg.account("work").unwrap();
        assert_eq!(work.imap_port, 993);
        assert_eq!(work.smtp_port, 587);
        assert!(work.imap_tls && work.smtp_tls);
        assert_eq!(work.sent_folder, "Sent");
        let test = cfg.account("test").unwrap();
        assert_eq!(test.imap_port, 10143);
        assert!(!test.imap_tls);
        assert!(test.smtp_host.is_none());
        assert_eq!(test.sent_folder, "INBOX/Sent");
        assert!(cfg.account("nope").is_none());
    }

    #[test]
    fn pgp_section_defaults_and_overrides() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.pgp.command, "gpg");
        assert!(cfg.pgp.sign_key.is_none());
        assert!(!cfg.pgp.sign_by_default && !cfg.pgp.encrypt_by_default);
        let cfg: Config = toml::from_str(
            "[pgp]\ncommand = \"gpg2\"\nsign_key = \"jane@x\"\nsign_by_default = true\n",
        )
        .unwrap();
        assert_eq!(cfg.pgp.command, "gpg2");
        assert_eq!(cfg.pgp.sign_key.as_deref(), Some("jane@x"));
        assert!(cfg.pgp.sign_by_default && !cfg.pgp.encrypt_by_default);
    }

    #[test]
    fn account_missing_required_field_fails_parse() {
        assert!(toml::from_str::<Config>("[[accounts]]\nname = \"x\"\n").is_err());
    }

    fn test_account() -> Account {
        Account {
            name: "t".into(),
            user: "u".into(),
            password_command: None,
            password: None,
            imap_host: None,
            imap_port: 993,
            imap_tls: true,
            smtp_host: None,
            smtp_port: 587,
            smtp_tls: true,
            sent_folder: "Sent".into(),
        }
    }

    #[test]
    fn password_command_takes_first_line() {
        let account = |cmd: &str| Account {
            password_command: Some(cmd.into()),
            ..test_account()
        };
        assert_eq!(
            account("printf 'secret\\nrest\\n'").password().unwrap(),
            "secret"
        );
        assert!(account("false").password().is_err());
        assert!(account("true").password().is_err()); // empty output
    }

    #[test]
    fn stored_password_and_precedence() {
        let stored = Account {
            password: Some("hunter2".into()),
            ..test_account()
        };
        assert_eq!(stored.password().unwrap(), "hunter2");
        // A configured command wins over the stored password.
        let both = Account {
            password_command: Some("echo from-command".into()),
            password: Some("hunter2".into()),
            ..test_account()
        };
        assert_eq!(both.password().unwrap(), "from-command");
        let neither = test_account();
        assert!(neither.password().is_err());
        let cfg: Config = toml::from_str(
            "[[accounts]]\nname = \"x\"\nuser = \"u\"\npassword = \"pw\"\nimap_host = \"h\"\n",
        )
        .unwrap();
        assert_eq!(cfg.account("x").unwrap().password().unwrap(), "pw");
    }
}
