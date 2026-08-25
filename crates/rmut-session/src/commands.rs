//! Config commands, and the hooks that run them.
//!
//! mutt's `set`, `unset`, `toggle` and `alias` change what the session
//! is; `bind`, `macro`, `push` and `exec` change what keys do, and a
//! session has no keys. So a command line runs here as far as it can
//! and the rest goes back to the front end as a [`Request`], which is
//! also what a hook line does: a `folder-hook` naming a `bind` is
//! perfectly legal, and only the front end can honour it.

use rmut_core::command;
use rmut_core::config::Config;

use crate::{ComposeBase, ComposeKind, Request, Session};

/// What a command line did, for the front end to finish and report.
#[derive(Default)]
pub struct CommandRun {
    /// What the commands had to say (`set beep?` and the like), in
    /// order, for the front end to show as one line.
    pub reports: Vec<String>,
    /// Warnings from recompiling the session's derived state.
    pub warnings: Vec<String>,
}

impl Session {
    /// Run a config command line (mutt's enter-command, and every
    /// hook's payload).
    ///
    /// Everything the session owns is applied here; whatever belongs
    /// to the keys goes back as [`Request::Command`], and the front
    /// end is told the config moved with [`Request::ConfigChanged`].
    pub fn run_command_line(&mut self, line: &str) -> CommandRun {
        let mut run = CommandRun::default();
        let commands = match command::parse(line) {
            Ok(commands) => commands,
            Err(err) => {
                self.error(err);
                return run;
            }
        };
        if commands.is_empty() {
            return run;
        }
        let sort_before = (
            self.config.index.sort.clone(),
            self.config.index.sort_aux.clone(),
        );
        for cmd in commands {
            let outcome = match &cmd {
                command::Command::Bind { .. }
                | command::Command::Macro { .. }
                | command::Command::Push(_)
                | command::Command::Exec(_) => {
                    // The front end's: it has the key tables.
                    self.requests.push(Request::Command(cmd));
                    continue;
                }
                command::Command::Alias { nick, expansion } => self.alias_command(nick, expansion),
                config_command => command::apply(&mut self.config, config_command),
            };
            match outcome {
                Ok(Some(text)) => run.reports.push(text),
                Ok(None) => {}
                Err(err) => {
                    self.error(err);
                    return run;
                }
            }
        }
        // A `:set trash="=Trash"` names a mailbox too.
        self.config.expand_folders();
        run.warnings = self.recompile();
        // A new $sort takes effect where it can be seen.
        if sort_before
            != (
                self.config.index.sort.clone(),
                self.config.index.sort_aux.clone(),
            )
        {
            if let Some(spec) = self.config.index.sort.clone()
                && let Some((sort, rev)) = crate::parse_sort(&spec)
            {
                self.sort = sort;
                self.sort_rev = rev;
            }
            self.apply_sort();
        }
        self.requests.push(Request::ConfigChanged);
        run
    }

    /// mutt's `alias` command: one line appended to the alias file.
    fn alias_command(&mut self, nick: &str, expansion: &str) -> Result<Option<String>, String> {
        if nick.contains(char::is_whitespace) {
            return Err("the alias nick must be one word".into());
        }
        match rmut_core::alias::append(nick, expansion) {
            Ok(_) => Ok(Some(format!("added: alias {nick} {expansion}"))),
            Err(err) => Err(format!("cannot save the alias: {err:#}")),
        }
    }

    /// Run one hook's command line, naming the hook when it fails so
    /// it is clear where a bad line came from.
    fn run_hook(&mut self, what: &str, line: &str) {
        self.clear_notice();
        let run = self.run_command_line(line);
        if let Some(err) = self.notice().filter(|n| n.is_error()).map(|n| n.text()) {
            self.error(format!("{what}: {err}"));
        } else if !run.warnings.is_empty() {
            self.error(format!("{what}: {}", run.warnings.join("; ")));
        }
    }

    /// mutt's folder-hook: the lines matching the mailbox that is now
    /// open.
    pub fn run_folder_hooks(&mut self) {
        if self.config.folder_hooks.is_empty() {
            return;
        }
        let title = self.title.clone();
        let lines: Vec<String> = self
            .config
            .folder_hooks
            .iter()
            .filter(|h| rmut_core::config::glob_match(&h.folder, &title))
            .map(|h| h.command.clone())
            .collect();
        for line in lines {
            self.run_hook("folder-hook", &line);
        }
    }

    /// mutt's message-hook: the lines matching the selected message
    /// are in force while it is selected, and the config goes back to
    /// what it was as soon as the match set changes. Cheap when
    /// nothing matches, so a draw loop can call it every frame.
    pub fn sync_message_hooks(&mut self) {
        if self.message_hooks.is_empty() && self.active_message_hooks.is_empty() {
            return;
        }
        let matching = self.matching_message_hooks();
        if matching == self.active_message_hooks {
            return;
        }
        self.active_message_hooks = matching.clone();
        // Back to the pre-hook config first: a hook that no longer
        // matches must leave no trace.
        self.restore_hook_base();
        if matching.is_empty() {
            return;
        }
        self.hook_base = Some(Box::new(self.config.clone()));
        for i in matching {
            let Some(line) = self.message_hooks.get(i).map(|h| h.value.clone()) else {
                continue;
            };
            self.run_hook("message-hook", &line);
        }
    }

    /// Leaving the message (or the mailbox): whatever the
    /// message-hooks changed goes back.
    pub fn clear_message_hooks(&mut self) {
        self.active_message_hooks.clear();
        self.restore_hook_base();
    }

    fn restore_hook_base(&mut self) {
        let Some(base) = self.hook_base.take() else {
            return;
        };
        self.config = *base;
        let warnings = self.recompile();
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
        self.requests.push(Request::ConfigChanged);
    }

    /// mutt's reply-hook: the lines matching the message being replied
    /// to, in force while this reply's draft is built, so `set from`,
    /// edit_headers and my_hdr all see them. Hands back the config to
    /// put back afterwards.
    pub fn apply_reply_hooks(
        &mut self,
        base: Option<&ComposeBase>,
        kind: ComposeKind,
    ) -> Option<Box<Config>> {
        if self.reply_hooks.is_empty()
            || !matches!(
                kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            )
        {
            return None;
        }
        let lines = self.reply_hook_lines(&base?.path);
        if lines.is_empty() {
            return None;
        }
        let saved = Box::new(self.config.clone());
        for line in lines {
            self.run_hook("reply-hook", &line);
        }
        Some(saved)
    }

    /// Undo [`Session::apply_reply_hooks`].
    pub fn restore_after_reply_hooks(&mut self, saved: Option<Box<Config>>) {
        let Some(saved) = saved else { return };
        self.config = *saved;
        let warnings = self.recompile();
        if !warnings.is_empty() {
            self.error(warnings.join("; "));
        }
        self.requests.push(Request::ConfigChanged);
    }
}
