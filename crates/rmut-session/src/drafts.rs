//! Starting a draft: the questions between a key and an editor.
//!
//! mutt asks its way into a message: reply to the Reply-To address or
//! the From one, to whom, about what, quote the original or not. Every
//! one of those is an [`Ask`], so the flow lives here rather than in
//! whatever is drawing the prompts, and it ends by handing the front
//! end a draft to open an editor on.

use std::path::Path;

use anyhow::Result;
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
        self.continue_setup(kind, base, None)
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
        self.continue_setup(ComposeKind::ListReply, Some(base), None)
    }

    /// mutt's forward-message in the attachment menu, for the part at
    /// `index` of the selected message: a part that reads as text is
    /// quoted the way a forward quotes a body, and one that does not
    /// is attached, as mutt's $mime_forward_rest has it. With that
    /// off, such a part is refused rather than sent as an empty
    /// forward. forward = "attach" (mutt's $mime_forward) attaches
    /// the part whatever it is.
    pub fn start_forward_part(&mut self, index: usize) -> Option<Ask> {
        let Some(base) = self.compose_base() else {
            self.error("no message selected");
            return None;
        };
        let part = match message::parts(&base.path) {
            Ok(parts) => parts.into_iter().nth(index)?,
            Err(err) => {
                self.error(format!("cannot list parts: {err:#}"));
                return None;
            }
        };
        if !self.part_reads_as_text(&part)
            && !self.forward_attaches()
            && !self.config.mail.mime_forward_rest.unwrap_or(true)
        {
            self.error(format!(
                "{} does not read as text, and $mime_forward_rest is off",
                part.mimetype
            ));
            return None;
        }
        self.continue_setup(ComposeKind::Forward, Some(base), Some(index))
    }

    /// Whether a forward can quote this part: text, or a type an
    /// auto_view filter turns into text.
    fn part_reads_as_text(&self, part: &message::Part) -> bool {
        part.is_text || self.display.filters.contains_key(&part.mimetype)
    }

    fn continue_setup(
        &mut self,
        kind: ComposeKind,
        base: Option<ComposeBase>,
        part: Option<usize>,
    ) -> Option<Ask> {
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
                ) => self.reply_subject(&b.subject),
                (ComposeKind::Forward, Some(b)) => self.forward_subject(b),
                _ => String::new(),
            };
            self.setup = Some(ComposeSetup {
                kind,
                base,
                to: Some(to),
                cc: None,
                bcc: None,
                subject_prefill: None,
                subject: None,
                fwd_attach: None,
                part,
            });
            return self.finish_compose_setup(&subject, true);
        }
        let ask_reply_to = matches!(kind, ComposeKind::Reply | ComposeKind::GroupReply)
            && base.as_ref().is_some_and(|b| b.has_reply_to);
        self.setup = Some(ComposeSetup {
            kind,
            base,
            to: None,
            cc: None,
            bcc: None,
            subject_prefill: None,
            subject: None,
            fwd_attach: None,
            part,
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
                .map(|b| self.reply_subject(&b.subject))
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
        let to = alias::expand(input, &alias::load(self.config.mail.alias_file.as_deref()));
        self.setup.as_mut()?.to = Some(to);
        let setup = self.setup.as_ref()?;
        let subject_prefill = match (&setup.kind, &setup.base) {
            (ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply, Some(b)) => {
                self.reply_subject(&b.subject)
            }
            (ComposeKind::Forward, Some(b)) => self.forward_subject(b),
            _ => String::new(),
        };
        // What the Subject prompt will offer, parked while the
        // copies are asked about; $fast_reply skips the prompt when
        // it gets there.
        self.setup.as_mut()?.subject_prefill = Some(subject_prefill);
        self.ask_cc_or_on()
    }

    /// mutt's $askcc: the copies, prefilled with whatever a group
    /// reply worked out. Straight on when it is off.
    fn ask_cc_or_on(&mut self) -> Option<Ask> {
        if !self.config.mail.ask_cc {
            return self.ask_bcc_or_on();
        }
        let prefill = self.group_cc().unwrap_or_default();
        Some(Ask::Line {
            label: "Cc: ".into(),
            prefill,
            wants: Wants::Address,
            what: AskKind::ComposeCc,
        })
    }

    /// mutt's $askbcc, which nothing prefills.
    fn ask_bcc_or_on(&mut self) -> Option<Ask> {
        if !self.config.mail.ask_bcc {
            return self.ask_subject();
        }
        Some(Ask::Line {
            label: "Bcc: ".into(),
            prefill: String::new(),
            wants: Wants::Address,
            what: AskKind::ComposeBcc,
        })
    }

    /// The Subject prompt, or straight past it when $fast_reply has
    /// already filled it in.
    fn ask_subject(&mut self) -> Option<Ask> {
        let prefill = self.setup.as_mut()?.subject_prefill.take()?;
        if self.config.mail.fast_reply && !prefill.is_empty() {
            return self.subject_submitted(&prefill);
        }
        Some(Ask::Line {
            label: "Subject: ".into(),
            prefill,
            wants: Wants::Other,
            what: AskKind::ComposeSubject,
        })
    }

    pub(crate) fn answer_cc(&mut self, input: &str) -> Option<Ask> {
        let cc = alias::expand(input, &alias::load(self.config.mail.alias_file.as_deref()));
        self.setup.as_mut()?.cc = Some(cc);
        self.ask_bcc_or_on()
    }

    pub(crate) fn answer_bcc(&mut self, input: &str) -> Option<Ask> {
        let bcc = alias::expand(input, &alias::load(self.config.mail.alias_file.as_deref()));
        self.setup.as_mut()?.bcc = Some(bcc);
        self.ask_subject()
    }

    /// mutt's group reply: everyone else on the original, minus who
    /// is already in To and (unless $metoo) me. None when there is
    /// nobody left, when this is not a group reply, or when the
    /// sender named a Mail-Followup-To, which replaces To and leaves
    /// the copies alone.
    fn group_cc(&self) -> Option<String> {
        let setup = self.setup.as_ref()?;
        if setup.kind != ComposeKind::GroupReply {
            return None;
        }
        let base = setup.base.as_ref()?;
        if !base.followup_to.trim().is_empty() {
            return None;
        }
        let joined = compose::group_recipients(
            &base.orig_to,
            &base.orig_cc,
            setup.to.as_deref().unwrap_or_default(),
            self.me(),
            self.config.mail.metoo,
        );
        (!joined.is_empty()).then_some(joined)
    }

    /// mutt's $indent_string: what each quoted line starts with.
    fn indent_string(&self) -> &str {
        self.config
            .mail
            .indent_string
            .as_deref()
            .unwrap_or(compose::DEFAULT_INDENT)
    }

    /// mutt's $signature: the text this draft ends with, read (or run)
    /// afresh for every draft, so a generated one can say something
    /// new each time.
    fn signature(&self) -> Option<String> {
        compose::signature_text(self.config.mail.signature.as_deref()?)
    }

    /// mutt's $sig_dashes: on unless turned off, as in mutt.
    fn sig_dashes(&self) -> bool {
        self.config.mail.sig_dashes.unwrap_or(true)
    }

    /// After the Subject prompt: mutt's $abort_nosubject on an empty
    /// subject, then on replies mutt's $include (ask-yes).
    fn subject_submitted(&mut self, input: &str) -> Option<Ask> {
        if input.trim().is_empty() {
            // mutt's quadoption: the ask forms differ only in what
            // Enter takes, and the other two answer it themselves.
            match self
                .config
                .mail
                .abort_nosubject
                .as_deref()
                .unwrap_or("ask-yes")
            {
                "no" => return self.subject_ready(String::new()),
                "yes" => {
                    self.cancel_setup();
                    self.error("aborted (no subject)");
                    return None;
                }
                quad => {
                    return Some(Ask::Key {
                        label: "No subject, abort? (y/n): ".into(),
                        what: AskKind::NoSubject {
                            default_yes: quad != "ask-no",
                        },
                    });
                }
            }
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
            // mutt's $include: "yes" and "no" decide it, the two
            // ask forms ask, and Enter takes the one they name.
            match self.config.mail.include.as_deref().unwrap_or("ask-yes") {
                "yes" => return self.finish_compose_setup(&subject, true),
                "no" => return self.finish_compose_setup(&subject, false),
                include => {
                    if let Some(setup) = &mut self.setup {
                        setup.subject = Some(subject);
                    }
                    return Some(Ask::Key {
                        label: "Include message in reply? (y/n): ".into(),
                        what: AskKind::IncludeReply {
                            default_yes: include != "ask-no",
                        },
                    });
                }
            }
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
        self.finish_compose_setup(&subject, true)
    }

    /// The draft is built and on its way to the editor, or, for a
    /// forward, possibly past it ($forward_edit), which may take one
    /// more question.
    fn finish_compose_setup(&mut self, subject: &str, include: bool) -> Option<Ask> {
        let setup = self.setup.take()?;
        // mutt's reply-hook: in force while this reply's draft is
        // built, so `set from`, edit_headers and my_hdr all see it.
        let reply_hooks = self.apply_reply_hooks(setup.base.as_ref(), setup.kind);
        let ask = self.finish_compose_draft(setup, subject, include);
        self.restore_after_reply_hooks(reply_hooks);
        ask
    }

    fn finish_compose_draft(
        &mut self,
        setup: ComposeSetup,
        subject: &str,
        include: bool,
    ) -> Option<Ask> {
        let asked_cc = setup.cc.clone().filter(|cc| !cc.trim().is_empty());
        let bcc = setup.bcc.clone().filter(|bcc| !bcc.trim().is_empty());
        let mut to = setup.to.unwrap_or_default();
        let mut cc = None;
        let mut in_reply_to = None;
        let mut references = None;
        let mut body = String::new();
        let mut attach = None;
        let mut part_line = None;
        if let Some(b) = &setup.base {
            match setup.kind {
                ComposeKind::Reply | ComposeKind::GroupReply | ComposeKind::ListReply => {
                    if include {
                        let orig = message::body_text(&b.path).unwrap_or_default();
                        let quoted = self.quoted_of(b);
                        let attribution = compose::attribution(
                            self.config
                                .mail
                                .attribution
                                .as_deref()
                                .unwrap_or(compose::DEFAULT_ATTRIBUTION),
                            &quoted,
                        );
                        body = compose::quote(&attribution, self.indent_string(), &orig);
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
                ComposeKind::Forward if setup.part.is_some() => {
                    let whole = setup.fwd_attach.unwrap_or_else(|| self.forward_attaches());
                    match self.forward_part(b, setup.part.unwrap_or_default(), whole) {
                        Ok((text, line)) => {
                            body = text;
                            part_line = line;
                        }
                        Err(err) => {
                            self.error(format!("cannot forward the part: {err:#}"));
                            return None;
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
                    // mutt's $forward_quote: the original comes in
                    // quoted, so a reply to the forward reads right.
                    let indent = self.config.mail.forward_quote.then(|| self.indent_string());
                    body =
                        compose::forward_body(&b.from_display, b.date, &b.subject, &orig, indent);
                }
                ComposeKind::New => {}
            }
        }
        // mutt's $signature closes every draft it starts, quoted
        // original or not; $sig_on_top puts it above the quote.
        if let Some(sig) = self.signature() {
            let on_top = self.config.mail.sig_on_top.unwrap_or(false);
            body = compose::with_signature_at(&body, &sig, self.sig_dashes(), on_top);
        }
        // An answered Cc ($askcc) is what the user said, over
        // whatever the group reply worked out.
        let cc = asked_cc.or(cc);
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
        // DraftHeaders has no Mail-Followup-To or Bcc slot; both go
        // ahead of the blank line, where edit_headers shows them like
        // any other header.
        let mut extra: Vec<String> = Vec::new();
        if let Some(value) = followup {
            extra.push(format!("Mail-Followup-To: {value}"));
        }
        if let Some(value) = bcc {
            extra.push(format!("Bcc: {value}"));
        }
        extra.extend(part_line);
        let text = match extra.is_empty() {
            true => text,
            false => match text.split_once("\n\n") {
                Some((head, rest)) => format!("{head}\n{}\n\n{rest}", extra.join("\n")),
                None => text,
            },
        };
        match self.stage_draft(&text) {
            Ok((path, hidden_head)) => {
                // What the editor is being handed, for mutt's
                // $abort_unmodified when it hands it straight back.
                let staged = std::fs::read_to_string(&path).unwrap_or_default();
                self.staged = Some((path.clone(), staged));
                let security = self.security_for(&setup.kind, setup.base.as_ref());
                let draft = Compose {
                    path,
                    recall_source: None,
                    security,
                    attach,
                    hidden_head,
                    fcc: None,
                };
                // mutt's $forward_edit, except that $autoedit with
                // edit_headers always edits, as in mutt.
                let forward_edit = match setup.kind {
                    ComposeKind::Forward if !(self.config.mail.autoedit && self.edit_headers()) => {
                        self.config.mail.forward_edit.as_deref().unwrap_or("yes")
                    }
                    _ => "yes",
                };
                match forward_edit {
                    "no" => self.skip_editor(draft),
                    ask @ ("ask-yes" | "ask-no") => {
                        self.parked_forward = Some(draft);
                        return Some(Ask::Key {
                            label: "Edit forwarded message? (y/n): ".into(),
                            what: AskKind::ForwardEdit {
                                default_yes: ask == "ask-yes",
                            },
                        });
                    }
                    _ => self.requests.push(Request::Editor(draft)),
                }
            }
            Err(err) => self.error(format!("cannot write draft: {err:#}")),
        }
        None
    }

    /// Straight to the compose menu with the draft as it was built,
    /// the way the editor hands one back.
    fn skip_editor(&mut self, draft: Compose) {
        // Nobody edited it, and that is not $abort_unmodified's case.
        self.staged = None;
        self.set_draft(draft);
        self.requests.push(Request::ShowDraft);
    }

    /// $forward_edit asked: the parked forward goes to the editor or
    /// straight to the compose menu. Anything but y or n (or Enter)
    /// cancels the forward.
    pub(crate) fn answer_forward_edit(&mut self, edit: Option<bool>) -> Option<Ask> {
        let draft = self.parked_forward.take()?;
        match edit {
            Some(true) => self.requests.push(Request::Editor(draft)),
            Some(false) => self.skip_editor(draft),
            None => {
                let _ = std::fs::remove_file(&draft.path);
                self.staged = None;
                self.note("forward cancelled");
            }
        }
        None
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
        self.finish_compose_setup(&subject, include)
    }

    pub(crate) fn answer_forward_attach(&mut self, attach: bool) -> Option<Ask> {
        let subject = self.parked_subject();
        if let Some(setup) = &mut self.setup {
            setup.fwd_attach = Some(attach);
        }
        self.finish_compose_setup(&subject, true)
    }

    /// A forwarded part: the body that quotes it, and the Attach line
    /// that carries it when it goes as a file (`whole`, or a part that
    /// does not read as text). The file is the part decoded into the
    /// temp directory, unlinked once the message is sent.
    fn forward_part(
        &self,
        b: &ComposeBase,
        index: usize,
        whole: bool,
    ) -> Result<(String, Option<String>)> {
        let part = message::parts(&b.path)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| anyhow::anyhow!("no part {}", index + 1))?;
        let indent = self.config.mail.forward_quote.then(|| self.indent_string());
        let quote =
            |text: &str| compose::forward_body(&b.from_display, b.date, &b.subject, text, indent);
        if !whole && self.part_reads_as_text(&part) {
            let text = match self.display.filters.get(&part.mimetype) {
                Some(command) => message::filter_part(&b.path, index, command)?,
                None => {
                    let text = message::part_text(&b.path, index)?;
                    match part.mimetype == "text/html" && self.display.html_to_text {
                        true => rmut_core::html::to_text(&text),
                        false => text,
                    }
                }
            };
            return Ok((quote(&text), None));
        }
        let bytes = message::part_bytes(&b.path, index)?;
        let dir = std::env::temp_dir().join(format!("rmut-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        // The part's own name, its basename only, as mutt's sanitizer
        // leaves it; a second forward of the same name gets a prefix
        // on disk and goes out under the name it came with.
        let name = part
            .filename
            .as_deref()
            .and_then(|n| Path::new(n).file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("part-{}", index + 1));
        let mut path = dir.join(&name);
        let mut n = 1;
        while path.exists() {
            n += 1;
            path = dir.join(format!("{n}-{name}"));
        }
        std::fs::write(&path, bytes)?;
        let mut file = compose::Attachment::of(path);
        file.mime = Some(part.mimetype.clone());
        file.name = (n > 1).then_some(name);
        file.unlink = true;
        Ok((quote(""), Some(compose::attach_line(&file))))
    }

    /// The subject parked while a question was up.
    fn parked_subject(&mut self) -> String {
        self.setup
            .as_mut()
            .and_then(|s| s.subject.take())
            .unwrap_or_default()
    }

    /// The message a reply or a forward is about, for the format
    /// strings that describe it.
    fn quoted_of<'a>(&self, base: &'a ComposeBase) -> compose::Quoted<'a> {
        compose::Quoted {
            from: &base.from_hdr,
            subject: &base.subject,
            message_id: base.msg_id.as_deref(),
            date: base.date,
        }
    }

    /// mutt's $forward_format over the message being forwarded.
    fn forward_subject(&self, base: &ComposeBase) -> String {
        compose::forward_subject(
            self.config
                .mail
                .forward_format
                .as_deref()
                .unwrap_or(compose::DEFAULT_FORWARD_FORMAT),
            &self.quoted_of(base),
        )
    }

    /// Give up on the draft that was being set up.
    pub fn cancel_setup(&mut self) {
        self.setup = None;
    }
}
