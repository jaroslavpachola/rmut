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

    /// Enter at a y/n question: whichever way the quadoption leans.
    fn answer_enter(&mut self, ask: Option<Ask>) -> Option<Ask> {
        let what = ask_kind(ask.expect("a question was asked"));
        self.session.answer(what, Answer::Key(Key::Enter))
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

/// A message written in the same clock tick as the fixture leaves
/// new/ and cur/ with the mtimes the session already saw, so the poll
/// would skip the rescan. Push them a second on, as time would.
fn touch_dirs(dir: &Path) {
    for sub in ["new", "cur"] {
        let f = fs::File::open(dir.join(sub)).unwrap();
        let t = f.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(1);
        f.set_modified(t).unwrap();
    }
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
fn abort_nosubject_can_ask_the_other_way_or_not_at_all() {
    // ask-no: the question still comes, but Enter keeps the draft.
    let mut config = reply_config();
    config.mail.abort_nosubject = Some("ask-no".into());
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    let ask = f.answer_line(ask, "");
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "No subject, abort? (y/n): "
    );
    assert!(f.answer_enter(ask).is_none());
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(
        text.lines().any(|l| l.trim() == "Subject:"),
        "an empty subject: {text}"
    );

    // no: nothing is asked at all.
    let mut config = reply_config();
    config.mail.abort_nosubject = Some("no".into());
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "").is_none(), "no question");
    draft_from_requests(&mut f.session);

    // yes: aborted without one.
    let mut config = reply_config();
    config.mail.abort_nosubject = Some("yes".into());
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "").is_none());
    assert_eq!(f.log.last_text(), "aborted (no subject)");
    assert!(f.session.take_request().is_none(), "no draft was written");
}

#[test]
fn a_signature_ends_every_draft_it_starts() {
    let dir = tempfile::tempdir().unwrap();
    let sig = dir.path().join("signature");
    fs::write(&sig, "Ann\nx.example\n").unwrap();
    let mut config = reply_config();
    config.mail.signature = Some(sig.to_string_lossy().into_owned());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Reply);
    let ask = f.answer_line(ask, "sender@example.com");
    let ask = f.answer_line(ask, "Re: Lunch");
    f.answer_key(ask, 'y');
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    // Under the quoted original, not over it.
    assert!(text.ends_with("\n-- \nAnn\nx.example\n"), "{text}");
    assert!(
        text.find("body of Lunch").unwrap() < text.find("-- \nAnn").unwrap(),
        "{text}"
    );
}

#[test]
fn forward_quote_indents_the_forwarded_message() {
    let mut config = reply_config();
    config.mail.forward_quote = true;
    config.mail.indent_string = Some("| ".into());
    let mut f = Fixture::with_config(&["Lunch on Friday?"], config);
    f.select("Lunch on Friday?");
    let ask = f.session.start_compose(crate::ComposeKind::Forward);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "[Fwd: Lunch]").is_none());
    let draft = draft_from_requests(&mut f.session);
    let text = crate::draft_full(&draft).unwrap();
    assert!(text.contains("\n| body of Lunch on Friday?"), "{text}");
    assert!(
        text.contains("\n----- End forwarded message -----"),
        "the markers stay flush: {text}"
    );
}

#[test]
fn an_untouched_first_edit_drops_the_draft() {
    let mut f = Fixture::with_config(&["one"], reply_config());
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "hi").is_none());
    let draft = draft_from_requests(&mut f.session);
    let path = draft.path.clone();
    // The editor came back with the file exactly as it was.
    f.session.set_draft(draft);
    assert!(f.session.draft().is_none(), "the draft is gone");
    assert_eq!(f.log.last_text(), "aborted unmodified message");
    assert!(!path.exists(), "and so is the file");

    // A body typed into it is a message, and $abort_unmodified only
    // looks at the first edit: the same file, handed back twice.
    let mut f = Fixture::with_config(&["one"], reply_config());
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "hi").is_none());
    let draft = draft_from_requests(&mut f.session);
    let path = draft.path.clone();
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}something to say\n")).unwrap();
    f.session.set_draft(draft);
    assert!(f.session.draft().is_some());
    let again = crate::Compose {
        path: path.clone(),
        recall_source: None,
        security: crate::Security::None,
        attach: None,
        hidden_head: None,
        fcc: None,
    };
    f.session.set_draft(again);
    assert!(f.session.draft().is_some(), "a re-edit is not the first");
}

#[test]
fn abort_unmodified_off_keeps_the_untouched_draft() {
    let mut config = reply_config();
    config.mail.abort_unmodified = Some(false);
    let mut f = Fixture::with_config(&["one"], config);
    let ask = f.session.start_compose(crate::ComposeKind::New);
    let ask = f.answer_line(ask, "someone@example.com");
    assert!(f.answer_line(ask, "hi").is_none());
    let draft = draft_from_requests(&mut f.session);
    f.session.set_draft(draft);
    assert!(f.session.draft().is_some());
}

#[test]
fn mark_old_can_be_told_not_to() {
    // A message in new/: leaving the mailbox ages it, as mutt does.
    let mut f = Fixture::new(&["read"]);
    let arrival = f._dir.path().join("new").join("1234.rmut");
    fs::write(&arrival, "From: s@example.com\nSubject: fresh\n\nbody\n").unwrap();
    touch_dirs(f._dir.path());
    f.session.check_new_mail();
    assert_eq!(f.session.new_count(), 1);
    f.session.mark_old_unread();
    assert_eq!(f.session.new_count(), 0, "aged to old on the way out");
    assert!(!arrival.exists(), "and moved out of new/");

    // $mark_old off: it is still new next time.
    let mut config = Config::default();
    config.mail.mark_old = Some(false);
    let mut f = Fixture::with_config(&["read"], config);
    let arrival = f._dir.path().join("new").join("1234.rmut");
    fs::write(&arrival, "From: s@example.com\nSubject: fresh\n\nbody\n").unwrap();
    touch_dirs(f._dir.path());
    f.session.check_new_mail();
    f.session.mark_old_unread();
    assert_eq!(f.session.new_count(), 1, "left as it was found");
    assert!(arrival.exists());
}

#[test]
fn an_arrival_says_so_as_an_arrival() {
    // $beep_new needs the notice to be recognizable without reading
    // the sentence.
    let mut f = Fixture::new(&["read"]);
    fs::write(
        f._dir.path().join("new").join("1234.rmut"),
        "From: s@example.com\nSubject: fresh\n\nbody\n",
    )
    .unwrap();
    touch_dirs(f._dir.path());
    f.session.check_new_mail();
    let said = f.log.notices();
    assert!(
        said.iter().any(rmut_core::notice::Notice::is_new_mail),
        "{said:?}"
    );
    assert!(f.log.said("new mail in"), "and it still reads as prose");
}

#[test]
fn the_print_question_takes_mutts_four_answers() {
    let printed = tempfile::tempdir().unwrap();
    let target = printed.path().join("out");
    let command = format!("cat > {}", target.display());
    let config_with = |quad: Option<&str>| {
        let mut config = Config::default();
        config.mail.print = Some(command.clone());
        config.mail.print_confirm = quad.map(str::to_string);
        config
    };

    // The default is mutt's ask-no: the question comes, Enter is no.
    let mut f = Fixture::with_config(&["one"], config_with(None));
    let ask = f.session.ask_print(false);
    assert_eq!(ask_label(ask.as_ref().unwrap()), "Print message? (y/n): ");
    f.answer_enter(ask);
    assert!(!target.exists(), "Enter declined");

    // ask-yes: the same question, the other default.
    let mut f = Fixture::with_config(&["one"], config_with(Some("ask-yes")));
    let ask = f.session.ask_print(false);
    assert!(ask.is_some());
    f.answer_enter(ask);
    assert!(target.exists(), "Enter printed");
    fs::remove_file(&target).unwrap();

    // yes: no question at all.
    let mut f = Fixture::with_config(&["one"], config_with(Some("yes")));
    assert!(f.session.ask_print(false).is_none());
    assert!(f.log.said("printed via"), "{}", f.log.last_text());
    assert!(target.exists());
    fs::remove_file(&target).unwrap();

    // no: nothing printed, and it says why.
    let mut f = Fixture::with_config(&["one"], config_with(Some("no")));
    assert!(f.session.ask_print(false).is_none());
    assert!(f.log.said("printing is off"), "{}", f.log.last_text());
    assert!(!target.exists());
}

#[test]
fn reverse_realname_off_keeps_the_configured_name() {
    let with = |realname: Option<bool>| {
        let mut config = Config::default();
        config.identity.name = Some("Jane at home".into());
        config.identity.email = Some("work@example.com".into());
        config.identity.reverse_name = true;
        config.identity.reverse_realname = realname;
        config
    };
    let base = |f: &mut Fixture| {
        f.select("to my work address");
        f.session.compose_base().unwrap()
    };
    // The message came to "Jane Work <work@example.com>": on (mutt's
    // default) that whole form is the reply's From.
    let mut f = Fixture::with_config(&[], with(None));
    write_message(
        f._dir.path(),
        0,
        "to my work address",
        Some("Cc: Jane Work <work@example.com>"),
    );
    touch_dirs(f._dir.path());
    f.session.check_new_mail();
    let b = base(&mut f);
    assert_eq!(
        f.session.compose_from(Some(&b), "").as_deref(),
        Some("Jane Work <work@example.com>")
    );

    // Off: the address moves, the configured name stays.
    let mut f = Fixture::with_config(&[], with(Some(false)));
    write_message(
        f._dir.path(),
        0,
        "to my work address",
        Some("Cc: Jane Work <work@example.com>"),
    );
    touch_dirs(f._dir.path());
    f.session.check_new_mail();
    let b = base(&mut f);
    assert_eq!(
        f.session.compose_from(Some(&b), "").as_deref(),
        Some("Jane at home <work@example.com>")
    );
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

// ---- how the threads themselves are ordered

/// An old thread with a new reply, and a standalone message in
/// between: the two orders disagree about which comes first.
fn aux_fixture(sort_aux: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "old root", None);
    write_message(dir.path(), 1, "lone", None);
    write_message(
        dir.path(),
        2,
        "Re: old root",
        Some("In-Reply-To: <m0@example.com>\nReferences: <m0@example.com>"),
    );
    let mut config = Config::default();
    config.index.sort_aux = sort_aux.map(str::to_string);
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
fn sort_aux_decides_which_thread_comes_first() {
    // By the root's date: the old thread, then the lone message.
    assert_eq!(
        aux_fixture(None).subjects(),
        ["old root", "Re: old root", "lone"]
    );
    // By the newest message in each: the lone one is older than the
    // reply, so it goes first.
    assert_eq!(
        aux_fixture(Some("last-date-received")).subjects(),
        ["lone", "old root", "Re: old root"]
    );
    // reverse- turns the threads round and leaves each one's own
    // order alone.
    assert_eq!(
        aux_fixture(Some("reverse-date")).subjects(),
        ["lone", "old root", "Re: old root"]
    );
    assert_eq!(
        aux_fixture(Some("reverse-last-date-sent")).subjects(),
        ["old root", "Re: old root", "lone"]
    );
}

/// A thread of three in `dir/cur`: root, a reply, and a reply to the
/// reply, dated in that order.
fn write_thread(dir: &Path) {
    write_message(dir, 0, "root", None);
    write_message(dir, 1, "reply", Some("In-Reply-To: <m0@example.com>"));
    write_message(
        dir,
        2,
        "again",
        Some("References: <m0@example.com> <m1@example.com>"),
    );
}

fn threaded() -> Fixture {
    let mut f = Fixture::new(&[]);
    write_thread(f._dir.path());
    write_message(f._dir.path(), 3, "loner", None);
    f.session.rescan();
    f.session.sort = SortKey::Threads;
    f.session.apply_sort();
    f
}

fn depths(f: &Fixture) -> Vec<(String, usize)> {
    f.session
        .visible
        .iter()
        .map(|&i| {
            (
                f.session.msgs[i].env.subject.clone(),
                f.session.thread_depth[i],
            )
        })
        .collect()
}

#[test]
fn break_thread_rewrites_the_message_and_undo_puts_it_back() {
    let mut f = threaded();
    assert_eq!(
        depths(&f),
        [("root", 0), ("reply", 1), ("again", 2), ("loner", 0)].map(|(s, d)| (s.to_string(), d))
    );
    let path = f.path_of("reply");
    let before = fs::read(&path).unwrap();

    // The reply and what hangs under it become a thread of their
    // own; the file lost its In-Reply-To.
    f.select("reply");
    f.session.break_thread();
    assert_eq!(f.log.last_text(), "thread broken");
    assert_eq!(
        depths(&f),
        [("root", 0), ("reply", 0), ("again", 1), ("loner", 0)].map(|(s, d)| (s.to_string(), d))
    );
    let after = fs::read_to_string(&path).unwrap();
    assert!(!after.contains("In-Reply-To"), "{after}");
    assert!(after.contains("body of reply"), "the body is untouched");
    assert_eq!(f.subjects()[1], "reply", "the cursor's message stays");

    // A root has nothing to break.
    f.select("root");
    f.session.break_thread();
    assert_eq!(f.log.last_text(), "already a thread of its own");

    // z writes the old bytes back and re-threads.
    f.session.undo_last();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(depths(&f)[1], ("reply".to_string(), 1));
    assert_eq!(depths(&f)[2], ("again".to_string(), 2));
}

#[test]
fn link_threads_hangs_the_tagged_messages_under_the_cursor() {
    let mut f = threaded();
    // Needs something tagged, and a Message-ID to point at.
    f.select("root");
    f.session.link_threads();
    assert_eq!(f.log.last_text(), "first tag the message(s) to link");

    f.tag("loner");
    f.session.link_threads();
    assert_eq!(f.log.last_text(), "1 linked");
    assert!(!f.is_tagged("loner"), "mutt untags what it linked");
    let text = fs::read_to_string(f.path_of("loner")).unwrap();
    assert!(text.contains("In-Reply-To: <m0@example.com>"), "{text}");
    assert_eq!(
        depths(&f),
        [("root", 0), ("reply", 1), ("again", 2), ("loner", 1)].map(|(s, d)| (s.to_string(), d))
    );
    assert_eq!(
        f.subjects()[f.session.sel],
        "root",
        "the cursor stays on the parent"
    );

    // One step back: the tag and the file.
    f.session.undo_last();
    assert!(f.is_tagged("loner"));
    assert_eq!(depths(&f)[3], ("loner".to_string(), 0));

    // Without thread sort, as in mutt, they refuse.
    f.session.sort = SortKey::Date;
    f.session.apply_sort();
    f.session.link_threads();
    assert_eq!(
        f.log.last_text(),
        "thread operations need thread sort (o t)"
    );
}

#[test]
fn read_thread_and_parent_message() {
    let mut f = threaded();
    for m in &mut f.session.msgs {
        m.env.file.flags.seen = false;
    }
    // Ctrl+R on the reply: its whole thread, three messages.
    f.select("reply");
    f.session.thread_mark(false, crate::ThreadOp::Read);
    assert_eq!(f.log.last_text(), "3 marked read");
    let unread = |f: &Fixture| {
        f.session
            .msgs
            .iter()
            .filter(|m| !m.env.file.flags.seen)
            .map(|m| m.env.subject.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(unread(&f), ["loner"]);
    f.session.undo_last();
    assert_eq!(unread(&f).len(), 4);
    // Esc r: the message and its replies only.
    f.select("reply");
    f.session.thread_mark(true, crate::ThreadOp::Read);
    assert_eq!(unread(&f), ["root", "loner"]);

    // P walks up, root-message goes straight to the top.
    f.select("again");
    f.session.jump_parent(false);
    assert_eq!(f.subjects()[f.session.sel], "reply");
    f.session.jump_parent(false);
    assert_eq!(f.subjects()[f.session.sel], "root");
    f.session.jump_parent(false);
    assert_eq!(f.log.last_text(), "no parent message");
    f.select("again");
    f.session.jump_parent(true);
    assert_eq!(f.subjects()[f.session.sel], "root");
}

#[test]
fn thread_patterns_see_the_whole_thread() {
    let mut f = threaded();
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~(~s again)");
    assert_eq!(f.subjects(), ["root", "reply", "again"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~<(~s root)");
    assert_eq!(f.subjects(), ["reply"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~>(~s again)");
    assert_eq!(f.subjects(), ["reply"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~$");
    assert_eq!(f.subjects(), ["loner"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "");
    // ~v: a folded thread's head, a thread of one included.
    f.session.toggle_collapse(true);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~v");
    assert_eq!(f.subjects(), ["root", "loner"]);
}

#[test]
fn reply_regexp_strips_the_prefix_it_names() {
    let mut config = reply_config();
    config.mail.reply_regexp = Some(r"^(re|aw):[ \t]*".into());
    let mut f = Fixture::with_config(&[], config);
    write_message(f._dir.path(), 0, "AW: Lunch", None);
    write_message(f._dir.path(), 1, "Re[2]: Dinner", None);
    f.session.rescan();
    assert_eq!(f.session.reply_subject("AW: Lunch"), "Re: Lunch");
    // Not in this regex, so it stacks, as mutt's would.
    assert_eq!(
        f.session.reply_subject("Re[2]: Dinner"),
        "Re: Re[2]: Dinner"
    );
    f.session.config.mail.reply_regexp = None;
    f.session.recompile();
    assert_eq!(f.session.reply_subject("Re[2]: Dinner"), "Re: Dinner");
}

#[test]
fn edit_label_writes_x_label_and_sorts_by_it() {
    let mut f = Fixture::new(&["apple", "cherry", "banana"]);
    // Label two of them; the third stays unlabelled.
    f.select("cherry");
    f.session.edit_label("zeta", false);
    assert_eq!(f.log.last_text(), "labelled 1 message(s)");
    f.select("apple");
    f.session.edit_label("alpha", false);

    // The header reached the file, and re-parsing sees it.
    let text = fs::read_to_string(f.path_of("apple")).unwrap();
    assert!(text.contains("X-Label: alpha"), "{text}");
    assert_eq!(
        f.session
            .msgs
            .iter()
            .find(|m| m.env.subject == "apple")
            .unwrap()
            .env
            .label
            .as_deref(),
        Some("alpha")
    );

    // sort=label: alpha, zeta, then the unlabelled one last.
    f.session.sort = SortKey::Label;
    f.session.apply_sort();
    assert_eq!(f.subjects(), ["apple", "cherry", "banana"]);

    // ~y finds a label, %y is available through the format.
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~y alpha");
    assert_eq!(f.subjects(), ["apple"]);
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "");

    // Clearing it: empty value drops the header.
    f.select("apple");
    f.session.edit_label("", false);
    assert_eq!(f.log.last_text(), "label cleared on 1 message(s)");
    let text = fs::read_to_string(f.path_of("apple")).unwrap();
    assert!(!text.contains("X-Label"), "{text}");

    // z puts the header back.
    f.session.undo_last();
    assert!(
        fs::read_to_string(f.path_of("apple"))
            .unwrap()
            .contains("X-Label: alpha")
    );
}

#[test]
fn the_read_only_pattern_table() {
    // A mailbox where read/replied/old and label/subscribed all
    // differ, so each term picks out its own.
    let mut f = Fixture::new(&[]);
    let d = f._dir.path();
    // read (cur, S): apple; unread old (cur, no S): pear;
    // new (new/): plum; replied (cur, RS): quince.
    fs::write(
        d.join("cur").join("0000.rmut:2,S"),
        "From: a@x\nTo: me@example.com\nSubject: apple\nDate: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <a@x>\n\nbody\n",
    )
    .unwrap();
    fs::write(
        d.join("cur").join("0001.rmut:2,"),
        "From: a@x\nTo: me@example.com\nSubject: pear\nDate: Mon, 11 Mar 2024 10:00:00 +0000\nMessage-ID: <p@x>\n\nbody\n",
    )
    .unwrap();
    fs::write(
        d.join("new").join("plum.rmut"),
        "From: a@x\nSubject: plum\nDate: Mon, 12 Mar 2024 10:00:00 +0000\nMessage-ID: <pl@x>\n\nbody\n",
    )
    .unwrap();
    fs::write(
        d.join("cur").join("0003.rmut:2,RS"),
        "From: a@x\nCc: list@team.example.com\nSubject: quince\nDate: Mon, 13 Mar 2024 10:00:00 +0000\nMessage-ID: <q@x>\n\nbody\n",
    )
    .unwrap();
    touch_dirs(f._dir.path());
    f.session.check_new_mail();

    let limit = |f: &mut Fixture, pat: &str| {
        let ask = Some(f.session.ask_limit());
        f.answer_line(ask, pat);
        let mut s = f.subjects();
        s.sort();
        let ask = Some(f.session.ask_limit());
        f.answer_line(ask, "");
        s
    };
    assert_eq!(limit(&mut f, "~R"), ["apple", "quince"]);
    assert_eq!(limit(&mut f, "~O"), ["pear"]);
    assert_eq!(limit(&mut f, "~Q"), ["quince"]);
    // ~B reads the whole message; the body of apple holds "body".
    assert_eq!(limit(&mut f, "~B quince"), ["quince"]);
    // ~L is from-or-to; a@x is the sender of all four.
    assert_eq!(limit(&mut f, "~L a@x").len(), 4);

    // ~u needs a subscribed list.
    f.session.config.mail.subscribed = vec!["team\\.example\\.com".into()];
    f.session.recompile();
    assert_eq!(limit(&mut f, "~u"), ["quince"]);

    // The refusals name themselves rather than reading as unknown.
    let ask = Some(f.session.ask_limit());
    f.answer_line(ask, "~g");
    assert_eq!(f.log.last_text(), "bad pattern: ~g is not supported");
}

#[test]
fn decode_save_writes_the_decoded_message() {
    let mut f = Fixture::new(&[]);
    let d = f._dir.path().to_path_buf();
    // A base64 text/plain message: the raw body is not readable text.
    fs::write(
        d.join("cur").join("0000.rmut:2,S"),
        "From: Jane <jane@example.com>\nTo: me@example.com\nSubject: encoded\n         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <e@x>\n         MIME-Version: 1.0\nContent-Type: text/plain\nContent-Transfer-Encoding: base64\n\n         aGVsbG8gd29ybGQK\n",
    )
    .unwrap();
    touch_dirs(&d);
    f.session.check_new_mail();

    let out = d.join("decoded");
    // decode-save: the delivered copy holds the decoded body.
    let ask = f.session.ask_copy_decode(true, false, true);
    f.answer_line(ask, out.to_str().unwrap());
    assert!(
        f.log.last_text().starts_with("saved to"),
        "{}",
        f.log.last_text()
    );
    let copy_path = fs::read_dir(out.join("cur"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let body = fs::read_to_string(&copy_path).unwrap();
    assert!(body.contains("hello world"), "decoded: {body}");
    assert!(!body.contains("aGVsbG8"), "still base64: {body}");
    assert!(
        body.contains("Subject: encoded"),
        "weeded headers kept: {body}"
    );
    // The original was marked deleted, and z brings it back.
    assert!(f.session.msgs[0].env.file.flags.deleted);
    f.session.undo_last();
    assert!(!f.session.msgs[0].env.file.flags.deleted);
    assert!(!copy_path.exists(), "undo removed the copy");

    // A plain copy keeps the raw base64.
    let out2 = d.join("raw");
    let ask = f.session.ask_copy(false, false);
    f.answer_line(ask, out2.to_str().unwrap());
    let raw = fs::read_to_string(
        fs::read_dir(out2.join("cur"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(raw.contains("aGVsbG8"), "copy stays raw: {raw}");
}

#[test]
fn pipe_split_runs_the_command_per_message() {
    let mut f = Fixture::new(&["one", "two", "three"]);
    for m in &mut f.session.msgs {
        m.env.tagged = true;
    }
    let count = f._dir.path().join("count");
    let cmd = format!("printf x >> {}", count.display());
    // pipe_split off (default): one run over all three.
    f.session.pipe_message(&cmd, true);
    assert_eq!(fs::read_to_string(&count).unwrap(), "x");
    fs::remove_file(&count).unwrap();
    // pipe_split on: one run each.
    f.session.config.mail.pipe_split = Some(true);
    f.session.pipe_message(&cmd, true);
    assert_eq!(fs::read_to_string(&count).unwrap(), "xxx");
}

#[test]
fn sort_by_to_and_unsorted() {
    let mut f = Fixture::new(&[]);
    let d = f._dir.path().to_path_buf();
    // Three messages addressed to different people, written in a
    // fixed file order so "unsorted" is predictable.
    let write = |name: &str, to: &str, subj: &str| {
        fs::write(
            d.join("cur").join(format!("{name}:2,S")),
            format!("From: s@x\nTo: {to}\nSubject: {subj}\nDate: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <{name}@x>\n\nbody\n"),
        )
        .unwrap();
    };
    write("0001", "zoe@example.com", "first");
    write("0002", "amy@example.com", "second");
    write("0003", "mike@example.com", "third");
    touch_dirs(&d);
    f.session.check_new_mail();

    f.session.sort = SortKey::To;
    f.session.apply_sort();
    assert_eq!(f.subjects(), ["second", "third", "first"]); // amy, mike, zoe

    // Unsorted: the on-disk (file name) order.
    f.session.sort = SortKey::Unsorted;
    f.session.apply_sort();
    assert_eq!(f.subjects(), ["first", "second", "third"]);

    // The sort menu keys reach them (o = to, u = unsorted).
    let ask = Some(f.session.ask_sort());
    f.answer_key(ask, 'o');
    assert_eq!(f.session.sort, SortKey::To);
    let ask = Some(f.session.ask_sort());
    f.answer_key(ask, 'u');
    assert_eq!(f.session.sort, SortKey::Unsorted);
}

#[test]
fn postpone_quadoption_decides_without_asking() {
    // Stage a draft the way the compose flow does, then leave it.
    let stage = |postpone: &str| -> Fixture {
        let mut config = reply_config();
        config.mail.postpone = Some(postpone.into());
        let mut f = Fixture::with_config(&["one"], config);
        let ask = f.session.start_compose(crate::ComposeKind::New);
        let ask = f.answer_line(ask, "someone@example.com");
        let _ = f.answer_line(ask, "a subject");
        let draft = draft_from_requests(&mut f.session);
        // Make the file look edited, or abort_unmodified drops it.
        let text = fs::read_to_string(&draft.path).unwrap();
        fs::write(&draft.path, format!("{text}a line of my own\n")).unwrap();
        f.session.set_draft(draft);
        assert!(f.session.draft().is_some());
        f
    };

    // no: discard without a question.
    let mut f = stage("no");
    assert!(f.session.ask_postpone().is_none(), "no question");
    assert_eq!(f.log.last_text(), "message discarded");
    assert!(!f.session.has_postponed(), "nothing was filed");

    // yes: postpone without a question.
    let mut f = stage("yes");
    assert!(f.session.ask_postpone().is_none());
    assert!(f.session.has_postponed(), "the draft was filed");

    // ask-no: a question whose Enter discards.
    let mut f = stage("ask-no");
    let ask = f.session.ask_postpone();
    assert_eq!(
        ask_label(ask.as_ref().unwrap()),
        "Postpone this message? (y/n): "
    );
    assert!(f.answer_enter(ask).is_none());
    assert!(!f.session.has_postponed(), "Enter discarded it");

    // ask-yes (the default): Enter postpones.
    let mut f = stage("ask-yes");
    let ask = f.session.ask_postpone();
    assert!(f.answer_enter(ask).is_none());
    assert!(f.session.has_postponed(), "Enter filed it");
}
