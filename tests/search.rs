//! A whole path typed (or pasted) into the search field finds that item,
//! wherever the search looks, and Enter goes there like for any result.

mod common;

use common::*;
use fractale::model::entry::Kind;
use fractale::model::search::typed_path;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::Duration;

/// Type `text` into the search field and wait for the search to finish.
fn search(h: &mut H, text: &str) -> Vec<(PathBuf, Kind)> {
    h.state_mut().topbar.search = text.to_string();
    until(h, Duration::from_secs(10), text, |w| w.explorer.search.as_ref().is_some_and(|v| v.text == text && v.done));
    h.state().explorer.search.as_ref().unwrap().hits.iter().map(|h| (h.path.clone(), h.kind)).collect()
}

fn enter(h: &mut H, modifiers: egui::Modifiers) {
    h.key_press_modifiers(modifiers, egui::Key::Enter);
    h.run_steps(2);
}

#[test]
fn a_typed_path_finds_that_item() {
    let s = Scratch::new("search-path");
    let here = s.dir("here");
    let file = s.file("elsewhere/deep/bar report.txt", 3);
    let folder = s.dir("elsewhere/deep/sub");
    s.file("elsewhere/moved/gone.txt", 3);
    let mut h = window(&here);
    listed(&mut h);

    // A file outside the open folder, though the search looks only here.
    let text = file.display().to_string();
    assert_eq!(search(&mut h, &text), vec![(file.clone(), Kind::File)]);
    // Enter opens it with its app, as for any file result.
    enter(&mut h, egui::Modifiers::NONE);
    assert_eq!(h.state().opened, vec![file.clone()]);
    assert!(h.state().explorer.search.is_none());

    // Pasted with quotes and a trailing newline; Alt+Enter: its folder,
    // with it selected.
    search(&mut h, &format!("'{}'\n", file.display()));
    enter(&mut h, egui::Modifiers::ALT);
    until(&mut h, Duration::from_secs(10), "the file's folder", |w| w.explorer.is_current() && w.explorer.folder == file.parent().unwrap() && w.explorer.selection.is_selected(OsStr::new("bar report.txt")));

    // A folder, with a trailing slash and a `..` in it: Enter opens it.
    let text = format!("{}/sub/../sub/", file.parent().unwrap().display());
    assert_eq!(search(&mut h, &text), vec![(folder.clone(), Kind::Dir)]);
    enter(&mut h, egui::Modifiers::NONE);
    until(&mut h, Duration::from_secs(10), "the folder", |w| w.explorer.is_current() && w.explorer.folder == folder);

    // A URI.
    let uri = format!("file://{}", file.display()).replace(' ', "%20");
    assert_eq!(search(&mut h, &uri), vec![(file.clone(), Kind::File)]);

    // Gone: its name, under what's left of the path.
    let text = s.0.join("elsewhere/old/gone.txt").display().to_string();
    assert_eq!(search(&mut h, &text), vec![(s.0.join("elsewhere/moved/gone.txt"), Kind::File)]);

    // `~/`: from the home folder. HOME is read per search, and this is the
    // only test in this binary.
    std::env::set_var("HOME", s.0.join("elsewhere"));
    assert_eq!(typed_path("~/deep/sub"), Some(folder.clone()));
    assert_eq!(search(&mut h, "~/deep/bar report.txt"), vec![(file.clone(), Kind::File)]);

    // Not paths: searched as names.
    assert_eq!(typed_path("bar"), None);
    assert_eq!(typed_path("~bar"), None);
}
