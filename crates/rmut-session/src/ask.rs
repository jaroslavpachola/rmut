//! What a session needs from whoever is driving it.
//!
//! An operation that cannot finish without an answer stops and says
//! what it needs: a mailbox name, a command, yes or no. The front end
//! collects the answer however it likes (mutt's message line, a
//! dialog, a test calling [`Session::answer`] straight away) and hands
//! it back with the [`AskKind`] it came with. Answering can lead to
//! another question, which is how a two-step operation like bounce
//! (to whom, then really?) works without the session knowing anything
//! about prompts.
//!
//! The same goes the other way for [`Request`]: things only the front
//! end can do, because it owns the terminal, or the window, or in the
//! case of a library caller nothing at all.

use crate::{Msg, Security, Session, SortKey};

/// A question waiting on an answer.
pub enum Ask {
    /// A line of text.
    Line {
        /// What goes in front of the cursor.
        label: String,
        /// What the line starts out holding.
        prefill: String,
        /// What sort of answer it is, for a front end that offers
        /// history or completion.
        wants: Wants,
        what: AskKind,
    },
    /// One keystroke: a yes/no, or a small menu.
    Key { label: String, what: AskKind },
}

/// What a line answer is, so a front end can help the user give one.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Wants {
    Mailbox,
    Pattern,
    Address,
    Command,
    Other,
}

/// The keystroke answering a [`Ask::Key`]. `Other` is anything the
/// question does not recognise, which usually calls it off.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Other,
}

/// An answer on its way back in.
pub enum Answer<'a> {
    Line(&'a str),
    Key(Key),
}

/// Which question is being answered, and everything the operation was
/// holding when it stopped to ask. The front end carries it back
/// untouched, so nothing about a half-done operation lives in the
/// front end.
#[derive(Clone)]
pub enum AskKind {
    /// mutt's limit: show only the messages matching a pattern.
    Limit,
    /// An index search; `back` is mutt's search-reverse.
    Search {
        back: bool,
    },
    /// A mark applied to every message matching a pattern.
    Pattern {
        op: PatternOp,
    },
    /// Where to copy the messages, and whether the originals are
    /// marked deleted afterwards (mutt's save).
    CopyTo {
        delete: bool,
        tagged: bool,
    },
    Pipe {
        tagged: bool,
    },
    PrintConfirm {
        tagged: bool,
    },
    BounceTo {
        tagged: bool,
    },
    BounceConfirm {
        to: String,
        tagged: bool,
    },
    /// Purge the deleted messages? `quit` leaves afterwards.
    Purge {
        quit: bool,
    },
    /// mutt's sort menu: one key picks the order.
    Sort,
    /// The nick for a create-alias; the address came from the message.
    AliasNick {
        addr: String,
    },
    /// mutt's $reply_to (ask-yes): reply to the Reply-To address?
    ReplyTo,
    /// Who the draft goes to.
    ComposeTo,
    /// mutt's $askcc / $askbcc: who else gets a copy.
    ComposeCc,
    ComposeBcc,
    /// What it is about.
    ComposeSubject,
    /// mutt's $abort_nosubject (ask-yes): no subject, abort?
    NoSubject,
    /// mutt's $include: quote the original in the reply? `default_yes`
    /// is what Enter takes, from ask-yes or ask-no.
    IncludeReply {
        default_yes: bool,
    },
    /// mime_forward = "ask": forward the original as an attachment?
    ForwardAttach,
    /// $abort_noattach = ask: the body mentions an attachment and
    /// none is attached. Send it anyway?
    NoAttach,
    /// A header of the draft in hand, edited from the compose menu.
    EditHeader {
        name: String,
    },
    /// Where the sent copy goes (empty keeps none).
    EditFcc,
    /// A file to attach.
    AttachFile,
    /// The description or the content-type of the k-th attachment.
    AttachField {
        k: usize,
        is_type: bool,
    },
    /// mutt's compose menu `p`: sign, encrypt, both, or neither.
    Security,
    /// Leaving the compose menu: postpone the draft, or throw it away?
    PostponeAsk,
    /// mutt's `!`: a shell command to run with the display stood down.
    Shell,
}

/// The pattern operations, mutt's D/U/T/Ctrl+T.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PatternOp {
    Delete,
    Undelete,
    Tag,
    Untag,
}

impl PatternOp {
    /// The word the undo step and the note use.
    fn verb(self) -> &'static str {
        match self {
            PatternOp::Delete => "deleted",
            PatternOp::Undelete => "undeleted",
            PatternOp::Tag => "tagged",
            PatternOp::Untag => "untagged",
        }
    }

    /// The word the question uses.
    fn label(self) -> &'static str {
        match self {
            PatternOp::Delete => "Delete",
            PatternOp::Undelete => "Undelete",
            PatternOp::Tag => "Tag",
            PatternOp::Untag => "Untag",
        }
    }

    fn apply(self, m: &mut Msg) {
        match self {
            PatternOp::Delete => {
                if !m.env.file.flags.deleted {
                    m.env.file.flags.deleted = true;
                    m.dirty = true;
                }
            }
            PatternOp::Undelete => {
                if m.env.file.flags.deleted {
                    m.env.file.flags.deleted = false;
                    m.dirty = true;
                }
            }
            PatternOp::Tag => m.env.tagged = true,
            PatternOp::Untag => m.env.tagged = false,
        }
    }
}

/// Something only the front end can do, handed back for it to honour
/// or refuse.
pub enum Request {
    /// Nothing is pending; the session is done being driven.
    Quit,
    /// The config moved: whatever the front end derives from it (the
    /// colours, the key tables) wants rebuilding.
    ConfigChanged,
    /// A command line the session does not handle, because it binds a
    /// key, queues one, or runs a function.
    Command(rmut_core::command::Command),
    /// The mailbox counts moved: a front end showing them (a
    /// sidebar) wants to redraw.
    MailboxesChanged,
    /// A message is ready to read: show it however messages are
    /// shown (mutt puts it in the pager).
    ShowMessage(Box<rmut_core::message::MessageView>),
    /// The draft is back in the front end's hands: put it on screen
    /// again, however drafts are shown.
    ShowDraft,
    /// Run a shell command with the display stood down, mutt's `!`.
    Shell(String),
    /// Stop, and pick the display back up when the job resumes.
    Suspend,
    /// Open an editor on this draft, and put it back on screen
    /// afterwards. Only the front end knows how to stand its display
    /// down for one.
    Editor(crate::Compose),
}

impl Session {
    /// mutt's limit prompt.
    pub fn ask_limit(&self) -> Ask {
        Ask::Line {
            label: "Limit (~f/~s/~b/~t/~c/~d/flags, ! | (), empty=all): ".into(),
            prefill: self
                .limit
                .as_ref()
                .map(|(raw, _)| raw.clone())
                .unwrap_or_default(),
            wants: Wants::Pattern,
            what: AskKind::Limit,
        }
    }

    pub fn ask_search(&self, back: bool) -> Ask {
        Ask::Line {
            label: match back {
                true => "Reverse search: ".into(),
                false => "Search: ".into(),
            },
            prefill: String::new(),
            wants: Wants::Pattern,
            what: AskKind::Search { back },
        }
    }

    /// A mark applied to everything matching a pattern. None when it
    /// would have to write a read-only mailbox.
    pub fn ask_pattern(&mut self, op: PatternOp) -> Option<Ask> {
        if matches!(op, PatternOp::Delete | PatternOp::Undelete) && self.deny_readonly() {
            return None;
        }
        Some(Ask::Line {
            label: format!("{} messages matching: ", op.label()),
            prefill: String::new(),
            wants: Wants::Pattern,
            what: AskKind::Pattern { op },
        })
    }

    /// Where to save or copy. None when there is nothing to copy, or
    /// when saving would have to write a read-only mailbox.
    pub fn ask_copy(&mut self, delete: bool, tagged: bool) -> Option<Ask> {
        self.visible.get(self.sel)?;
        // Save marks the original deleted; a plain copy is fine.
        if delete && self.deny_readonly() {
            return None;
        }
        Some(Ask::Line {
            label: match delete {
                true => "Save to mailbox: ".into(),
                false => "Copy to mailbox: ".into(),
            },
            prefill: self.config.mail.save.clone().unwrap_or_default(),
            wants: Wants::Mailbox,
            what: AskKind::CopyTo { delete, tagged },
        })
    }

    pub fn ask_pipe(&self, tagged: bool) -> Option<Ask> {
        self.visible.get(self.sel)?;
        Some(Ask::Line {
            label: "Pipe to command: ".into(),
            prefill: String::new(),
            wants: Wants::Command,
            what: AskKind::Pipe { tagged },
        })
    }

    pub fn ask_bounce(&self, tagged: bool) -> Option<Ask> {
        self.visible.get(self.sel)?;
        Some(Ask::Line {
            label: "Bounce message to: ".into(),
            prefill: String::new(),
            wants: Wants::Address,
            what: AskKind::BounceTo { tagged },
        })
    }

    pub fn ask_print(&self, tagged: bool) -> Option<Ask> {
        self.visible.get(self.sel)?;
        let n = self.op_targets(tagged).len();
        Some(Ask::Key {
            label: match n {
                1 => "Print message? (y/n): ".to_string(),
                _ => format!("Print {n} messages? (y/n): "),
            },
            what: AskKind::PrintConfirm { tagged },
        })
    }

    /// mutt's create-alias, on the sender of the selected message.
    pub fn ask_alias(&mut self) -> Option<Ask> {
        let &i = self.visible.get(self.sel)?;
        let path = self.msgs[i].env.file.path.clone();
        let Some(from) = rmut_core::message::first_header(&path, "From") else {
            self.error("the message has no From header");
            return None;
        };
        let nick = rmut_core::compose::bare_address(&from)
            .and_then(|a| a.split('@').next().map(|l| l.to_lowercase()))
            .unwrap_or_default();
        Some(Ask::Line {
            label: "Alias as (nick): ".into(),
            prefill: nick,
            wants: Wants::Other,
            what: AskKind::AliasNick {
                addr: from.trim().to_string(),
            },
        })
    }

    pub fn ask_sort(&self) -> Ask {
        Ask::Key {
            label: "Sort: (d)ate (f)rom (s)ubject si(z)e (t)hreads, uppercase reverses: ".into(),
            what: AskKind::Sort,
        }
    }

    /// Purge the deleted messages before leaving this mailbox?
    ///
    /// mutt's $delete decides it without asking when it is set to yes
    /// or no; then there is nothing to ask and the work is already
    /// done, so this returns None.
    pub fn ask_purge(&mut self, quit: bool) -> Option<Ask> {
        match self.config.mail.delete.as_deref() {
            Some("yes") | Some("no") => {
                let purge = self.config.mail.delete.as_deref() == Some("yes");
                self.sync(purge);
                if quit {
                    self.requests.push(Request::Quit);
                }
                None
            }
            _ => Some(Ask::Key {
                label: format!("Purge {} deleted message(s)? (y/n): ", self.deleted_count()),
                what: AskKind::Purge { quit },
            }),
        }
    }

    /// A header of the draft in hand (To, Cc, Bcc, Subject).
    pub fn ask_header(&self, name: &str) -> Option<Ask> {
        self.draft()?;
        Some(Ask::Line {
            label: format!("{name}: "),
            prefill: self.draft_header(name),
            wants: match name {
                "Subject" => Wants::Other,
                _ => Wants::Address,
            },
            what: AskKind::EditHeader {
                name: name.to_string(),
            },
        })
    }

    /// Where the sent copy goes, prefilled with where it would go.
    pub fn ask_fcc(&self) -> Option<Ask> {
        let draft = self.draft()?;
        Some(Ask::Line {
            label: "Fcc: ".into(),
            prefill: match draft.fcc.clone() {
                Some(fcc) => fcc,
                None => self.default_fcc(Some(draft)),
            },
            wants: Wants::Mailbox,
            what: AskKind::EditFcc,
        })
    }

    pub fn ask_attach_file(&self) -> Option<Ask> {
        self.draft()?;
        Some(Ask::Line {
            label: "Attach file: ".into(),
            prefill: String::new(),
            wants: Wants::Other,
            what: AskKind::AttachFile,
        })
    }

    /// The description (or content-type) of the attachment the menu's
    /// `sel`-th row stands for.
    pub fn ask_attach_field(&mut self, sel: usize, is_type: bool) -> Option<Ask> {
        let draft = self.draft()?;
        let fixed = 1 + usize::from(draft.attach.is_some());
        if sel < fixed {
            self.error("only Attach: files can be edited");
            return None;
        }
        let k = sel - fixed;
        let attachment = crate::draft_full(draft)
            .ok()
            .map(|full| rmut_core::compose::extract_attachments(&full).1)
            .and_then(|mut atts| (k < atts.len()).then(|| atts.swap_remove(k)))?;
        let prefill = match is_type {
            true => attachment
                .mime
                .clone()
                .unwrap_or_else(|| rmut_core::compose::content_type(&attachment.path).to_string()),
            false => attachment.description.clone().unwrap_or_default(),
        };
        Some(Ask::Line {
            label: match is_type {
                true => "Content-Type: ".into(),
                false => "Description: ".into(),
            },
            prefill,
            wants: Wants::Other,
            what: AskKind::AttachField { k, is_type },
        })
    }

    pub fn ask_security(&self) -> Option<Ask> {
        self.draft()?;
        Some(Ask::Key {
            label: "Security: (e)ncrypt (s)ign (b)oth (c)lear: ".into(),
            what: AskKind::Security,
        })
    }

    pub fn ask_postpone(&self) -> Option<Ask> {
        self.draft()?;
        Some(Ask::Key {
            label: "Postpone this message? (y/n): ".into(),
            what: AskKind::PostponeAsk,
        })
    }

    /// mutt's `!`: run something with the display out of the way.
    pub fn ask_shell(&self) -> Ask {
        Ask::Line {
            label: "Shell command: ".into(),
            prefill: String::new(),
            wants: Wants::Command,
            what: AskKind::Shell,
        }
    }

    /// Ctrl+Z: ask to be put in the background. A front end that
    /// cannot be suspended simply does not honour it.
    pub fn request_suspend(&mut self) {
        self.requests.push(Request::Suspend);
    }

    /// Hand back an answer. The next question, when there is one.
    pub fn answer(&mut self, what: AskKind, answer: Answer<'_>) -> Option<Ask> {
        match (what, answer) {
            (AskKind::Limit, Answer::Line(input)) => {
                self.set_limit(input);
                None
            }
            (AskKind::Search { back }, Answer::Line(input)) => {
                self.search_rev = back;
                if !input.is_empty() {
                    match rmut_core::pattern::parse(input) {
                        Ok(patterns) => {
                            self.resolve_body_terms(&patterns);
                            self.last_search = Some(patterns);
                        }
                        Err(err) => {
                            self.error(format!("bad pattern: {err}"));
                            return None;
                        }
                    }
                }
                self.search_next();
                None
            }
            (AskKind::Pattern { op }, Answer::Line(input)) => {
                self.apply_pattern(input, op.verb(), |m| op.apply(m));
                None
            }
            (AskKind::CopyTo { delete, tagged }, Answer::Line(input)) => {
                let input = self.expand_folder(input);
                self.copy_message(&input, delete, tagged);
                None
            }
            (AskKind::Pipe { tagged }, Answer::Line(command)) => {
                self.pipe_message(command, tagged);
                None
            }
            (AskKind::AliasNick { addr }, Answer::Line(nick)) => {
                self.create_alias(nick, &addr);
                None
            }
            // ---- the compose flow: one question leads to the next
            (AskKind::ReplyTo, Answer::Key(key)) => match key {
                Key::Char('y') | Key::Enter => self.answer_reply_to(true),
                Key::Char('n') => self.answer_reply_to(false),
                _ => {
                    self.cancel_setup();
                    self.note("reply cancelled");
                    None
                }
            },
            (AskKind::EditHeader { name }, Answer::Line(input)) => {
                let value = match name.as_str() {
                    "Subject" => input.to_string(),
                    _ => rmut_core::alias::expand(input, &rmut_core::alias::load_default()),
                };
                self.set_draft_header(&name, &value);
                None
            }
            (AskKind::Shell, Answer::Line(command)) => {
                if !command.is_empty() {
                    self.requests.push(Request::Shell(command.to_string()));
                }
                None
            }
            (AskKind::EditFcc, Answer::Line(input)) => {
                if let Some(draft) = self.draft_mut() {
                    draft.fcc = Some(input.trim().to_string());
                }
                None
            }
            (AskKind::AttachFile, Answer::Line(input)) => {
                self.attach_file(input);
                None
            }
            (AskKind::AttachField { k, is_type }, Answer::Line(input)) => {
                self.set_attach_field(k, input, is_type);
                None
            }
            (AskKind::Security, Answer::Key(key)) => {
                if let Some(draft) = self.draft_mut() {
                    draft.security = match key {
                        Key::Char('e') => Security::Encrypt,
                        Key::Char('s') => Security::Sign,
                        Key::Char('b') => Security::Both,
                        Key::Char('c') => Security::None,
                        _ => draft.security,
                    };
                }
                None
            }
            (AskKind::PostponeAsk, Answer::Key(key)) => {
                match key {
                    Key::Char('y') | Key::Enter => {
                        if let Some(draft) = self.take_draft() {
                            self.postpone_draft(draft);
                        }
                    }
                    Key::Char('n') => {
                        if let Some(draft) = self.take_draft() {
                            let _ = std::fs::remove_file(&draft.path);
                            self.note("message discarded");
                        }
                    }
                    // Anything else goes back to the menu.
                    _ => self.requests.push(Request::ShowDraft),
                }
                None
            }
            (AskKind::NoAttach, Answer::Key(key)) => match key {
                // ask-no: Enter goes back to the menu, where `a`
                // attaches the file that was forgotten.
                Key::Char('y') => {
                    self.confirm_attachment();
                    self.send_draft()
                }
                _ => {
                    self.note("not sent; a attaches a file");
                    self.requests.push(Request::ShowDraft);
                    None
                }
            },
            (AskKind::ComposeTo, Answer::Line(input)) => self.answer_to(input),
            (AskKind::ComposeCc, Answer::Line(input)) => self.answer_cc(input),
            (AskKind::ComposeBcc, Answer::Line(input)) => self.answer_bcc(input),
            (AskKind::ComposeSubject, Answer::Line(input)) => self.answer_subject(input),
            (AskKind::NoSubject, Answer::Key(key)) => match key {
                // ask-yes: Enter aborts, like mutt.
                Key::Char('n') => self.answer_subject_kept(),
                _ => {
                    self.cancel_setup();
                    self.error("aborted (no subject)");
                    None
                }
            },
            (AskKind::IncludeReply { default_yes }, Answer::Key(key)) => match key {
                Key::Char('n') => self.answer_include(false),
                Key::Char('y') => self.answer_include(true),
                Key::Enter => self.answer_include(default_yes),
                _ => {
                    self.cancel_setup();
                    self.note("reply cancelled");
                    None
                }
            },
            (AskKind::ForwardAttach, Answer::Key(key)) => match key {
                // ask-yes: Enter takes the attachment.
                Key::Char('n') => self.answer_forward_attach(false),
                Key::Char('y') | Key::Enter => self.answer_forward_attach(true),
                _ => {
                    self.cancel_setup();
                    self.note("forward cancelled");
                    None
                }
            },
            (AskKind::BounceTo { tagged }, Answer::Line(input)) => {
                let to = rmut_core::alias::expand(input, &rmut_core::alias::load_default());
                if to.trim().is_empty() {
                    self.note("no recipients, bounce cancelled");
                    return None;
                }
                let n = self.op_targets(tagged).len();
                Some(Ask::Key {
                    label: match n {
                        1 => format!("Bounce message to {to}? (y/n): "),
                        _ => format!("Bounce {n} messages to {to}? (y/n): "),
                    },
                    what: AskKind::BounceConfirm { to, tagged },
                })
            }
            (AskKind::BounceConfirm { to, tagged }, Answer::Key(key)) => {
                if key == Key::Char('y') {
                    self.bounce_current(&to, tagged);
                }
                None
            }
            (AskKind::PrintConfirm { tagged }, Answer::Key(key)) => {
                if key == Key::Char('y') {
                    self.print_current(tagged);
                }
                None
            }
            (AskKind::Purge { quit }, Answer::Key(key)) => {
                // ask-yes, like mutt's $delete: Enter takes the yes.
                // n writes flag changes but keeps the messages marked
                // deleted; anything else calls the whole thing off,
                // including the quit that asked.
                if matches!(key, Key::Char('y') | Key::Char('n') | Key::Enter) {
                    self.sync(key != Key::Char('n'));
                    if quit {
                        self.requests.push(Request::Quit);
                    }
                }
                None
            }
            (AskKind::Sort, Answer::Key(key)) => {
                let (sort, rev) = match key {
                    Key::Char('d') => (SortKey::Date, false),
                    Key::Char('D') => (SortKey::Date, true),
                    Key::Char('f') => (SortKey::From, false),
                    Key::Char('F') => (SortKey::From, true),
                    Key::Char('s') => (SortKey::Subject, false),
                    Key::Char('S') => (SortKey::Subject, true),
                    Key::Char('z') => (SortKey::Size, false),
                    Key::Char('Z') => (SortKey::Size, true),
                    Key::Char('t') | Key::Char('T') => (SortKey::Threads, false),
                    _ => return None,
                };
                self.sort = sort;
                self.sort_rev = rev;
                self.apply_sort();
                self.note(format!(
                    "sorted by {}{}",
                    sort.name(),
                    if rev { " (reverse)" } else { "" }
                ));
                None
            }
            // A key answer to a line question, or the other way round:
            // nothing to do with it.
            _ => None,
        }
    }

    /// Anything the session wants the front end to do, oldest first.
    pub fn take_request(&mut self) -> Option<Request> {
        match self.requests.is_empty() {
            true => None,
            false => Some(self.requests.remove(0)),
        }
    }

    /// mutt's `+x` / `=x`: a mailbox under $folder.
    fn expand_folder(&self, input: &str) -> String {
        rmut_core::config::expand_folder(input, self.config.mail.folder.as_deref())
    }

    /// The limit pattern, or none of it when the answer is empty or
    /// mutt's "all".
    fn set_limit(&mut self, input: &str) {
        let keep = self.selected_path();
        if input.is_empty() || input == "all" {
            self.limit = None;
        } else {
            match rmut_core::pattern::parse(input) {
                Ok(patterns) => {
                    self.resolve_body_terms(&patterns);
                    self.limit = Some((input.to_string(), patterns));
                }
                Err(err) => {
                    self.error(format!("bad pattern: {err}"));
                    return;
                }
            }
        }
        self.rebuild_visible(keep);
        if self.visible.is_empty() {
            self.note("no messages match the limit");
        }
    }
}
