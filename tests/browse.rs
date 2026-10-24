//! M1: browsing, driven through the UI.

mod common;

use common::*;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use fractale::model::view::Pane;
use std::ffi::OsStr;
use std::time::Duration;

fn sample() -> Scratch {
    let s = Scratch::new("browse");
    s.file("alpha/one.txt", 10);
    s.file("beta/two.txt", 20);
    s.file("beta/deeper/three.txt", 30);
    s.dir("gamma");
    s.file("file1.txt", 100);
    s.file("file2.txt", 200);
    s.file("file10.txt", 300);
    s.file(".hidden.txt", 5);
    s
}

#[test]
fn clicking_a_row_opens_it_and_history_works() {
    let s = sample();
    let mut h = window(&s.0);
    listed(&mut h);
    h.get_by_label("beta").click();
    h.run_steps(2);
    assert_eq!(h.state().explorer.folder, s.0.join("beta"));
    listed(&mut h);
    h.get_by_label("two.txt");
    h.get_by_label("deeper");

    // Backspace goes up, with the folder we came from under the cursor.
    h.key_press(Key::Backspace);
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0);
    assert!(h.state().explorer.selection.is_cursor(Pane::Folders, OsStr::new("beta")));

    // Alt+Left/Right walk the history.
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0.join("beta"));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowRight);
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0);

    // The breadcrumb opens a parent.
    h.get_by_label("beta").click();
    listed(&mut h);
    let parent_name = s.0.file_name().unwrap().to_string_lossy().into_owned();
    h.get_by_label(&format!("crumb {parent_name}")).click();
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0);
}

#[test]
fn natural_order_and_hidden_files() {
    let s = sample();
    let mut h = window(&s.0);
    listed(&mut h);
    let names: Vec<String> = h.state().explorer.order(Pane::Files).iter().map(|n| n.to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["file1.txt", "file2.txt", "file10.txt"]);
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    h.run_steps(2);
    assert_eq!(h.state().explorer.order(Pane::Files)[0], ".hidden.txt");
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    h.run_steps(2);
    assert_eq!(h.state().explorer.files.len(), 3);
}

#[test]
fn selection_with_mouse_and_keys() {
    let s = sample();
    let mut h = window(&s.0);
    listed(&mut h);
    h.get_by_label("file1.txt").click();
    h.run_steps(1);
    h.get_by_label("file10.txt").click_modifiers(Modifiers::SHIFT);
    h.run_steps(1);
    assert_eq!(h.state().explorer.selection.items.len(), 3, "shift-click selects the range");
    h.get_by_label("file2.txt").click_modifiers(Modifiers::COMMAND);
    h.run_steps(1);
    assert_eq!(h.state().explorer.selection.items.len(), 2, "ctrl-click toggles");
    h.key_press(Key::Escape);
    h.run_steps(1);
    assert!(h.state().explorer.selection.items.is_empty());

    // Arrows move in the focused pane; Tab switches panes.
    h.get_by_label("file1.txt").click();
    h.run_steps(1);
    h.key_press(Key::ArrowRight);
    h.run_steps(1);
    assert!(h.state().explorer.selection.is_cursor(Pane::Files, OsStr::new("file2.txt")));
    h.key_press_modifiers(Modifiers::SHIFT, Key::End);
    h.run_steps(1);
    assert_eq!(h.state().explorer.selection.items.len(), 2);
    h.key_press(Key::Tab);
    h.run_steps(1);
    assert_eq!(h.state().focus, Pane::Folders);
    h.key_press(Key::ArrowDown);
    h.run_steps(1);
    assert!(h.state().explorer.selection.is_cursor(Pane::Folders, OsStr::new("beta")), "{:?}", h.state().explorer.selection.cursor);
    // Enter opens the folder under the cursor.
    h.key_press(Key::Enter);
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0.join("beta"));
    h.key_press(Key::Backspace);
    listed(&mut h);

    // Typing jumps to a name; Enter on a file hands it to its app.
    h.key_press(Key::Tab);
    h.run_steps(1);
    h.event(egui::Event::Text("file1".into()));
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().opened, [s.0.join("file1.txt")]);
    // Ctrl+A selects everything (both panes).
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    assert_eq!(h.state().explorer.selection.items.len(), 6, "3 folders + 3 visible files");
}

#[test]
fn path_field_opens_typed_paths() {
    let s = sample();
    let mut h = window(&s.0);
    listed(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.run_steps(3);
    assert!(h.state().topbar.path_text.is_some());
    h.state_mut().topbar.path_text = Some(format!("{}/beta/deeper", s.0.display()));
    h.run_steps(1);
    h.key_press(Key::Enter);
    h.run_steps(2);
    listed(&mut h);
    assert_eq!(h.state().explorer.folder, s.0.join("beta/deeper"));
    assert!(h.state().topbar.path_text.is_none());
}

/// Files appearing and disappearing in the open folder show up within
/// 300 ms, and neither the selection nor the scroll position moves.
#[test]
fn live_updates_keep_selection_and_scroll() {
    let s = Scratch::new("live");
    for i in 0..3000 {
        s.file(&format!("f{i:05}.dat"), 10);
    }
    let mut h = window(&s.0);
    listed(&mut h);
    h.state_mut().explorer.layout = fractale::model::explorer::Layout::List;
    // Select an item far down and scroll it into view.
    h.state_mut().focus = Pane::Files;
    h.state_mut().explorer.selection.only(Pane::Files, OsStr::new("f02000.dat"));
    h.state_mut().explorer.reveal_cursor = true;
    h.run_steps(4);
    let offset_before = h.state().scroll[1].offset;
    assert!(offset_before > 1000.0, "scrolled down: {offset_before}");

    // Another process adds and removes files.
    std::thread::sleep(Duration::from_millis(400));
    let writer = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("cd '{}' && touch new-a.dat new-b.dat new-c.dat && rm f00000.dat f00001.dat", s.0.display()))
        .status()
        .unwrap();
    assert!(writer.success());
    let took = until(&mut h, Duration::from_secs(3), "the change", |w| w.explorer.find(OsStr::new("new-c.dat")).is_some() && w.explorer.find(OsStr::new("f00001.dat")).is_none());
    eprintln!("change visible after {took:?}");
    assert!(took < Duration::from_millis(300), "took {took:?}");
    h.run_steps(2);
    assert!(h.state().explorer.selection.is_cursor(Pane::Files, OsStr::new("f02000.dat")));
    assert_eq!(h.state().explorer.selection.items.len(), 1);
    let offset_after = h.state().scroll[1].offset;
    assert_eq!(offset_before, offset_after, "scroll position kept");
}

/// Opening a 10k-file folder never blocks a frame. Debug builds only
/// report; the 16 ms target is checked in release (`cargo test --release`).
#[test]
fn ten_thousand_files_open_without_stalling() {
    let s = Scratch::new("big");
    for i in 0..10_000 {
        s.file(&format!("file{i}.log"), (i * 37) % 9000);
    }
    let mut h = window(&s.0.join("..").canonicalize().unwrap());
    listed(&mut h);
    h.state_mut().stats.worst = Duration::ZERO;
    h.state_mut().explorer.open(s.0.clone());
    until(&mut h, Duration::from_secs(10), "10k listing", |w| w.explorer.is_current() && w.explorer.files.len() == 10_000);
    // A few frames scrolled through the grid too.
    for _ in 0..30 {
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -400.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel });
        h.step();
    }
    let worst = h.state().stats.worst;
    eprintln!("worst UI frame while opening and scrolling 10k files: {worst:?} ({} frames)", h.state().stats.frames);
    if !cfg!(debug_assertions) {
        assert!(worst < Duration::from_millis(16), "worst frame {worst:?}");
    }
}
