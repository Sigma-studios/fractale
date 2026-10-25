//! File operations from the window: shortcuts, rename in place, the
//! dialogs, and dragging items onto folder rows.

mod common;

use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use fractale::model::view::Pane;
use std::time::Duration;

fn wait_for(h: &mut H, what: &str, cond: impl Fn(&fractale::ui::Window) -> bool) {
    until(h, Duration::from_secs(10), what, cond);
}

fn selected(h: &H, name: &str) -> bool {
    h.state().explorer.selection.is_selected(std::ffi::OsStr::new(name))
}

#[test]
fn rename_in_place() {
    let s = Scratch::new("ui-rename");
    s.file("notes.txt", 10);
    let mut h = window(&s.0);
    listed(&mut h);
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.key_press(Key::F2);
    h.run_steps(2);
    // The name without its extension is selected: typing replaces it.
    h.event(egui::Event::Text("ideas".into()));
    h.run_steps(1);
    h.key_press(Key::Enter);
    wait_for(&mut h, "the rename", |w| w.explorer.find(std::ffi::OsStr::new("ideas.txt")).is_some());
    assert!(s.0.join("ideas.txt").exists() && !s.0.join("notes.txt").exists());
    wait_for(&mut h, "the selection", |w| w.explorer.selection.is_selected(std::ffi::OsStr::new("ideas.txt")));

    // Escape keeps the old name.
    h.key_press(Key::F2);
    h.run_steps(2);
    h.event(egui::Event::Text("nope".into()));
    h.key_press(Key::Escape);
    h.run_steps(5);
    assert!(h.state().ops.rename.is_none());
    assert!(s.0.join("ideas.txt").exists());
}

#[test]
fn new_folder_starts_renamed() {
    let s = Scratch::new("ui-newfolder");
    s.file("a.txt", 1);
    let mut h = window(&s.0);
    listed(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::N);
    wait_for(&mut h, "the new folder in rename", |w| w.ops.rename.is_some());
    assert!(s.0.join("New folder").is_dir());
    h.run_steps(2);
    h.event(egui::Event::Text("Photos".into()));
    h.key_press(Key::Enter);
    wait_for(&mut h, "the renamed folder", |w| w.explorer.find(std::ffi::OsStr::new("Photos")).is_some());
    assert!(s.0.join("Photos").is_dir() && !s.0.join("New folder").exists());
}

#[test]
fn copy_paste_and_conflicts() {
    let s = Scratch::new("ui-paste");
    s.file("a.txt", 5);
    s.file("b.txt", 5);
    let dest = s.dir("dest");
    std::fs::write(dest.join("a.txt"), "older").unwrap();
    let mut h = window(&s.0);
    listed(&mut h);
    h.get_by_label("a.txt").click();
    h.get_by_label("b.txt").click_modifiers(Modifiers::COMMAND);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::C);
    h.run_steps(1);
    assert_eq!(h.state().explorer.clipboard.as_ref().unwrap().paths.len(), 2);
    h.get_by_label("dest").click();
    wait_for(&mut h, "dest", |w| w.explorer.is_current() && w.explorer.folder.ends_with("dest"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    // a.txt is there already: the dialog asks.
    wait_for(&mut h, "the conflict dialog", |w| w.explorer.jobs.views().iter().any(|j| j.question.is_some()));
    h.run_steps(2);
    h.get_by_label("Keep both").click();
    wait_for(&mut h, "the paste", |w| !w.explorer.jobs.busy() && w.explorer.find(std::ffi::OsStr::new("b.txt")).is_some() && w.explorer.find(std::ffi::OsStr::new("a (2).txt")).is_some());
    assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "older");
    assert!(s.0.join("a.txt").exists(), "copied, not moved");
    wait_for(&mut h, "pasted items selected", |w| w.explorer.selection.items.len() == 2);
    assert!(selected(&h, "b.txt") && selected(&h, "a (2).txt"));
    assert!(h.state().explorer.jobs.views().is_empty(), "a clean job leaves the strip");

    // Cut then paste moves, and empties the clipboard.
    h.get_by_label("b.txt").click();
    h.key_press_modifiers(Modifiers::COMMAND, Key::X);
    h.key_press(Key::Backspace);
    wait_for(&mut h, "parent", |w| w.explorer.is_current() && !w.explorer.folder.ends_with("dest"));
    std::fs::remove_file(s.0.join("b.txt")).unwrap();
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    wait_for(&mut h, "the move", |w| !w.explorer.jobs.busy() && w.explorer.find(std::ffi::OsStr::new("b.txt")).is_some());
    assert!(!dest.join("b.txt").exists());
    assert!(h.state().explorer.clipboard.is_none());
}

#[test]
fn delete_permanently_asks_with_count_and_size() {
    let s = Scratch::new("ui-delete");
    s.file("keep.txt", 1);
    s.file("junk/one.bin", 50_000);
    s.file("junk/two.bin", 50_000);
    let mut h = window(&s.0);
    listed(&mut h);
    h.state_mut().explorer.selection.only(Pane::Folders, std::ffi::OsStr::new("junk"));
    h.key_press_modifiers(Modifiers::SHIFT, Key::Delete);
    wait_for(&mut h, "the count", |w| w.explorer.counted.is_some());
    h.run_steps(2);
    // 3 items: the folder and its two files.
    h.get_by_label_contains("3 files and folders");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(s.0.join("junk").exists());
    h.key_press_modifiers(Modifiers::SHIFT, Key::Delete);
    wait_for(&mut h, "the count", |w| w.explorer.counted.is_some());
    h.run_steps(2);
    h.get_by_label("Delete").click();
    wait_for(&mut h, "the delete", |w| !w.explorer.jobs.busy() && w.explorer.find(std::ffi::OsStr::new("junk")).is_none());
    assert!(!s.0.join("junk").exists() && s.0.join("keep.txt").exists());
}

#[test]
fn drag_a_file_onto_a_folder_row() {
    let s = Scratch::new("ui-drag");
    s.file("photo.jpg", 100);
    s.dir("album");
    let mut h = window(&s.0);
    listed(&mut h);
    let from = h.get_by_label("photo.jpg").rect().center();
    let to = h.get_by_label("album").rect().center();
    h.event(egui::Event::PointerMoved(from));
    h.run_steps(1);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for k in 1..=10 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
        h.run_steps(1);
    }
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
    wait_for(&mut h, "the move", |w| !w.explorer.jobs.busy() && w.explorer.find(std::ffi::OsStr::new("photo.jpg")).is_none());
    assert!(s.0.join("album/photo.jpg").exists(), "moved into the folder");
    // It didn't open the folder (a drag isn't a click).
    assert!(!h.state().explorer.folder.ends_with("album"));
}

/// Pictures of the menus and dialogs, to look at
/// (`FRACTALE_SHOTS=1`, written to target/out/).
#[test]
fn pictures() {
    if std::env::var_os("FRACTALE_SHOTS").is_none() {
        return;
    }
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
    std::fs::create_dir_all(&out).unwrap();
    let s = Scratch::new("ui-shots");
    s.file("a.txt", 5_000);
    s.file("dest/a.txt", 9_000);
    let mut h = window(&s.0);
    listed(&mut h);
    h.get_by_label("a.txt").click_secondary();
    h.run_steps(3);
    h.render().unwrap().save(out.join("menu-item.png")).unwrap();
    h.key_press(Key::Escape);
    h.run_steps(2);
    h.get_by_label("a.txt").click();
    h.key_press_modifiers(Modifiers::COMMAND, Key::C);
    h.get_by_label("dest").click();
    wait_for(&mut h, "dest", |w| w.explorer.is_current() && w.explorer.folder.ends_with("dest"));
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    wait_for(&mut h, "the conflict dialog", |w| w.explorer.jobs.views().iter().any(|j| j.question.is_some()));
    h.run_steps(3);
    h.render().unwrap().save(out.join("conflict.png")).unwrap();
}
