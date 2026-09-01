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

/// MAILCAPS is process-global: every test that sets it holds this
/// lock for its whole body, so two never race.
static MAILCAPS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mailcaps_guard() -> std::sync::MutexGuard<'static, ()> {
    MAILCAPS.lock().unwrap_or_else(|e| e.into_inner())
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
fn pager_scrollbar_jumps_the_view() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let mut text = String::from(
        "From: jane@example.com\nTo: sam@example.com\nSubject: long\n\
         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <l1@example.com>\n\n",
    );
    for i in 0..200 {
        text += &format!("line {i}\n");
    }
    fs::write(dir.path().join("cur").join("0001.x:2,S"), text).unwrap();
    let (session, _) = Session::open(dir.path(), Config::default()).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    key(&mut gui, KeyCode::Enter);
    assert!(matches!(gui.mode, Mode::Pager(_)));
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.set_size(eframe::egui::Vec2::new(400.0, 300.0));
    harness.run();
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!("still paging");
    };
    assert_eq!(pager.scroll, 0);
    // A click two thirds down the right-edge scrollbar jumps there.
    let pos = eframe::egui::pos2(397.0, 200.0);
    harness.event(eframe::egui::Event::PointerButton {
        pos,
        button: eframe::egui::PointerButton::Primary,
        pressed: true,
        modifiers: eframe::egui::Modifiers::NONE,
    });
    harness.run();
    harness.event(eframe::egui::Event::PointerButton {
        pos,
        button: eframe::egui::PointerButton::Primary,
        pressed: false,
        modifiers: eframe::egui::Modifiers::NONE,
    });
    harness.run();
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!("still paging");
    };
    assert!(pager.scroll > 50, "the view jumped: {}", pager.scroll);
}

#[test]
fn pager_decodes_image_parts_for_inlining() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let text = "From: jane@example.com\nTo: sam@example.com\nSubject: picture\n\
         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <i1@example.com>\n\
         MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"b\"\n\n\
         --b\nContent-Type: text/plain\n\nsee the picture\n\
         --b\nContent-Type: image/png; name=\"pic.png\"\n\
         Content-Disposition: attachment; filename=\"pic.png\"\n\
         Content-Transfer-Encoding: base64\n\nAAEC\n--b--\n";
    fs::write(dir.path().join("cur").join("0001.x:2,S"), text).unwrap();
    let (session, _) = Session::open(dir.path(), Config::default()).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    key(&mut gui, KeyCode::Enter);
    let Mode::Pager(pager) = &gui.mode else {
        panic!("Enter opens the pager");
    };
    assert!(
        pager.view.body.contains("[-- Type: image/png"),
        "{}",
        pager.view.body
    );
    // The first image marker maps to the decoded png leaf; there is
    // no second image.
    let (uri, bytes) = gui.pager_image(0).expect("the png leaf decodes");
    assert!(uri.contains("#1"), "{uri}");
    assert_eq!(&bytes[..], &[0u8, 1, 2], "decoded base64 bytes");
    assert!(gui.pager_image(1).is_none());
    // The frame lays out with the image row on screen, and the
    // config key can turn the drawing off.
    let off: Config = toml::from_str("[gui]\ninline_images = false\n").unwrap();
    assert_eq!(off.gui.inline_images, Some(false));
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.run();
    assert!(
        harness.state().pager_drew_images,
        "the marker drew its image"
    );
}

#[test]
fn attach_menu_views_through_mailcap_and_as_text() {
    let _guard = mailcaps_guard();
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    // "aGVsbG8gbWFpbGNhcA==" is base64 for "hello mailcap".
    let text = "From: jane@example.com\nTo: sam@example.com\nSubject: parts\n\
         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <p2@example.com>\n\
         MIME-Version: 1.0\nContent-Type: multipart/mixed; boundary=\"b\"\n\n\
         --b\nContent-Type: text/plain\n\nhello body\n\
         --b\nContent-Type: application/octet-stream; name=\"blob.bin\"\n\
         Content-Disposition: attachment; filename=\"blob.bin\"\n\
         Content-Transfer-Encoding: base64\n\naGVsbG8gbWFpbGNhcA==\n--b--\n";
    fs::write(dir.path().join("cur").join("0001.x:2,S"), text).unwrap();
    let mailcap = dir.path().join("mailcap");
    fs::write(
        &mailcap,
        "application/octet-stream; cat %s; copiousoutput\n",
    )
    .unwrap();
    unsafe { std::env::set_var("MAILCAPS", &mailcap) };
    let (session, _) = Session::open(dir.path(), Config::default()).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    press(&mut gui, "vj");
    // T: the bytes as text, whatever the type claims.
    press(&mut gui, "T");
    let Mode::Pager(pager) = &gui.mode else {
        panic!("T views the part as text");
    };
    assert!(
        pager.view.body.contains("hello mailcap"),
        "{}",
        pager.view.body
    );
    press(&mut gui, "q");
    assert!(matches!(gui.mode, Mode::Attach { .. }), "back to the menu");
    // m: the mailcap viewer; copiousoutput lands in a part pager.
    press(&mut gui, "m");
    let Mode::Pager(pager) = &gui.mode else {
        panic!("m views through mailcap");
    };
    assert!(
        pager.view.body.contains("hello mailcap"),
        "{}",
        pager.view.body
    );
    press(&mut gui, "q");
    // Enter on a type that is not text and has no [filters] entry
    // also goes through mailcap (mutt's view-attach).
    key(&mut gui, KeyCode::Enter);
    let Mode::Pager(pager) = &gui.mode else {
        panic!("Enter falls through to mailcap");
    };
    assert!(
        pager.view.body.contains("hello mailcap"),
        "{}",
        pager.view.body
    );
    press(&mut gui, "q");
    // With no matching entry, mutt says so and shows text.
    fs::write(&mailcap, "video/mp4; mpv %s\n").unwrap();
    key(&mut gui, KeyCode::Enter);
    let notice = gui.notice().expect("the fallback is said");
    assert!(
        notice.text().contains("viewing as text"),
        "{}",
        notice.text()
    );
    let Mode::Pager(pager) = &gui.mode else {
        panic!("the bytes still show as text");
    };
    assert!(
        pager.view.body.contains("hello mailcap"),
        "{}",
        pager.view.body
    );
    press(&mut gui, "qq");
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

#[test]
fn each_terminal_gets_its_own_calling_convention() {
    use crate::app::terminal_invocation;
    assert_eq!(terminal_invocation("gnome-terminal"), ["--wait", "--"]);
    assert_eq!(
        terminal_invocation("/usr/bin/gnome-terminal"),
        ["--wait", "--"]
    );
    assert_eq!(terminal_invocation("kitty"), [] as [&str; 0]);
    assert_eq!(terminal_invocation("terminator"), ["-x"]);
    assert_eq!(
        terminal_invocation("xfce4-terminal"),
        ["--disable-server", "-x"]
    );
    assert_eq!(terminal_invocation("foot"), ["-e"]);
    assert_eq!(terminal_invocation("x-terminal-emulator"), ["-e"]);
}

#[test]
fn the_builtin_editor_carries_the_compose_flow() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "one");
    let config: Config = toml::from_str("[gui]\neditor = \"builtin\"\n").unwrap();
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    press(&mut gui, "m");
    press(&mut gui, "jane@example.com");
    key(&mut gui, KeyCode::Enter);
    press(&mut gui, "hello");
    key(&mut gui, KeyCode::Enter);
    let Mode::Edit { text, .. } = &mut gui.mode else {
        panic!("the staged draft opened in the built-in editor");
    };
    text.push_str("typed in the window\n");
    // Keymap keys stay out of the editor.
    press(&mut gui, "q");
    assert!(
        matches!(gui.mode, Mode::Edit { .. }),
        "q is just a letter here"
    );
    gui.finish_edit(true);
    assert!(
        matches!(gui.mode, Mode::Compose { .. }),
        "Done lands on the compose menu"
    );
    // Enter previews the body; q returns to the compose menu, not
    // the index - the draft stays reachable.
    key(&mut gui, KeyCode::Enter);
    assert!(
        matches!(gui.mode, Mode::Help { .. }),
        "Enter previews the entry"
    );
    press(&mut gui, "q");
    assert!(
        matches!(gui.mode, Mode::Compose { .. }),
        "q resumes the send flow"
    );
    let draft = gui.session.draft().expect("the draft is set");
    let written = fs::read_to_string(&draft.path).unwrap();
    assert!(written.contains("typed in the window"), "{written}");
    // q from the menu asks about postponing; Esc keeps the draft.
    press(&mut gui, "q");
}

#[test]
fn compose_menu_views_an_attachment_as_text_and_through_mailcap() {
    let _guard = mailcaps_guard();
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "one");
    let attachment = dir.path().join("notes.txt");
    fs::write(&attachment, "the attached words\n").unwrap();
    let mailcap = dir.path().join("mailcap");
    fs::write(&mailcap, "text/plain; cat %s; copiousoutput\n").unwrap();
    unsafe { std::env::set_var("MAILCAPS", &mailcap) };
    let config: Config = toml::from_str("[gui]\neditor = \"builtin\"\n").unwrap();
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    press(&mut gui, "m");
    press(&mut gui, "jane@example.com");
    key(&mut gui, KeyCode::Enter);
    press(&mut gui, "hello");
    key(&mut gui, KeyCode::Enter);
    let Mode::Edit { text, .. } = &mut gui.mode else {
        panic!("the draft opened in the built-in editor");
    };
    // An unmodified draft aborts (mutt's $abort_unmodified).
    text.push_str("body\n");
    gui.finish_edit(true);
    assert!(matches!(gui.mode, Mode::Compose { .. }));
    press(&mut gui, "a");
    press(&mut gui, attachment.to_str().unwrap());
    key(&mut gui, KeyCode::Enter);
    press(&mut gui, "j");
    let Mode::Compose { sel } = gui.mode else {
        panic!("still on the compose menu");
    };
    assert_eq!(sel, 1, "j lands on the attachment row");
    // Esc v: the bytes as text, whatever the type.
    gui.handle_keys(vec![KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT)]);
    let Mode::Help { lines, .. } = &gui.mode else {
        panic!("Esc v shows the file as text");
    };
    assert!(
        lines.iter().any(|l| l.contains("the attached words")),
        "{lines:?}"
    );
    press(&mut gui, "q");
    // V: the mailcap viewer; copiousoutput lands in the window. The
    // return from the view put the cursor back on the body row.
    press(&mut gui, "jV");
    let Mode::Help { lines, .. } = &gui.mode else {
        panic!("V shows the copiousoutput viewer's text");
    };
    assert!(
        lines.iter().any(|l| l.contains("the attached words")),
        "{lines:?}"
    );
    press(&mut gui, "q");
    press(&mut gui, "q");
    key(&mut gui, KeyCode::Esc);
}

#[test]
fn real_nvim_round_trips_a_file() {
    // The embedding, end to end against the real binary: no display,
    // just RPC. Skipped quietly where nvim is not installed.
    if std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("draft.txt");
    fs::write(&file, "before\n").unwrap();
    let mut nvim = crate::nvim::Embedded::start(&file, 60, 15, || {}).unwrap();
    nvim.input("ohello from the grid<Esc>:wq<CR>");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !nvim.finished && std::time::Instant::now() < deadline {
        nvim.pump();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(nvim.finished, "nvim exited on :wq");
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("hello from the grid"), "{text}");
    assert!(text.starts_with("before"), "{text}");
}

#[test]
fn an_arrival_below_the_fold_scrolls_into_sight() {
    let (dir, mut gui) = fixture(&["m1", "m2", "m3", "m4", "m5"]);
    // A five-row mailbox on a three-row view, showing the tail.
    gui.index_offset = 2;
    gui.follow_tail(3);
    assert_eq!(gui.index_len, 5, "the first call only takes note");
    assert_eq!(gui.index_offset, 2);
    // A sixth message lands (the mtime bump makes the poll look).
    write_message(dir.path(), 5, "m6");
    for sub in ["new", "cur"] {
        let f = fs::File::open(dir.path().join(sub)).unwrap();
        let t = f.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(1);
        f.set_modified(t).unwrap();
    }
    gui.session.check_new_mail();
    assert_eq!(gui.session.visible.len(), 6, "the poll saw it");
    gui.follow_tail(3);
    assert_eq!(gui.index_offset, 3, "the tail view followed the growth");
    // A view scrolled up into history stays where the reader left it.
    gui.index_offset = 0;
    write_message(dir.path(), 6, "m7");
    for sub in ["new", "cur"] {
        let f = fs::File::open(dir.path().join(sub)).unwrap();
        let t = f.metadata().unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
        f.set_modified(t).unwrap();
    }
    gui.session.check_new_mail();
    gui.follow_tail(3);
    assert_eq!(
        gui.index_offset, 0,
        "history reading is not yanked to the tail"
    );
}
/// Reported 2026-09-01: with the mini-index above the pager, a
/// subject wider than the window expanded egui's region past the
/// screen edge, and the scrollbar hung on that edge - painted and
/// clickable in the void. It must stay on the visible edge.
#[test]
fn scrollbar_stays_on_screen_beside_wide_rows() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let mut text = String::from(
        "From: jane@example.com\nTo: sam@example.com\nSubject: a very very very very very very very very very very long subject line indeed truly\n\
         Date: Mon, 10 Mar 2024 10:00:00 +0000\nMessage-ID: <l2@example.com>\n\n",
    );
    for i in 0..200 {
        text += &format!("line {i}\n");
    }
    fs::write(dir.path().join("cur").join("0001.x:2,S"), text).unwrap();
    let config: Config = toml::from_str("[pager]\nindex_lines = 3\n").unwrap();
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    key(&mut gui, KeyCode::Enter);
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.set_size(eframe::egui::Vec2::new(300.0, 300.0));
    harness.run();
    let pos = eframe::egui::pos2(297.0, 200.0);
    harness.event(eframe::egui::Event::PointerButton {
        pos,
        button: eframe::egui::PointerButton::Primary,
        pressed: true,
        modifiers: eframe::egui::Modifiers::NONE,
    });
    harness.run();
    harness.event(eframe::egui::Event::PointerButton {
        pos,
        button: eframe::egui::PointerButton::Primary,
        pressed: false,
        modifiers: eframe::egui::Modifiers::NONE,
    });
    harness.run();
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!()
    };
    assert!(pager.scroll > 50, "narrow window scroll: {}", pager.scroll);
}

/// Asked 2026-09-01: the wheel moves the part it is over - the
/// mini-index with the pointer on its rows, the message below it
/// otherwise - never both at once.
#[test]
fn wheel_scrolls_only_the_hovered_region() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    for i in 0..30 {
        write_message(dir.path(), i, &format!("subject {i}"));
    }
    let mut text = String::from(
        "From: jane@example.com\nTo: sam@example.com\nSubject: zz long\n\
         Date: Mon, 20 Mar 2024 10:00:00 +0000\nMessage-ID: <w1@example.com>\n\n",
    );
    for i in 0..200 {
        text += &format!("line {i}\n");
    }
    fs::write(dir.path().join("cur").join("0100.x:2,S"), text).unwrap();
    let config: Config = toml::from_str("[pager]\nindex_lines = 3\n").unwrap();
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    // Open the long message, wherever the date sort put it.
    gui.session.sel = (0..gui.session.visible.len())
        .find(|&v| gui.session.msgs[gui.session.visible[v]].env.subject == "zz long")
        .expect("the long message is listed");
    gui.index_offset = 0;
    key(&mut gui, KeyCode::Enter);
    assert!(matches!(gui.mode, Mode::Pager(_)));
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.set_size(eframe::egui::Vec2::new(400.0, 400.0));
    harness.run();
    let offset_before = harness.state().index_offset;
    let wheel = |harness: &egui_kittest::Harness<'_, Gui>, pos: eframe::egui::Pos2| {
        harness.event(eframe::egui::Event::PointerMoved(pos));
        harness.event(eframe::egui::Event::MouseWheel {
            unit: eframe::egui::MouseWheelUnit::Point,
            delta: eframe::egui::Vec2::new(0.0, -80.0),
            phase: eframe::egui::TouchPhase::Move,
            modifiers: eframe::egui::Modifiers::NONE,
        });
    };
    // Deep in the body: the message scrolls, the list stays.
    wheel(&harness, eframe::egui::pos2(200.0, 300.0));
    harness.run();
    harness.run_steps(8);
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!()
    };
    assert!(pager.scroll > 0, "the body scrolled: {}", pager.scroll);
    assert_eq!(
        harness.state().index_offset,
        offset_before,
        "the list stayed put"
    );
    // On the mini-index rows: the list scrolls, the message stays.
    let body_scroll = pager.scroll;
    wheel(&harness, eframe::egui::pos2(200.0, 60.0));
    harness.run();
    harness.run_steps(8);
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!()
    };
    assert_eq!(pager.scroll, body_scroll, "the message stayed put");
    assert!(
        harness.state().index_offset > offset_before,
        "the list scrolled: {}",
        harness.state().index_offset
    );
}

/// Asked 2026-09-01: a click on a mini-index row shows that message,
/// the way j/k move the pager - the body below never goes stale.
#[test]
fn click_on_the_mini_index_opens_that_message() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    for (i, subject) in ["one", "two", "three"].iter().enumerate() {
        write_message(dir.path(), i, subject);
    }
    let config: Config = toml::from_str("[pager]\nindex_lines = 3\n").unwrap();
    let (session, _) = Session::open(dir.path(), config).unwrap();
    let mut gui = Gui::new(session, Vec::new(), false);
    gui.session.sel = 0;
    key(&mut gui, KeyCode::Enter);
    assert!(matches!(gui.mode, Mode::Pager(_)));
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.set_size(eframe::egui::Vec2::new(400.0, 400.0));
    harness.run();
    // The third row of the mini-index (the slice starts at ~52 with
    // ~18.3pt rows at the default size).
    let pos = eframe::egui::pos2(200.0, 95.0);
    for pressed in [true, false] {
        harness.event(eframe::egui::Event::PointerButton {
            pos,
            button: eframe::egui::PointerButton::Primary,
            pressed,
            modifiers: eframe::egui::Modifiers::NONE,
        });
        harness.run();
    }
    assert_eq!(harness.state().session.sel, 2, "the cursor moved");
    let Mode::Pager(pager) = &harness.state().mode else {
        panic!("still paging");
    };
    assert!(
        pager.view.body.contains("body of three"),
        "the clicked message shows: {}",
        pager.view.body
    );
}

/// Asked 2026-09-01: the pointer's tagging. Ctrl+click toggles the
/// tag on the clicked row (the keyboard's t, advance included);
/// Shift+click tags the run from the cursor to the click as one
/// undo step, cursor landing on the click.
#[test]
fn ctrl_and_shift_clicks_tag_rows() {
    let (_dir, mut gui) = fixture(&["one", "two", "three", "four", "five"]);
    gui.session.sel = 0;
    let mut harness = egui_kittest::Harness::new_ui_state(|ui, gui: &mut Gui| gui.frame(ui), gui);
    harness.set_size(eframe::egui::Vec2::new(400.0, 400.0));
    harness.run();
    let click = |harness: &mut egui_kittest::Harness<'_, Gui>,
                 y: f32,
                 modifiers: eframe::egui::Modifiers| {
        harness.event(eframe::egui::Event::ModifiersChanged(modifiers));
        for pressed in [true, false] {
            harness.event(eframe::egui::Event::PointerButton {
                pos: eframe::egui::pos2(200.0, y),
                button: eframe::egui::PointerButton::Primary,
                pressed,
                modifiers,
            });
            harness.run();
        }
        harness.event(eframe::egui::Event::ModifiersChanged(
            eframe::egui::Modifiers::NONE,
        ));
        harness.run();
    };
    // Ctrl+click the third row: tagged, cursor advanced past it.
    click(&mut harness, 95.0, eframe::egui::Modifiers::CTRL);
    let tagged = |gui: &Gui, vi: usize| gui.session.msgs[gui.session.visible[vi]].env.tagged;
    assert!(tagged(harness.state(), 2), "ctrl+click tagged the row");
    assert_eq!(harness.state().session.sel, 3, "t advances");
    // Shift+click the first row: the whole run back up gets tagged.
    click(&mut harness, 58.0, eframe::egui::Modifiers::SHIFT);
    for vi in 0..=3 {
        assert!(tagged(harness.state(), vi), "row {vi} in the run");
    }
    assert!(!tagged(harness.state(), 4), "past the run stays clear");
    assert_eq!(
        harness.state().session.sel,
        0,
        "the cursor lands on the click"
    );
    // The sweep is one undo step: z takes it back, leaving only the
    // ctrl+click's tag (it was a step of its own).
    press(harness.state_mut(), "z");
    assert!(tagged(harness.state(), 2), "the earlier tag survives");
    for vi in [0, 1, 3] {
        assert!(!tagged(harness.state(), vi), "row {vi} untagged by undo");
    }
}

/// The real fix for the stderr logs (asked 2026-09-01): the window
/// is up before the mailbox is - the boot frame draws with no
/// session at all, shows the opening progress, and becomes rmut the
/// moment the session lands from its thread.
#[test]
fn the_window_opens_before_the_session() {
    let (tx, rx) = std::sync::mpsc::channel();
    let boot = crate::Boot::Opening {
        spec: "inbox".into(),
        rx,
        last: String::new(),
        plan: crate::Plan {
            read_only: false,
            commands: Vec::new(),
            postponed: false,
            folders: false,
            config_warning: None,
        },
        failed: Default::default(),
        canvas: (eframe::egui::Color32::BLACK, eframe::egui::Color32::WHITE),
    };
    let mut harness =
        egui_kittest::Harness::new_ui_state(|ui, boot: &mut crate::Boot| boot.frame(ui), boot);
    // The spinner keeps asking for frames, so step rather than run.
    harness.run_steps(2);
    assert!(matches!(harness.state(), crate::Boot::Opening { .. }));
    tx.send(crate::OpenEvent::Progress("connecting".into()))
        .unwrap();
    harness.run_steps(2);
    let crate::Boot::Opening { last, .. } = harness.state() else {
        panic!("still opening");
    };
    assert_eq!(last, "connecting", "progress shows in the window");
    // The session lands; the window becomes rmut.
    let dir = tempfile::tempdir().unwrap();
    for sub in ["cur", "new", "tmp"] {
        fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    write_message(dir.path(), 0, "one");
    let (session, warnings) = Session::open(dir.path(), Config::default()).unwrap();
    tx.send(crate::OpenEvent::Done(Box::new(Ok((session, warnings)))))
        .unwrap();
    harness.run_steps(2);
    harness.run();
    let crate::Boot::Ready(gui) = harness.state() else {
        panic!("the session's arrival made the window rmut");
    };
    assert_eq!(gui.session.visible.len(), 1);
}
