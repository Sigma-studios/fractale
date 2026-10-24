//! M2: folder sizes without répertoire.

mod common;

use common::*;
use fractale::model::fallback::Fallback;
use fractale::model::sizes::{self, FolderSizes, SizeMsg};
use fractale::ui::folders::{rows, RowKind};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// `du`'s total for a folder, in bytes (allocated, hard links once).
fn du(path: &Path) -> u64 {
    let (flag, unit) = if cfg!(target_os = "linux") { ("--block-size=1", 1) } else { ("-k", 1024) };
    let out = Command::new("du").args(["-s", flag]).arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().parse::<u64>().unwrap() * unit
}

/// Collect a fallback's messages for `folder` until it says it's complete.
fn sized(fb: &mut Fallback, folder: &Path, timeout: Duration) -> (FolderSizes, Vec<SizeMsg>) {
    let mut folders = HashMap::new();
    let mut seen = Vec::new();
    let start = Instant::now();
    loop {
        while let Ok(m) = fb.rx.try_recv() {
            sizes::apply(&mut folders, &m);
            seen.push(m);
        }
        if let Some(f) = folders.get(folder).filter(|f: &&FolderSizes| f.complete) {
            return (f.clone(), seen);
        }
        assert!(start.elapsed() < timeout, "{} not sized in time", folder.display());
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tree(s: &Scratch) {
    s.file("big/a.bin", 3_000_000);
    s.file("big/inner/b.bin", 1_000_000);
    s.file("small/c.txt", 1000);
    s.dir("empty");
    s.file("top.txt", 20_000);
    // A sparse file counts what it allocates.
    let f = std::fs::File::create(s.0.join("big/sparse.img")).unwrap();
    f.set_len(1 << 30).unwrap();
    // Hard links: inside one folder (counted once there).
    s.file("links/orig.bin", 500_000);
    std::fs::hard_link(s.0.join("links/orig.bin"), s.0.join("links/again.bin")).unwrap();
    // Symlinks aren't followed.
    std::os::unix::fs::symlink(s.0.join("big"), s.0.join("small/to-big")).unwrap();
}

#[test]
fn sizes_match_du() {
    let s = Scratch::new("du");
    tree(&s);
    let mut fb = Fallback::spawn(Arc::new(|| {}));
    fb.watch(&s.0, Vec::new(), Vec::new());
    let (f, _) = sized(&mut fb, &s.0, Duration::from_secs(20));
    for name in ["big", "small", "empty", "links"] {
        let got = f.children[OsStr::new(name)].total;
        assert_eq!(got, du(&s.0.join(name)), "{name}");
    }
    assert_eq!(f.total, du(&s.0), "folder total");
    assert_eq!(f.total, f.files_here + f.children_total(), "parts add up");
}

#[test]
fn hard_links_across_subfolders_count_once() {
    let s = Scratch::new("links");
    s.file("a/orig.bin", 2_000_000);
    s.dir("b");
    std::fs::hard_link(s.0.join("a/orig.bin"), s.0.join("b/link.bin")).unwrap();
    let mut fb = Fallback::spawn(Arc::new(|| {}));
    fb.watch(&s.0, Vec::new(), Vec::new());
    let (f, _) = sized(&mut fb, &s.0, Duration::from_secs(20));
    assert_eq!(f.total, du(&s.0));
    let (a, b) = (f.children[OsStr::new("a")].total, f.children[OsStr::new("b")].total);
    assert!(a.min(b) < 1_000_000, "the 2 MB file is charged to one folder only: {a} / {b}");
}

#[test]
fn results_stream_and_navigation_cancels() {
    let s = Scratch::new("stream");
    for d in 0..30 {
        for f in 0..300 {
            s.file(&format!("d{d:02}/f{f}"), 100);
        }
    }
    s.file("other/x", 10);
    let mut fb = Fallback::spawn(Arc::new(|| {}));
    fb.watch(&s.0, Vec::new(), Vec::new());
    // Subfolder sizes come one by one, before the folder is complete.
    let start = Instant::now();
    let first_child = loop {
        if let Ok(m @ SizeMsg::Child { .. }) = fb.rx.try_recv() {
            break m;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(first_child, SizeMsg::Child { .. }));
    // Leaving for another folder stops the walk and starts on it.
    let other = s.0.join("other");
    let t = Instant::now();
    fb.watch(&other, Vec::new(), Vec::new());
    let (f, msgs) = sized(&mut fb, &other, Duration::from_secs(10));
    eprintln!("switched folders in {:?}", t.elapsed());
    assert!(t.elapsed() < Duration::from_secs(2), "the old walk kept the worker busy");
    assert_eq!(f.total, du(&other));
    let stale = msgs.iter().filter(|m| matches!(m, SizeMsg::Done { folder, complete: true, .. } if *folder == s.0)).count();
    assert_eq!(stale, 0, "the cancelled folder was never reported complete");
}

#[test]
fn bars_and_extra_rows_add_up_in_the_window() {
    let s = Scratch::new("rows");
    tree(&s);
    let mut h = window(&s.0);
    listed(&mut h);
    until(&mut h, Duration::from_secs(20), "sizes", |w| w.explorer.folder_sizes().is_some_and(|f| f.complete));
    h.run_steps(2);
    let w = h.state();
    let total = w.explorer.folder_sizes().unwrap().total;
    let rows = rows(w);
    let sum: u64 = rows.iter().filter(|r| r.depth == 0).filter_map(|r| r.bytes()).sum();
    assert_eq!(sum, total, "top-level rows add up to the folder's total");
    assert!(rows.iter().any(|r| matches!(r.kind, RowKind::FilesHere(_))));
    // Every bar is a share of that total.
    assert!(rows.iter().filter(|r| r.depth == 0).all(|r| r.of == Some(total) || r.bytes().is_none()));
    // Sorted by size: largest first.
    let first = rows.iter().find_map(|r| match &r.kind {
        RowKind::Folder { name, .. } => Some(name.clone()),
        _ => None,
    });
    assert_eq!(first.as_deref(), Some(OsStr::new("big")));
}

#[test]
fn expanding_a_row_answers_from_the_tree() {
    let s = Scratch::new("expand");
    tree(&s);
    let mut h = window(&s.0);
    listed(&mut h);
    until(&mut h, Duration::from_secs(20), "sizes", |w| w.explorer.folder_sizes().is_some_and(|f| f.complete));
    h.state_mut().explorer.toggle_expanded(s.0.join("big"));
    let took = until(&mut h, Duration::from_secs(5), "the expanded row's sizes", |w| w.explorer.sizes.contains_key(&s.0.join("big")));
    eprintln!("expansion answered in {took:?}");
    h.run_steps(2);
    let rows = rows(h.state());
    let inner = rows.iter().find(|r| matches!(&r.kind, RowKind::Folder { name, top: false, .. } if name == "inner")).expect("nested row");
    assert_eq!(inner.depth, 1);
    let big_total = h.state().explorer.folder_sizes().unwrap().children[OsStr::new("big")].total;
    assert_eq!(inner.of, Some(big_total), "nested bars are a share of their parent row");
    // Nested rows plus their files-here row add up to the parent.
    let nested: u64 = rows.iter().filter(|r| r.depth == 1).filter_map(|r| r.bytes()).sum();
    assert_eq!(nested, big_total);
    // Collapsing removes them.
    h.state_mut().explorer.toggle_expanded(s.0.join("big"));
    h.run_steps(2);
    assert!(rows_of(&h).iter().all(|r| r.depth == 0));
}

fn rows_of(h: &H) -> Vec<fractale::ui::folders::Row> {
    rows(h.state())
}

/// The fallback watches the open folder only: changes directly in it are
/// measured again by themselves; deeper changes on F5 (répertoire is what
/// makes deep changes live).
#[test]
fn changes_are_measured_again() {
    let s = Scratch::new("change");
    tree(&s);
    let mut h = window(&s.0);
    listed(&mut h);
    until(&mut h, Duration::from_secs(20), "sizes", |w| w.explorer.folder_sizes().is_some_and(|f| f.complete));
    let files_before = h.state().explorer.folder_sizes().unwrap().files_here;
    // A new file and a new folder right in the open folder.
    s.file("new-top.bin", 1_000_000);
    s.file("new-dir/inside.bin", 2_000_000);
    let took = until(&mut h, Duration::from_secs(5), "the new sizes", |w| {
        w.explorer.folder_sizes().is_some_and(|f| f.files_here > files_before + 900_000 && f.children.get(OsStr::new("new-dir")).is_some_and(|c| c.total >= 2_000_000))
    });
    eprintln!("changes in the open folder measured after {took:?}");
    // Deeper: shows after F5.
    let before = h.state().explorer.folder_sizes().unwrap().children[OsStr::new("small")].total;
    s.file("small/deeper.bin", 2_000_000);
    h.key_press(egui::Key::F5);
    until(&mut h, Duration::from_secs(10), "the refreshed size", |w| {
        w.explorer.folder_sizes().and_then(|f| f.children.get(OsStr::new("small"))).is_some_and(|c| c.total > before + 1_900_000)
    });
    until(&mut h, Duration::from_secs(10), "complete", |w| w.explorer.folder_sizes().is_some_and(|f| f.complete));
    assert_eq!(h.state().explorer.folder_sizes().unwrap().total, du(&s.0));
}
