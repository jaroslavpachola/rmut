//! Starting a draft: the questions between a key and an editor.
//!
//! mutt asks its way into a message: reply to the Reply-To address or
//! the From one, to whom, about what, quote the original or not. Every
//! one of those is an [`Ask`], so the flow lives here rather than in
//! whatever is drawing the prompts, and it ends by handing the front
//! end a draft to open an editor on.

use rmut_core::{alias, compose, message};

use crate::{
    Ask, AskKind, Compose, ComposeBase, ComposeKind, ComposeSetup, Request, Session, Wants,
};

impl Session {
    /// Start a draft: new, a reply, or a forward. The first question
    /// comes back, or none when nothing needs asking.
    pub fn start_compose(&mut self, kind: ComposeKind) -> Option<Ask> {
        let base = match kind {
            ComposeKind::New => None,
            _ => match self.compose_base() {
                Some(b) => Some(b),
                None => {
                    self.error("no message selected");
                    return None;
                }
            },
        };
        self.continue_setup(kind, base)
    }

    /// `L`: reply to the mailing list. Refuses when the message names
    /// no list rmut knows of, rather than quietly replying to the
    /// author, which is the mistake list-reply exists to prevent.
    pub fn start_list_reply(&mut self) -> Option<Ask> {
        let Some(base) = self.compose_base() else {
            self.error("no message selected");
            return None;
        };
        if self.list_target(&base).is_none() {
            self.error(match self.lists.is_empty() {
                true => "no mailing lists configured (mail.lists / mail.subscribed)",
                false => "not a message from a known mailing list",
            });
            return None;
        }
        self.continue_setup(ComposeKind::ListReply, Some(base))
    }

    fn continue_setup(&mut self, kind: ComposeKind, base: Option<ComposeBase>) -> Option<Ask> {
        // mutt's $autoedit (with edit_headers): no prompts, no
        // questions: the defaults land in the draft and the editor
        // opens; everything stays editable there and in the menu.
        if self.config.mail.autoedit && self.edit_headers() {
            let to = match (&kind, &base) {
                (ComposeKind::Reply | ComposeKind::GroupReply, Some(b)) => b.reply_to.clone(),
                (ComposeKind::ListReply, Some(b)) => self.list_target(b).unwrap_or_default(),
                _ => String::new(),
            };
            let subject = match (&kind, &base) {
                (
                    ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply,
                    Some(b),
                ) => compose::reply_subject(&b.subject),
                (ComposeKind::Forward, Some(b)) => {
                    compose::forward_subject(&b.from_addr, &b.subject)
                }
                _ => String::new(),
            };
            self.setup = Some(ComposeSetup {
                kind,
                base,
                to: Some(to),
                subject: None,
                fwd_attach: None,
            });
            self.finish_compose_setup(&subject, true);
            return None;
        }
        let ask_reply_to = matches!(kind, ComposeKind::Reply | ComposeKind::GroupReply)
            && base.as_ref().is_some_and(|b| b.has_reply_to);
        self.setup = Some(ComposeSetup {
            kind,
            base,
            to: None,
            subject: None,
            fwd_attach: None,
        });
        if ask_reply_to {
            // mutt's $reply_to = ask-yes.
            let addr = self
                .setup
                .as_ref()
                .and_then(|s| s.base.as_ref())
                .map(|b| b.reply_to.clone())
                .unwrap_or_default();
            return Some(Ask::Key {
                label: format!("Reply to {addr}? (y/n): "),
                what: AskKind::ReplyTo,
            });
        }
        self.ask_to(true)
    }

    /// Who the draft goes to, unless $fast_reply says the prefill
    /// will do.
    fn ask_to(&mut self, use_reply_to: bool) -> Option<Ask> {
        let setup = self.setup.as_ref()?;
        let to_prefill = match setup.kind {
            ComposeKind::Reply | ComposeKind::GroupReply => setup
                .base
                .as_ref()
                .map(|b| {
                    if use_reply_to {
                        b.reply_to.clone()
                    } else {
                        b.from_hdr.clone()
                    }
                })
                .unwrap_or_default(),
            ComposeKind::ListReply => setup
                .base
                .as_ref()
                .and_then(|b| self.list_target(b))
                .unwrap_or_default(),
            ComposeKind::New | ComposeKind::Forward => String::new(),
        };
        // mutt's $fast_reply: replies take the prefills without the
        // To and Subject prompts (forwards still need a recipient).
        if self.config.mail.fast_reply
            && matches!(
                setup.kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            )
            && setup.base.is_some()
        {
            let subject = setup
                .base
                .as_ref()
                .map(|b| compose::reply_subject(&b.subject))
                .unwrap_or_default();
            if let Some(setup) = &mut self.setup {
                setup.to = Some(to_prefill);
            }
            return self.subject_submitted(&subject);
        }
        Some(Ask::Line {
            label: "To: ".into(),
            prefill: to_prefill,
            wants: Wants::Address,
            what: AskKind::ComposeTo,
        })
    }

    fn setup_to_submitted(&mut self, input: &str) -> Option<Ask> {
        let to = alias::expand(input, &alias::load_default());
        let setup = self.setup.as_mut()?;
        setup.to = Some(to);
        let subject_prefill = match (&setup.kind, &setup.base) {
            (ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply, Some(b)) => {
                compose::reply_subject(&b.subject)
            }
            (ComposeKind::Forward, Some(b)) => compose::forward_subject(&b.from_addr, &b.subject),
            _ => String::new(),
        };
        // $fast_reply also skips the Subject prompt on forwards.
        if self.config.mail.fast_reply && !subject_prefill.is_empty() {
            return self.subject_submitted(&subject_prefill);
        }
        Some(Ask::Line {
            label: "Subject: ".into(),
            prefill: subject_prefill,
            wants: Wants::Other,
            what: AskKind::ComposeSubject,
        })
    }

    /// After the Subject prompt: mutt's $abort_nosubject (ask-yes) on
    /// an empty subject, then on replies mutt's $include (ask-yes).
    fn subject_submitted(&mut self, input: &str) -> Option<Ask> {
        if input.trim().is_empty() {
            return Some(Ask::Key {
                label: "No subject, abort? (y/n): ".into(),
                what: AskKind::NoSubject,
            });
        }
        self.subject_ready(input.to_string())
    }

    fn subject_ready(&mut self, subject: String) -> Option<Ask> {
        let is_reply = self.setup.as_ref().is_some_and(|s| {
            matches!(
                s.kind,
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply
            ) && s.base.is_some()
        });
        let ask_fwd = self.config.mail.forward.as_deref() == Some("ask")
            && self
                .setup
                .as_ref()
                .is_some_and(|s| s.kind == ComposeKind::Forward && s.base.is_some());
        if is_reply {
            if let Some(setup) = &mut self.setup {
                setup.subject = Some(subject);
            }
            return Some(Ask::Key {
                label: "Include message in reply? (y/n): ".into(),
                what: AskKind::IncludeReply,
            });
        }
        if ask_fwd {
            // mime_forward = "ask": whole original vs inline quote.
            if let Some(setup) = &mut self.setup {
                setup.subject = Some(subject);
            }
            return Some(Ask::Key {
                label: "Forward as attachment? (y/n): ".into(),
                what: AskKind::ForwardAttach,
            });
        }
        self.finish_compose_setup(&subject, true);
        None
    }

    fn finish_compose_setup(&mut self, subject: &str, include: bool) {
        let Some(setup) = self.setup.take() else {
            return;
        };
        // mutt's reply-hook: in force while this reply's draft is
        // built, so `set from`, edit_headers and my_hdr all see it.
        let reply_hooks = self.apply_reply_hooks(setup.base.as_ref(), setup.kind);
        self.finish_compose_draft(setup, subject, include);
        self.restore_after_reply_hooks(reply_hooks);
    }

    fn finish_compose_draft(&mut self, setup: ComposeSetup, subject: &str, include: bool) {
        let mut to = setup.to.unwrap_or_default();
        let mut cc = None;
        let mut in_reply_to = None;
        let mut references = None;
        let mut body = String::new();
        let mut attach = None;
        if let Some(b) = &setup.base {
            match setup.kind {
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply => {
                    if include {
                        let orig = message::body_text(&b.path).unwrap_or_default();
                        body =
                            compose::quote(&compose::attribution(&b.from_display, b.date), &orig);
                    }
                    in_reply_to = b.msg_id.clone();
                    let mut refs = b.references.clone();
                    if let Some(id) = &b.msg_id
                        && !refs.contains(id)
                    {
                        refs.push(id.clone());
                    }
                    if !refs.is_empty() {
                        references = Some(refs.join(" "));
                    }
                    if setup.kind == ComposeKind::GroupReply {
                        // mutt honors a sender's Mail-Followup-To: it
                        // is exactly the recipient set they asked for,
                        // so it replaces To and leaves Cc alone.
                        if !b.followup_to.trim().is_empty() {
                            to = b.followup_to.trim().to_string();
                        } else {
                            // Everyone else on the original, minus the
                            // recipient already in To and (mutt's
                            // $metoo off) my own addresses: replying to
                            // all should not mail me a copy.
                            let joined = compose::group_recipients(
                                &b.orig_to,
                                &b.orig_cc,
                                &to,
                                self.me(),
                                self.config.mail.metoo,
                            );
                            if !joined.is_empty() {
                                cc = Some(joined);
                            }
                        }
                    }
                }
                ComposeKind::Forward
                    if setup.fwd_attach.unwrap_or_else(|| self.forward_attaches()) =>
                {
                    // The original goes along whole; nothing to quote.
                    attach = Some(b.path.clone());
                }
                ComposeKind::Forward => {
                    let orig = message::body_text(&b.path).unwrap_or_default();
                    body = compose::forward_body(&b.from_display, b.date, &b.subject, &orig);
                }
                ComposeKind::New => {}
            }
        }
        let from = self.compose_from(setup.base.as_ref(), &to);
        let followup =
            self.followup_header(&to, cc.as_deref(), from.as_deref().unwrap_or_default());
        let text = compose::draft_text(
            &compose::DraftHeaders {
                from,
                to,
                cc,
                subject: subject.to_string(),
                in_reply_to,
                references,
            },
            &body,
        );
        // DraftHeaders has no Mail-Followup-To slot; it goes ahead of
        // the blank line, where edit_headers shows it like any other.
        let text = match followup {
            Some(value) => match text.split_once("\n\n") {
                Some((head, rest)) => format!("{head}\nMail-Followup-To: {value}\n\n{rest}"),
                None => text,
            },
            None => text,
        };
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                let security = self.default_security();
                self.requests.push(Request::Editor(Compose {
                    path,
                    recall_source: None,
                    security,
                    attach,
                    hidden_head,
                    fcc: None,
                }));
            }
            Err(err) => self.error(format!("cannot write draft: {err:#}")),
        }
    }

    /// The Reply-To question is answered: on to the recipient.
    pub(crate) fn answer_reply_to(&mut self, use_reply_to: bool) -> Option<Ask> {
        self.ask_to(use_reply_to)
    }

    pub(crate) fn answer_to(&mut self, input: &str) -> Option<Ask> {
        self.setup_to_submitted(input)
    }

    pub(crate) fn answer_subject(&mut self, input: &str) -> Option<Ask> {
        self.subject_submitted(input)
    }

    /// mutt's $abort_nosubject answered "no, send it anyway".
    pub(crate) fn answer_subject_kept(&mut self) -> Option<Ask> {
        self.subject_ready(String::new())
    }

    pub(crate) fn answer_include(&mut self, include: bool) -> Option<Ask> {
        let subject = self.parked_subject();
        self.finish_compose_setup(&subject, include);
        None
    }

    pub(crate) fn answer_forward_attach(&mut self, attach: bool) -> Option<Ask> {
        let subject = self.parked_subject();
        if let Some(setup) = &mut self.setup {
            setup.fwd_attach = Some(attach);
        }
        self.finish_compose_setup(&subject, true);
        None
    }

    /// The subject parked while a question was up.
    fn parked_subject(&mut self) -> String {
        self.setup
            .as_mut()
            .and_then(|s| s.subject.take())
            .unwrap_or_default()
    }

    /// Give up on the draft that was being set up.
    pub fn cancel_setup(&mut self) {
        self.setup = None;
    }
}
