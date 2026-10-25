//! Looking into a subfolder from its row: its files in the files pane, the
//! folder rows staying put.

mod common;

use common::*;
use egui::Key;
use egui_kittest::kittest::Queryable;
use fractale::model::view::Pane;
use std::ffi::OsStr;
use std::time::Duration;

fn files(h: &H) -> Vec<String> {
    let ex = &h.state().explorer;
    ex.files.iter().map(|&i| ex.file_entry(i).name.to_string_lossy().into_owned()).collect()
}

#[test]
fn peek_shows_a_subfolders_files_without_opening_it() {
    let s = Scratch::new("peek");
    s.file("alpha/one.txt", 10);
    s.file("alpha/two.txt", 10);
    s.dir("beta");
    s.file("top.txt", 10);
    let mut h = window(&s.0);
    listed(&mut h);
    assert_eq!(files(&h), ["top.txt"]);

    // The eye on alpha's row.
    h.get_by_label("alpha").hover();
    h.run_steps(2);
    h.get_by_label("peek alpha").click();
    until(&mut h, Duration::from_secs(5), "alpha's files", |w| w.explorer.files.len() == 2);
    assert_eq!(files(&h), ["one.txt", "two.txt"]);
    assert_eq!(h.state().explorer.folder, s.0, "still in the same folder");
    assert!(h.state().explorer.folders.len() == 2, "folder rows unchanged");
    if std::env::var_os("FRACTALE_SHOTS").is_some() {
        h.get_by_label("alpha").hover();
        h.run_steps(2);
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/out");
        std::fs::create_dir_all(&out).unwrap();
        h.render().unwrap().save(out.join("peek.png")).unwrap();
    }

    // Working on peeked files: renaming happens in alpha.
    h.get_by_label("one.txt").click();
    h.run_steps(1);
    h.key_press(Key::F2);
    h.run_steps(2);
    h.event(egui::Event::Text("first".into()));
    h.key_press(Key::Enter);
    until(&mut h, Duration::from_secs(5), "the rename", |w| w.explorer.find_file(OsStr::new("first.txt")).is_some());
    assert!(s.0.join("alpha/first.txt").exists() && !s.0.join("top.txt").with_file_name("first.txt").exists());

    // A folder row click still opens it; Escape closes the peek first.
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().explorer.peek.is_none());
    assert_eq!(files(&h), ["top.txt"]);

    // Space peeks the folder under the cursor.
    h.state_mut().explorer.selection.only(Pane::Folders, OsStr::new("beta"));
    h.state_mut().focus = Pane::Folders;
    h.key_press(Key::Space);
    until(&mut h, Duration::from_secs(5), "beta peeked", |w| w.explorer.peek.as_ref().is_some_and(|p| p.listing.is_some()));
    assert!(files(&h).is_empty());

    // "Open" goes there for real.
    h.run_steps(2);
    h.get_by_label("Open").click();
    until(&mut h, Duration::from_secs(5), "beta opened", |w| w.explorer.is_current() && w.explorer.folder.ends_with("beta"));
    assert!(h.state().explorer.peek.is_none());
}

/// Regression: navigating while peeking a folder with more files than the
/// open one left the files pane indexing the wrong listing (a panic).
#[test]
fn navigating_while_peeking_a_big_folder() {
    let s = Scratch::new("peek-nav");
    for i in 0..40 {
        s.file(&format!("many/f{i:02}.txt"), 10);
    }
    s.file("few/a.txt", 10);
    s.file("top.txt", 10);
    let mut h = window(&s.0);
    listed(&mut h);
    h.state_mut().explorer.peek(s.0.join("many"));
    until(&mut h, Duration::from_secs(5), "the peek", |w| w.explorer.files.len() == 40);
    // Open another folder (a row click does this), and draw frames.
    h.state_mut().explorer.open(s.0.join("few"));
    h.run_steps(3);
    until(&mut h, Duration::from_secs(5), "few", |w| w.explorer.is_current() && w.explorer.files.len() == 1);
    assert_eq!(files(&h), ["a.txt"]);
    // Peek again and go back.
    h.state_mut().explorer.back();
    h.run_steps(3);
    h.state_mut().explorer.peek(s.0.join("many"));
    until(&mut h, Duration::from_secs(5), "the peek again", |w| w.explorer.files.len() == 40);
    h.state_mut().explorer.up();
    h.run_steps(5);
}
