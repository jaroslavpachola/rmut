//! Logic tests, off the pty.
//!
//! These used to be scenarios: a terminal, a screen scrape, and an
//! assertion on the words that came back. What they were really about
//! is what an operation does to a mailbox, which is a value now, so
//! they ask for it directly. The pty suite keeps what only it can
//! test: the keys, the drawing, and the paths that go all the way out
//! through sendmail, IMAP and gpg.

use std::fs;
use std::path::{Path, PathBuf};

use rmut_core::config::Config;
use rmut_core::notice::{Log, Notice};

use crate::{Answer, Ask, AskKind, Key, PatternOp, Session, SortKey, subject_key, wrap_order};

/// A mailbox on disk and a session over it, with everything the
/// session says recorded.
struct Fixture {
    /// Kept for its Drop: the mailbox lives as long as the fixture.
    _dir: tempfile::TempDir,
    session: Session,
    log: Log,
}

impl Fixture {
    /// A mailbox holding one message per subject, oldest first, in
    /// `cur` (read once, like mail that has been seen at least by the
    /// delivery), plus whatever the config says.
    fn with_config(subjects: &[&str], config: Config) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["cur", "new", "tmp"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        for (i, subject) in subjects.iter().enumerate() {
            write_message(dir.path(), i, subject, None);
        }
        let (mut session, warnings) = Session::open(dir.path(), config).unwrap();
        assert_eq!(warnings, Vec::<String>::new(), "clean open");
        let log = Log::default();
        session.install_notices(Box::new(log.clone()));
        Fixture {
            _dir: dir,
            session,
            log,
        }
    }

    fn new(subjects: &[&str]) -> Fixture {
        Fixture::with_config(subjects, Config::default())
    }

    /// The subjects as the index has them, in order.
    fn subjects(&self) -> Vec<String> {
        self.session
            .visible
            .iter()
            .map(|&i| self.session.msgs[i].env.subject.clone())
            .collect()
    }

    /// Put the cursor on the message with this subject.
    fn select(&mut self, subject: &str) {
        let at = self
            .subjects()
            .iter()
            .position(|s| s == subject)
            .unwrap_or_else(|| panic!("no message {subject:?} on screen"));
        self.session.select(at);
    }

    fn tag(&mut self, subject: &str) {
        let path = self.path_of(subject);
        for m in &mut self.session.msgs {
            if m.env.file.path == path {
                m.env.tagged = true;
            }
        }
    }

    fn path_of(&self, subject: &str) -> PathBuf {
        self.session
            .msgs
            .iter()
            .find(|m| m.env.subject == subject)
            .unwrap_or_else(|| panic!("no message {subject:?}"))
            .env
            .file
            .path
            .clone()
    }

    fn is_deleted(&self, subject: &str) -> bool {
        self.session
            .msgs
            .iter()
            .find(|m| m.env.subject == subject)
            .is_some_and(|m| m.env.file.flags.deleted)
    }

    fn is_tagged(&self, subject: &str) -> bool {
        self.session
            .msgs
            .iter()
            .find(|m| m.env.subject == subject)
            .is_some_and(|m| m.env.tagged)
    }

    /// Answer the question the session just asked, and hand back the
    /// next one.
    fn answer_line(&mut self, ask: Option<Ask>, input: &str) -> Option<Ask> {
        let what = ask_kind(ask.expect("a question was asked"));
        self.session.answer(what, Answer::Line(input))
    }

    fn answer_key(&mut self, ask: Option<Ask>, key: char) -> Option<Ask> {
        let what = ask_kind(ask.expect("a question was asked"));
        self.session.answer(what, Answer::Key(Key::Char(key)))
    }
}

fn ask_kind(ask: Ask) -> AskKind {
    match ask {
        Ask::Line { what, .. } | Ask::Key { what, .. } => what,
    }
}

fn ask_label(ask: &Ask) -> &str {
    match ask {
        Ask::Line { label, .. } | Ask::Key { label, .. } => label,
    }
}

/// One message in `dir/cur`, dated by its index so the order is the
/// order they were written in.
fn write_message(dir: &Path, i: usize, subject: &str, extra: Option<&str>) {
    let day = 10 + i;
    let text = format!(
        "From: Sender {i} <s{i}@example.com>\n\
         To: me@example.com\n\
         Subject: {subject}\n\
         Date: Mon, {day} Mar 2024 10:00:00 +0000\n\
         Message-ID: <m{i}@example.com>\n\
         {}\n\
         body of {subject}\n",
        extra.unwrap_or("")
    );
    fs::write(dir.join("cur").join(format!("{i:04}.rmut:2,")), text).unwrap();
}

#[test]
fn a_mailbox_opens_on_its_messages() {
    let f = Fixture::new(&["one", "two", "three"]);
    assert_eq!(f.subjects(), ["one", "two", "three"]);
    assert_eq!(f.session.new_count(), 0, "in cur, not new");
    assert_eq!(f.session.deleted_count(), 0);
}

#[test]
fn undo_walks_back_a_delete() {
    let mut f = Fixture::new(&["one", "two"]);
    f.select("two");
    let i = f.session.visible[f.session.sel];
    f.session.push_undo("delete", &[i]);
    f.session.msgs[i].env.file.flags.deleted = true;
    assert!(f.is_deleted("two"));
    f.session.undo_last();
    assert!(!f.is_deleted("two"));
    assert!(f.log.said("undone: delete"), "{}", f.log.last_text());
}

#[test]
fn undo_says_so_when_there_is_nothing_to_walk_back() {
    let mut f = Fixture::new(&["one"]);
    f.session.undo_last();
    assert_eq!(f.log.last_text(), "nothing to undo");
}

#[test]
fn a_pattern_delete_is_one_undo_step() {
    let mut f = Fixture::new(&["alpha", "beta", "gamma"]);
    let ask = Some(f.session.ask_pattern(PatternOp::Delete).unwrap());
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Delete messages matching: "
    );
    f.answer_line(ask, "~s ma");
    assert!(f.is_deleted("gamma"));
    assert!(!f.is_deleted("alpha") && !f.is_deleted("beta"));
    f.session.undo_last();
    assert!(!f.is_deleted("gamma"));
}

#[test]
fn a_tagged_operation_applies_to_the_tagged_set() {
    let mut f = Fixture::new(&["one", "two", "three"]);
    f.tag("one");
    f.tag("three");
    f.session
        .each_tagged("delete", |m| m.env.file.flags.deleted = true);
    assert!(f.is_deleted("one") && f.is_deleted("three"));
    assert!(!f.is_deleted("two"));
    assert!(f.log.said("2 tagged message(s)"), "{}", f.log.last_text());
    // And it is one step, so undo takes the whole set back.
    f.session.undo_last();
    assert!(!f.is_deleted("one") && !f.is_deleted("three"));
}

#[test]
fn without_the_tag_prefix_an_operation_is_about_the_cursor() {
    let mut f = Fixture::new(&["one", "two"]);
    f.tag("one");
    f.select("two");
    assert_eq!(f.session.op_targets(true).len(), 1);
    assert_eq!(
        f.session.op_targets(false),
        vec![f.session.visible[f.session.sel]]
    );
}

#[test]
fn a_limit_hides_what_does_not_match_and_all_brings_it_back() {
    let mut f = Fixture::new(&["report", "lunch", "report two"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~s report");
    assert_eq!(f.subjects(), ["report", "report two"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "all");
    assert_eq!(f.subjects(), ["report", "lunch", "report two"]);
}

#[test]
fn a_limit_matching_nothing_says_so() {
    let mut f = Fixture::new(&["one"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~s nothing");
    assert_eq!(f.subjects(), Vec::<String>::new());
    assert_eq!(f.log.last_text(), "no messages match the limit");
}

#[test]
fn sorting_reorders_the_index() {
    let mut f = Fixture::new(&["beta", "alpha", "gamma"]);
    let ask = Some(f.session.ask_sort());
    f.answer_key(ask, 's');
    assert_eq!(f.subjects(), ["alpha", "beta", "gamma"]);
    assert_eq!(f.session.sort, SortKey::Subject);
    let ask = Some(f.session.ask_sort());
    f.answer_key(ask, 'S');
    assert_eq!(f.subjects(), ["gamma", "beta", "alpha"]);
    assert!(f.session.sort_rev);
}

#[test]
fn a_sync_reports_what_it_did_in_numbers() {
    let mut f = Fixture::new(&["one", "two", "three"]);
    let i = f.session.visible[0];
    f.session.msgs[i].env.file.flags.deleted = true;
    let j = f.session.visible[1];
    f.session.msgs[j].env.file.flags.flagged = true;
    f.session.msgs[j].dirty = true;
    f.session.sync(true);
    assert_eq!(
        f.log.notices().last(),
        Some(&Notice::Synced {
            deleted: 1,
            updated: 1
        })
    );
    assert_eq!(f.subjects(), ["two", "three"]);
    // The stack cannot walk back what is on disk.
    f.session.undo_last();
    assert_eq!(f.log.last_text(), "nothing to undo");
}

#[test]
fn the_purge_question_is_asked_once_and_answered_with_a_key() {
    let mut f = Fixture::new(&["one", "two"]);
    let i = f.session.visible[0];
    f.session.msgs[i].env.file.flags.deleted = true;
    let ask = f.session.ask_purge(false).expect("it asks");
    assert_eq!(ask_label(&ask), "Purge 1 deleted message(s)? (y/n): ");
    f.answer_key(Some(ask), 'n');
    // n keeps the mark and the message.
    assert_eq!(f.subjects(), ["one", "two"]);
    assert!(f.is_deleted("one"));
    let ask = f.session.ask_purge(false);
    f.answer_key(ask, 'y');
    assert_eq!(f.subjects(), ["two"]);
}

#[test]
fn delete_yes_purges_without_asking() {
    let mut config = Config::default();
    config.mail.delete = Some("yes".into());
    let mut f = Fixture::with_config(&["one", "two"], config);
    let i = f.session.visible[0];
    f.session.msgs[i].env.file.flags.deleted = true;
    assert!(f.session.ask_purge(false).is_none(), "nothing to ask");
    assert_eq!(f.subjects(), ["two"]);
}

#[test]
fn saving_copies_the_message_and_marks_the_original() {
    let target = tempfile::tempdir().unwrap();
    let mut f = Fixture::new(&["keep me", "other"]);
    f.select("keep me");
    let ask = f.session.ask_copy(true, false);
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Save to mailbox: ");
    f.answer_line(ask, target.path().to_str().unwrap());
    assert!(f.is_deleted("keep me"));
    let copies: Vec<_> = fs::read_dir(target.path().join("cur")).unwrap().collect();
    assert_eq!(copies.len(), 1);
    assert!(f.log.said("saved to"), "{}", f.log.last_text());
    // Undo removes the copy and the mark together.
    f.session.undo_last();
    assert!(!f.is_deleted("keep me"));
    assert_eq!(fs::read_dir(target.path().join("cur")).unwrap().count(), 0);
}

#[test]
fn a_read_only_mailbox_refuses_the_operations_that_write() {
    let mut f = Fixture::new(&["one"]);
    f.session.read_only = true;
    assert!(f.session.ask_copy(true, false).is_none(), "no save prompt");
    assert_eq!(f.log.last_text(), "Mailbox is read-only.");
    // A plain copy writes nothing here, so it still asks.
    assert!(f.session.ask_copy(false, false).is_some());
}

// ---- the helpers that came with the logic
#[test]
fn subject_key_strips_reply_prefixes() {
    assert_eq!(subject_key("Re: Re: Lunch"), "lunch");
    assert_eq!(subject_key("FWD: re: x"), "x");
    assert_eq!(subject_key("Redo"), "redo");
}

#[test]
fn wrap_order_visits_everything_once() {
    assert_eq!(
        wrap_order(4, 1, true),
        vec![(2, false), (3, false), (0, true), (1, true)]
    );
    assert_eq!(
        wrap_order(4, 1, false),
        vec![(0, false), (3, true), (2, true), (1, true)]
    );
    assert_eq!(wrap_order(0, 0, true), vec![]);
    assert_eq!(wrap_order(1, 0, true), vec![(0, true)]);
}

// ---- threads

/// A thread: each message after the first answers the one before it.
fn thread_fixture() -> Fixture {
    thread_fixture_with(Config::default())
}

fn thread_fixture_with(config: Config) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "root", None);
    write_message(
        dir.path(),
        1,
        "Re: root",
        Some("In-Reply-To: <m0@example.com>\nReferences: <m0@example.com>"),
    );
    write_message(
        dir.path(),
        2,
        "Re: root again",
        Some("In-Reply-To: <m1@example.com>\nReferences: <m0@example.com> <m1@example.com>"),
    );
    write_message(dir.path(), 3, "unrelated", None);
    let (mut session, _) = Session::open(dir.path(), config).unwrap();
    let log = Log::default();
    session.install_notices(Box::new(log.clone()));
    session.sort = SortKey::Threads;
    session.apply_sort();
    Fixture {
        _dir: dir,
        session,
        log,
    }
}

#[test]
fn a_thread_op_takes_the_whole_thread() {
    let mut f = thread_fixture();
    f.select("Re: root");
    f.session.thread_mark(false, crate::ThreadOp::Delete);
    assert!(f.is_deleted("root") && f.is_deleted("Re: root") && f.is_deleted("Re: root again"));
    assert!(!f.is_deleted("unrelated"));
    f.session.undo_last();
    assert!(!f.is_deleted("root"));
}

#[test]
fn a_subthread_op_stops_at_the_message_it_starts_from() {
    let mut f = thread_fixture();
    f.select("Re: root");
    f.session.thread_mark(true, crate::ThreadOp::Delete);
    assert!(!f.is_deleted("root"), "the parent stays");
    assert!(f.is_deleted("Re: root") && f.is_deleted("Re: root again"));
}

#[test]
fn folding_a_thread_hides_its_replies() {
    let mut f = thread_fixture();
    f.select("root");
    f.session.toggle_collapse(false);
    assert_eq!(f.subjects(), ["root", "unrelated"]);
    f.session.toggle_collapse(false);
    assert_eq!(f.subjects().len(), 4);
}

// ---- hooks

#[test]
fn a_folder_hook_sets_what_it_names_for_this_mailbox() {
    let mut config = Config::default();
    config.folder_hooks.push(rmut_core::config::FolderHook {
        folder: "*".into(),
        command: "set sort=subject".into(),
    });
    let mut f = Fixture::with_config(&["beta", "alpha"], config);
    f.session.run_folder_hooks();
    assert_eq!(f.subjects(), ["alpha", "beta"], "the hook re-sorted");
}

#[test]
fn a_message_hook_is_undone_when_it_stops_matching() {
    let mut config = Config::default();
    config.message_hooks.push(rmut_core::config::MessageHook {
        pattern: "~s special".into(),
        command: "set beep=false".into(),
    });
    let mut f = Fixture::with_config(&["ordinary", "special"], config);
    assert!(f.session.config.ui.beep, "beeping is the default");
    f.select("special");
    f.session.sync_message_hooks();
    assert!(!f.session.config.ui.beep, "the hook took effect");
    f.select("ordinary");
    f.session.sync_message_hooks();
    assert!(f.session.config.ui.beep, "and was put back");
}

#[test]
fn a_command_the_session_does_not_own_comes_back_as_a_request() {
    let mut f = Fixture::new(&["one"]);
    let run = f.session.run_command_line("bind index x quit");
    assert!(run.reports.is_empty());
    let mut asked = Vec::new();
    while let Some(request) = f.session.take_request() {
        asked.push(request);
    }
    assert!(
        asked
            .iter()
            .any(|r| matches!(r, crate::Request::Command(_))),
        "the bind is the front end's"
    );
    assert!(
        asked
            .iter()
            .any(|r| matches!(r, crate::Request::ConfigChanged)),
        "and the config moved"
    );
}

#[test]
fn an_fcc_hook_names_where_the_copy_goes() {
    let mut config = Config::default();
    config.fcc_hooks.push(rmut_core::config::FccHook {
        pattern: "~t boss@example.com".into(),
        mailbox: "=important".into(),
    });
    let f = Fixture::with_config(&["one"], config);
    let draft = "From: me@example.com\nTo: boss@example.com\nSubject: hi\n\nbody\n";
    assert_eq!(
        f.session.fcc_hook_target(draft, Path::new("/tmp/draft")),
        Some("=important".to_string())
    );
    let other = "From: me@example.com\nTo: someone@example.com\nSubject: hi\n\nbody\n";
    assert_eq!(
        f.session.fcc_hook_target(other, Path::new("/tmp/draft")),
        None
    );
}

// ---- composing

/// Take the draft the compose flow ends by asking for an editor on.
fn draft_from_requests(session: &mut Session) -> crate::Compose {
    while let Some(request) = session.take_request() {
        if let crate::Request::Editor(compose) = request {
            return compose;
        }
    }
    panic!("no draft was handed to an editor");
}

fn reply_config() -> Config {
    let mut config = Config::default();
    config.identity.email = Some("me@example.com".into());
    config
}

#[test]
fn a_reply_asks_its_way_to_a_draft() {
    let mut f = Fixture::with_config(&["Lunch on Friday?"], reply_config());
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    assert_eq!(ask_label(ask.as_ref().unwrap()), "To: ");
    let ask = f.answer_line(ask, "sender@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Subject: ");
    let ask = f.answer_line(ask, "Re: Lunch on Friday?");
    // mutt's $include, on a reply: quote the original?
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Include message in reply? (y/n): "
    );
    assert!(
        f.answer_key(ask, 'y').is_none(),
        "that was the last question"
    );
    let draft = draft_from_requests(&mut f.session);
    // The headers are withheld from the editor by default (mutt's
    // $edit_headers), so the whole draft is what to look at.
    let text = crate::draft_full(&draft).unwrap();
    assert!(text.contains("To: sender@example.com"), "{text}");
    assert!(text.contains("Subject: Re: Lunch on Friday?"), "{text}");
    assert!(
        text.contains("body of Lunch on Friday?"),
        "the original is quoted: {text}"
    );
}

#[test]
fn fast_reply_skips_the_questions_it_can_answer_itself() {
    let mut config = reply_config();
    config.mail.fast_reply = true;
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    // Straight to the include question: To and Subject take their
    // prefills.
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Include message in reply? (y/n): "
    );
    f.answer_key(ask, 'n');
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(text.contains("To: Sender 0 <s0@example.com>"), "{text}");
    assert!(!text.contains("body of Lunch"), "n means no quote: {text}");
}

#[test]
fn an_empty_subject_raises_mutts_question_and_aborts_on_it() {
    let mut f = Fixture::with_config(&["one"], reply_config());
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Subject: ");
    let ask = f.answer_line(ask, "");
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "No subject, abort? (y/n): "
    );
    // ask-yes: anything but n aborts.
    assert!(f.answer_key(ask, 'y').is_none());
    assert_eq!(f.log.last_text(), "aborted (no subject)");
    assert!(f.session.take_request().is_none(), "no draft was written");
}

#[test]
fn the_attachment_reminder_reads_the_body_not_the_quotes() {
    let mut config = reply_config();
    config.mail.abort_noattach = Some("ask".into());
    let f = Fixture::with_config(&["one"], config);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("draft");
    fs::write(&path, "x").unwrap();
    let draft = crate::Compose {
        path,
        recall_source: None,
        security: crate::Security::None,
        attach: None,
        hidden_head: None,
        fcc: None,
    };
    let says_it = "To: you@example.com\nSubject: x\n\nthe file is attached\n";
    assert!(f.session.attachment_forgotten(says_it, &draft));
    let quoted = "To: you@example.com\nSubject: x\n\n> the file is attached\nthanks\n";
    assert!(
        !f.session.attachment_forgotten(quoted, &draft),
        "a quote is not a promise"
    );
    let with_one = "To: you@example.com\nSubject: x\nAttach: /etc/hostname\n\nattached\n";
    assert!(!f.session.attachment_forgotten(with_one, &draft));
}

#[test]
fn a_bounce_asks_who_and_then_confirms() {
    let mut f = Fixture::new(&["one"]);
    let ask = f.session.ask_bounce(false);
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Bounce message to: ");
    let ask = f.answer_line(ask, "someone@example.com");
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Bounce message to someone@example.com? (y/n): "
    );
    // Anything but y calls it off, and nothing is said.
    assert!(f.answer_key(ask, 'n').is_none());
    assert_eq!(f.log.last_text(), "");
}

#[test]
fn a_bounce_to_nobody_says_so_instead_of_confirming() {
    let mut f = Fixture::new(&["one"]);
    let ask = f.session.ask_bounce(false);
    assert!(f.answer_line(ask, "   ").is_none());
    assert_eq!(f.log.last_text(), "no recipients, bounce cancelled");
}

// ---- patterns, in the terms only a whole mailbox can answer

/// A mailbox with the awkward cases: an unusual header, a References
/// line, a big body, and two copies of one Message-ID.
fn pattern_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "lunch", None);
    write_message(dir.path(), 1, "ci", None);
    fs::write(
        dir.path().join("cur/0002.rmut:2,"),
        format!(
            "From: bulk@example.com\nTo: me@example.com\nSubject: newsletter\n\
             Date: Mon, 12 Mar 2024 10:00:00 +0000\nMessage-ID: <m2@example.com>\n\
             References: <m0@example.com>\nX-Spam-Score: 9.5\n\n{}",
            "padding line\n".repeat(400)
        ),
    )
    .unwrap();
    // A duplicate of the first message: same Message-ID, other file.
    fs::write(
        dir.path().join("cur/0003.rmut:2,"),
        "From: Sender 0 <s0@example.com>\nTo: me@example.com\nSubject: lunch (dup)\n\
         Date: Mon, 13 Mar 2024 10:00:00 +0000\nMessage-ID: <m0@example.com>\n\nsecond copy\n",
    )
    .unwrap();
    let (mut session, _) = Session::open(dir.path(), Config::default()).unwrap();
    let log = Log::default();
    session.install_notices(Box::new(log.clone()));
    Fixture {
        _dir: dir,
        session,
        log,
    }
}

impl Fixture {
    /// Limit to `pattern` and hand back what is left on screen.
    fn limited_to(&mut self, pattern: &str) -> Vec<String> {
        let ask = Some(self.session.ask_limit());
        self.answer_line(ask, pattern);
        self.subjects()
    }
}

#[test]
fn a_limit_can_ask_about_any_header() {
    let mut f = pattern_fixture();
    assert_eq!(f.limited_to("~h x-spam"), ["newsletter"]);
}

#[test]
fn message_ids_and_references_are_their_own_terms() {
    let mut f = pattern_fixture();
    // ~i is the id itself: the original and its duplicate.
    assert_eq!(f.limited_to("~i m0@"), ["lunch", "lunch (dup)"]);
    // ~x is what a message answers.
    assert_eq!(f.limited_to("~x m0@"), ["newsletter"]);
}

#[test]
fn duplicates_are_found_by_the_id_they_share() {
    let mut f = pattern_fixture();
    assert_eq!(f.limited_to("~="), ["lunch", "lunch (dup)"]);
}

#[test]
fn sizes_take_ranges_with_units() {
    let mut f = pattern_fixture();
    assert_eq!(f.limited_to("~z >4K"), ["newsletter"]);
    assert_eq!(f.limited_to("~z <4K").len(), 3);
}

#[test]
fn index_ranges_count_what_is_on_screen() {
    let mut f = pattern_fixture();
    assert_eq!(f.limited_to("~m 1-2"), ["lunch", "ci"]);
    // `.` is the cursor and `$` the last row, so on the last message
    // the range is just it.
    f.limited_to("all");
    f.session.select(usize::MAX);
    assert_eq!(f.limited_to("~m .-$"), ["lunch (dup)"]);
}

#[test]
fn a_pattern_op_takes_the_same_terms_as_a_limit() {
    let mut f = pattern_fixture();
    let ask = f.session.ask_pattern(PatternOp::Tag);
    f.answer_line(ask, "~=");
    assert!(f.is_tagged("lunch") && f.is_tagged("lunch (dup)"));
    assert!(!f.is_tagged("ci"));
    assert!(f.log.said("2 tagged"), "{}", f.log.last_text());
}

// ---- how long the network gets

#[test]
fn the_config_decides_how_long_a_connection_waits() {
    let config = Config::default();
    assert_eq!(config.net.connect_timeout, 10, "seconds, by default");
    assert_eq!(config.net.timeout, 30);
    let mut f = Fixture::with_config(&["one"], config);
    // Settable while running, like mutt's $connect_timeout. What the
    // numbers mean to the socket is net.rs's own test; they are a
    // process-wide setting, so asserting on them here would depend on
    // what the other tests in this file happen to be doing.
    f.session.run_command_line("set net_timeout=45");
    assert_eq!(f.session.config.net.timeout, 45);
    f.session.run_command_line("set connect_timeout=0");
    assert_eq!(f.session.config.net.connect_timeout, 0);
}

// ---- the reply and forward text

#[test]
fn a_quoted_reply_uses_the_configured_attribution_and_indent() {
    let mut config = reply_config();
    config.mail.attribution = Some("%n wrote on %{%Y-%m-%d}:".into());
    config.mail.indent_string = Some("| ".into());
    config.mail.include = Some("yes".into());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    let ask = f.answer_line(ask, "sender@example.com");
    // $include = yes: no question, straight to the draft.
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Subject: ");
    assert!(f.answer_line(ask, "Re: Lunch").is_none());
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(text.contains("Sender 0 wrote on 2024-03-"), "{text}");
    assert!(text.contains("| body of Lunch on Friday?"), "{text}");
}

#[test]
fn include_no_leaves_the_original_out_without_asking() {
    let mut config = reply_config();
    config.mail.include = Some("no".into());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    let ask = f.answer_line(ask, "sender@example.com");
    assert!(f.answer_line(ask, "Re: Lunch").is_none(), "no question");
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(!text.contains("body of Lunch"), "{text}");
}

#[test]
fn ask_no_makes_enter_mean_no() {
    let mut config = reply_config();
    config.mail.include = Some("ask-no".into());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    let ask = f.answer_line(ask, "sender@example.com");
    let ask = f.answer_line(ask, "Re: Lunch");
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Include message in reply? (y/n): "
    );
    let what = ask_kind(ask.unwrap());
    f.session.answer(what, Answer::Key(Key::Enter));
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(!text.contains("body of Lunch"), "Enter took the no: {text}");
}

#[test]
fn a_forward_takes_its_subject_from_the_format() {
    let mut config = reply_config();
    config.mail.forward_format = Some("Fwd: %s (%n)".into());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Forward);
    let ask = f.answer_line(ask, "someone@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Subject: ");
    match ask.unwrap() {
        Ask::Line { prefill, .. } => {
            assert_eq!(prefill, "Fwd: Lunch on Friday? (Sender 0)");
        }
        _ => panic!("a line was asked for"),
    }
}

#[test]
fn askcc_and_askbcc_ask_between_to_and_subject() {
    let mut config = reply_config();
    config.mail.ask_cc = true;
    config.mail.ask_bcc = true;
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "to@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Cc: ");
    let ask = f.answer_line(ask, "cc@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Bcc: ");
    let ask = f.answer_line(ask, "bcc@example.com");
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Subject: ");
    assert!(f.answer_line(ask, "hello").is_none());
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(text.contains("Cc: cc@example.com"), "{text}");
    assert!(text.contains("Bcc: bcc@example.com"), "{text}");
}

// ---- reading habits

#[test]
fn collapse_unread_off_leaves_unread_threads_open() {
    let mut config = Config::default();
    config.index.collapse_unread = Some(false);
    let mut f = thread_fixture_with(config);
    // The thread's second message is unread, so folding everything
    // leaves it open; the lone read message folds like any other.
    let i = f
        .session
        .msgs
        .iter()
        .position(|m| m.env.subject == "Re: root")
        .unwrap();
    f.session.msgs[i].env.file.flags.seen = false;
    for j in [0, 2, 3] {
        f.session.msgs[j].env.file.flags.seen = true;
    }
    f.session.toggle_collapse(true);
    assert_eq!(f.subjects().len(), 4, "the unread thread stayed open");
}

#[test]
fn uncollapse_jump_lands_on_the_unread_one() {
    let mut config = Config::default();
    config.index.uncollapse_jump = true;
    let mut f = thread_fixture_with(config);
    for m in &mut f.session.msgs {
        m.env.file.flags.seen = true;
    }
    let i = f
        .session
        .msgs
        .iter()
        .position(|m| m.env.subject == "Re: root again")
        .unwrap();
    f.session.msgs[i].env.file.flags.seen = false;
    f.select("root");
    f.session.toggle_collapse(false); // fold
    f.session.toggle_collapse(false); // and open again
    assert_eq!(
        f.subjects()[f.session.sel],
        "Re: root again",
        "the cursor followed the unread message"
    );
}

// ---- leaving and filing habits

#[test]
fn quit_no_refuses_and_ask_yes_asks_first() {
    let mut config = Config::default();
    config.mail.quit = Some("no".into());
    let mut f = Fixture::with_config(&["one"], config);
    assert!(f.session.leave().is_none());
    assert_eq!(f.log.last_text(), "quitting is off ($quit = no)");
    assert!(f.session.take_request().is_none(), "still here");

    let mut config = Config::default();
    config.mail.quit = Some("ask-yes".into());
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.leave();
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Quit rmut? (y/n): ");
    // n stays; Enter takes the yes that ask-yes names.
    f.answer_key(ask, 'n');
    assert!(f.session.take_request().is_none(), "n stayed");
    let ask = f.session.leave();
    let what = ask_kind(ask.unwrap());
    f.session.answer(what, Answer::Key(Key::Enter));
    assert!(
        matches!(f.session.take_request(), Some(crate::Request::Quit)),
        "Enter took the yes"
    );
}

#[test]
fn confirmappend_asks_before_adding_to_a_mailbox_that_exists() {
    let target = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(target.path().join(sub)).unwrap();
    }
    let mut config = Config::default();
    config.mail.confirmappend = true;
    let mut f = Fixture::with_config(&["one"], config);
    let path = target.path().to_str().unwrap().to_string();
    let ask = f.session.ask_copy(false, false);
    let ask = f.answer_line(ask, &path);
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        format!("Append messages to {path}? (y/n): ")
    );
    // n leaves the mailbox alone.
    f.answer_key(ask, 'n');
    assert_eq!(fs::read_dir(target.path().join("cur")).unwrap().count(), 0);
    let ask = f.session.ask_copy(false, false);
    let ask = f.answer_line(ask, &path);
    f.answer_key(ask, 'y');
    assert_eq!(fs::read_dir(target.path().join("cur")).unwrap().count(), 1);
}

#[test]
fn save_name_offers_the_mailbox_named_after_the_sender() {
    let folder = tempfile::tempdir().unwrap();
    // $folder/s0 exists, so $save_name offers it; nothing else does.
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(folder.path().join("s0").join(sub)).unwrap();
    }
    let mut config = Config::default();
    config.mail.folder = Some(folder.path().display().to_string());
    config.mail.save_name = true;
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.ask_copy(true, false).unwrap();
    match &ask {
        Ask::Line { prefill, .. } => assert_eq!(prefill, "=s0"),
        _ => panic!("a line was asked for"),
    }
    // Without a mailbox of that name, $save_name offers nothing and
    // $force_name offers it anyway.
    f.session.config.mail.save_name = false;
    f.session.config.mail.force_name = true;
    fs::remove_dir_all(folder.path().join("s0")).unwrap();
    match f.session.ask_copy(true, false).unwrap() {
        Ask::Line { prefill, .. } => assert_eq!(prefill, "=s0"),
        _ => panic!("a line was asked for"),
    }
}
