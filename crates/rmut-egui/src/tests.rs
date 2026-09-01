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
    (dir, Gui::new(session, Vec::new(), false))
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
fn minus_r_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "one");
    let (session, _) = Session::open(dir.path(), Config::default()).unwrap();
    let mut gui = Gui::new(session, Vec::new(), true);
    assert!(gui.session.read_only && gui.session.read_only_session);
    press(&mut gui, "d");
    let notice = gui.notice().expect("d complains");
    assert!(notice.text().contains("read-only"), "{}", notice.text());
}

#[test]
fn the_window_writes_now() {
    let (_dir, mut gui) = fixture(&["one", "two", "three"]);
    gui.session.sel = 0;
    press(&mut gui, "d");
    assert!(
        gui.session.msgs[gui.session.visible[0]]
            .env
            .file
            .flags
            .deleted
    );
    assert_eq!(gui.session.sel, 1, "delete advanced");
    press(&mut gui, "z");
    assert!(
        !gui.session.msgs[gui.session.visible[0]]
            .env
            .file
            .flags
            .deleted,
        "undo took the mark back"
    );
    assert_eq!(gui.session.sel, 0, "undo restored the selection too");
    // The pager's own marks work too.
    key(&mut gui, KeyCode::Enter);
    press(&mut gui, "F");
    let i = gui.session.visible[gui.session.sel];
    assert!(
        gui.session.msgs[i].env.file.flags.flagged,
        "F in the pager flags"
    );
    press(&mut gui, "d");
    assert!(gui.session.msgs[i].env.file.flags.deleted);
    let Mode::Pager(pager) = &gui.mode else {
        panic!("delete in the pager opened the next message");
    };
    assert!(
        pager.view.body.contains("body of two"),
        "{}",
        pager.view.body
    );
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

#[test]
fn gui_config_parses_size_and_font() {
    let config: Config =
        toml::from_str("[gui]\nsize = 18.0\nfont = \"~/fonts/mono.ttf\"\n").unwrap();
    assert_eq!(config.gui.size, Some(18.0));
    assert_eq!(config.gui.font.as_deref(), Some("~/fonts/mono.ttf"));
    let bare: Config = toml::from_str("").unwrap();
    assert_eq!(
        bare.gui.size, None,
        "unset stays unset; the window defaults to 14"
    );
}

#[test]
fn attachments_list_view_and_save() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let text = "From: jane@example.com\nTo: sam@example.com\nSubject: parts\n\
         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <p1@example.com>\n\
         MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"b\"\n\n\
         --b\nContent-Type: text/plain\n\nhello body\n\
         --b\nContent-Type: application/octet-stream; name=\"blob.bin\"\n\
         Content-Disposition: attachment; filename=\"blob.bin\"\n\
         Content-Transfer-Encoding: base64\n\nAAEC\n--b--\n";
    fs::write(dir.path().join("cur").join("0001.x:2,S"), text).unwrap();
    let (session, warnings) = Session::open(dir.path(), Config::default()).unwrap();
    assert_eq!(warnings, Vec::<String>::new());
    let mut gui = Gui::new(session, Vec::new(), false);
    press(&mut gui, "v");
    let Mode::Attach { parts, .. } = &gui.mode else {
        panic!("v opens the attachment menu");
    };
    assert_eq!(parts.len(), 2, "two leaves listed");
    // Enter views the text part in a part pager; q returns to the menu.
    key(&mut gui, KeyCode::Enter);
    let Mode::Pager(pager) = &gui.mode else {
        panic!("Enter views the part")
    };
    assert!(pager.back.is_some(), "a part pager knows its way back");
    assert!(pager.view.body.contains("hello body"));
    press(&mut gui, "q");
    assert!(matches!(gui.mode, Mode::Attach { .. }));
    // s saves the selected (binary) part under its own name.
    press(&mut gui, "j");
    press(&mut gui, "s");
    gui.handle_keys(vec![KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )]);
    let target = dir.path().join("blob.out");
    press(&mut gui, target.to_str().unwrap());
    key(&mut gui, KeyCode::Enter);
    assert_eq!(
        fs::read(&target).unwrap(),
        [0u8, 1, 2],
        "decoded base64 bytes"
    );
    press(&mut gui, "q");
    assert!(matches!(gui.mode, Mode::Index));
}

#[test]
fn pager_search_finds_and_toggles() {
    let (_dir, mut gui) = fixture(&["one", "two"]);
    gui.session.sel = 0;
    key(&mut gui, KeyCode::Enter);
    press(&mut gui, "/body");
    key(&mut gui, KeyCode::Enter);
    assert!(gui.pager_search.is_some(), "the search compiled");
    // The only hit is the body line; the next step wraps and says so.
    press(&mut gui, "n");
    let notice = gui.notice().expect("n wraps with a note");
    assert!(notice.text().contains("wrapped"), "{}", notice.text());
    press(&mut gui, "\\");
    assert!(gui.pager_search_off, "backslash hides the highlighting");
    // A missing pattern says so.
    press(&mut gui, "/");
    gui.handle_keys(vec![KeyEvent::new(
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    )]);
    key(&mut gui, KeyCode::Enter);
    // The old pattern survives an empty answer, like mutt's prefill.
    assert!(gui.pager_search.is_some());
}

#[test]
fn tab_at_an_empty_mailbox_prompt_opens_the_browser() {
    let (_dir, mut gui) = fixture(&["one"]);
    press(&mut gui, "c");
    assert!(matches!(gui.prompt, Some(crate::app::Prompt::Line { .. })));
    key(&mut gui, KeyCode::Tab);
    assert!(gui.prompt.is_none(), "the prompt made way");
    assert!(
        matches!(gui.mode, Mode::Folders { .. }),
        "Tab with nothing typed opens the folder browser"
    );
}

#[test]
fn compose_asks_before_any_editor() {
    let (_dir, mut gui) = fixture(&["one"]);
    press(&mut gui, "m");
    let Some(crate::app::Prompt::Line { label, .. }) = &gui.prompt else {
        panic!("m asks for the recipients");
    };
    assert!(label.contains("To"), "{label}");
    key(&mut gui, KeyCode::Esc);
    assert!(gui.prompt.is_none(), "Esc calls the compose off");
    assert!(gui.session.draft().is_none(), "no draft was staged");
    // Recalling with nothing postponed says so instead of spawning.
    gui.handle_keys(vec![]);
    press(&mut gui, "m");
    key(&mut gui, KeyCode::Esc);
}

#[test]
fn the_window_wears_its_own_palette() {
    let config: Config = toml::from_str(
        "[colors]\ndeleted = \"blue\"\n[gui]\nbackground = \"#202030\"\n[gui.colors]\ndeleted = \"#ff8000\"\n",
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "one");
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let gui = Gui::new(session, Vec::new(), false);
    use rmut_front::style::Color;
    assert_eq!(
        gui.theme.deleted,
        Color::Rgb(255, 128, 0),
        "[gui.colors] wins in the window"
    );
    assert_eq!(
        gui.session.config.colors.get("deleted").map(String::as_str),
        Some("blue"),
        "the shared [colors] stays as written for the terminal"
    );
}

#[test]
fn the_about_overlay_opens_and_esc_closes() {
    let (_dir, mut gui) = fixture(&["one", "two"]);
    gui.session.sel = 0;
    gui.about = true;
    press(&mut gui, "j");
    assert_eq!(gui.session.sel, 0, "keys stay out while it is up");
    key(&mut gui, KeyCode::Esc);
    assert!(!gui.about, "Esc closes the overlay");
    press(&mut gui, "j");
    assert_eq!(gui.session.sel, 1, "and the keys are back");
}

#[test]
fn prefs_apply_to_the_live_config() {
    let (_dir, mut gui) = fixture(&["one"]);
    let mut prefs = crate::app::Prefs::from_config(&gui.session.config);
    assert_eq!(prefs.size, 14.0, "the dialog opens on the defaults");
    prefs.size = 18.0;
    prefs.terminal = "foot".into();
    prefs.background = eframe::egui::Color32::from_rgb(0x20, 0x20, 0x30);
    gui.prefs = Some(prefs);
    gui.apply_prefs(&eframe::egui::Context::default());
    let cfg = &gui.session.config.gui;
    assert_eq!(cfg.size, Some(18.0));
    assert_eq!(cfg.terminal.as_deref(), Some("foot"));
    assert_eq!(cfg.background.as_deref(), Some("#202030"));
    // Esc closes the dialog and keys stay out while it is up.
    let sel = gui.session.sel;
    press(&mut gui, "j");
    assert_eq!(gui.session.sel, sel, "keys wait under the dialog");
    key(&mut gui, KeyCode::Esc);
    assert!(gui.prefs.is_none());
}

#[test]
fn proportional_parses_and_rides_the_overlay() {
    let config: Config = toml::from_str("[gui]\nproportional = true\n").unwrap();
    assert_eq!(config.gui.proportional, Some(true));
    let bare: Config = toml::from_str("").unwrap();
    assert_eq!(bare.gui.proportional, None, "unset means monospace");
}
