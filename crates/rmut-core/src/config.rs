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
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

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
    }
}
