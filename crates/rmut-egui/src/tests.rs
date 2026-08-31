//! The window's logic off the display: a maildir fixture, keys
//! through the same path the frame uses, and a kittest layout pass
//! for the painting.

use std::fs;
use std::path::Path;

use rmut_core::config::Config;
use rmut_front::{KeyCode, KeyEvent, KeyModifiers};
use rmut_session::Session;

use crate::app::{Gui, Mode};

fn write_message(dir: &Path, i: usize, subject: &str) {
    let day = 10 + i;
    let text = format!(
        "From: Sender {i} <s{i}@example.com>\n\
         To: jane@example.com\n\
         Subject: {subject}\n\
         Date: Mon, {day} Mar 2024 10:00:00 +0000\n\
         Message-ID: <m{i}@example.com>\n\
         \n\
         body of {subject}\n"
    );
    fs::write(dir.join("cur").join(format!("{i:04}.rmut:2,")), text).unwrap();
}

fn fixture(subjects: &[&str]) -> (tempfile::TempDir, Gui) {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    for (i, subject) in subjects.iter().enumerate() {
        write_message(dir.path(), i, subject);
    }
    let (session, warnings) = Session::open(dir.path(), Config::default()).unwrap();
    assert_eq!(warnings, Vec::<String>::new(), "clean open");
    (dir, Gui::new(session, Vec::new(), true))
}

fn press(gui: &mut Gui, keys: &str) {
    let events = keys
        .chars()
        .map(|c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
        .collect();
    gui.handle_keys(events);
}

fn key(gui: &mut Gui, code: KeyCode) {
    gui.handle_keys(vec![KeyEvent::new(code, KeyModifiers::NONE)]);
}

#[test]
fn index_keys_move_and_open_the_pager() {
    let (_dir, mut gui) = fixture(&["one", "two", "three"]);
    assert_eq!(gui.session.visible.len(), 3);
    press(&mut gui, "jj");
    assert_eq!(gui.session.sel, 2);
    press(&mut gui, "k");
    assert_eq!(gui.session.sel, 1);
    key(&mut gui, KeyCode::Enter);
    let Mode::Pager(pager) = &gui.mode else {
        panic!("Enter opens the pager");
    };
    assert!(
        pager.view.body.contains("body of two"),
        "{}",
        pager.view.body
    );
    press(&mut gui, "q");
    assert!(matches!(gui.mode, Mode::Index), "q returns to the index");
}

#[test]
fn the_window_never_writes() {
    let (_dir, mut gui) = fixture(&["one"]);
    assert!(gui.session.read_only && gui.session.read_only_session);
    press(&mut gui, "d");
    let notice = gui.notice().expect("d complains");
    assert!(notice.text().contains("read-only"), "{}", notice.text());
}

#[test]
fn limit_asks_and_narrows() {
    let (_dir, mut gui) = fixture(&["alpha", "beta", "gamma"]);
    press(&mut gui, "l~s beta");
    key(&mut gui, KeyCode::Enter);
    assert_eq!(gui.session.visible.len(), 1, "the limit narrowed");
    // l prefills the standing limit, the way mutt does; Ctrl+U
    // clears the line and an empty answer clears the limit.
    press(&mut gui, "l");
    gui.handle_keys(vec![KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )]);
    key(&mut gui, KeyCode::Enter);
    assert_eq!(gui.session.visible.len(), 3, "an empty limit clears");
}

#[test]
fn help_and_folders_are_screens_of_their_own() {
    let (_dir, mut gui) = fixture(&["one"]);
    press(&mut gui, "?");
    assert!(matches!(gui.mode, Mode::Help { .. }));
    press(&mut gui, "q");
    press(&mut gui, "y");
    assert!(matches!(gui.mode, Mode::Folders { .. }));
    press(&mut gui, "q");
    assert!(matches!(gui.mode, Mode::Index));
}

#[test]
fn sending_is_refused_with_a_pointer_at_rmut() {
    let (_dir, mut gui) = fixture(&["one"]);
    press(&mut gui, "m");
    let notice = gui.notice().expect("m says its round");
    assert!(
        notice.text().contains("not in the GUI yet"),
        "{}",
        notice.text()
    );
}

#[test]
fn a_frame_lays_out_without_a_display() {
    let (_dir, gui) = fixture(&["one", "two"]);
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.run();
    // The pager lays out too.
    harness
        .state_mut()
        .handle_keys(vec![KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)]);
    harness.run();
    assert!(matches!(harness.state().mode, Mode::Pager(_)));
}
