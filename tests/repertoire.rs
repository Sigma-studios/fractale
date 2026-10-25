//! M3: sizes and search from the répertoire daemon, and switching between
//! it and Fractale's own measuring.
//!
//! The daemon runs in a helper process (this test binary again, running the
//! ignored `daemon_helper`), so stopping it really closes its sockets.

mod common;

use common::*;
use fractale::model::explorer::Sources;
use fractale::model::fallback::Fallback;
use fractale::model::sizes::{self, SourceState};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Started by `Daemon::start` below; not a real test.
#[test]
#[ignore]
fn daemon_helper() {
    let (Some(root), Some(socket)) = (std::env::var_os("FRACTALE_TESTD_ROOT"), std::env::var_os("FRACTALE_TESTD_SOCKET")) else { return };
    let config = repertoire::Config {
        roots: vec![PathBuf::from(root)],
        state_dir: None,
        socket: Some(socket.into()),
        watch: std::env::var_os("FRACTALE_TESTD_WATCH").is_some(),
        quiet: Duration::from_millis(200),
        max_delay: Duration::from_secs(1),
        ..Default::default()
    };
    let _d = repertoire::Daemon::start(config).expect("daemon");
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Daemon {
    fn start(root: &Path, socket: &Path, watch: bool) -> Daemon {
        let _ = std::fs::remove_file(socket);
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "daemon_helper", "--ignored", "--nocapture", "--test-threads=1"]).env("FRACTALE_TESTD_ROOT", root).env("FRACTALE_TESTD_SOCKET", socket);
        if watch {
            cmd.env("FRACTALE_TESTD_WATCH", "1");
        }
        let child = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let d = Daemon { child, socket: socket.to_path_buf() };
        // Wait until it answers and has scanned.
        let start = Instant::now();
        loop {
            if let Ok(mut c) = repertoire_proto::Client::connect_to(socket) {
                if c.filesystems().is_ok_and(|f| f.iter().all(|f| matches!(f.state, repertoire_proto::FsState::Live | repertoire_proto::FsState::Static))) {
                    break;
                }
            }
            assert!(start.elapsed() < Duration::from_secs(20), "daemon didn't come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        d
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A socket path short enough for macOS (104 bytes).
fn socket(tag: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/fr-{tag}-{}.sock", std::process::id()))
}

fn tree(s: &Scratch) {
    s.file("big/a.bin", 3_000_000);
    s.file("big/inner/b.bin", 1_000_000);
    s.file("big/inner/deeper/c.bin", 500_000);
    s.file("small/c.txt", 1000);
    s.file("small/steam-notes.txt", 100);
    s.dir("empty");
    s.dir("steamapps/common/Hades");
    s.file("steamapps/common/Hades/data.pak", 2_000_000);
    s.file("top.txt", 20_000);
    s.file("links/orig.bin", 500_000);
    std::fs::hard_link(s.0.join("links/orig.bin"), s.0.join("links/again.bin")).unwrap();
}

fn sources(socket: &Path) -> Sources {
    Sources { socket: socket.to_path_buf(), retry: Duration::from_millis(300), thumbnails: common::thumbnails() }
}

fn fallback_sizes(folder: &Path) -> fractale::model::sizes::FolderSizes {
    let mut fb = Fallback::spawn(Arc::new(|| {}));
    fb.watch(folder, Vec::new(), Vec::new());
    let mut map = HashMap::new();
    let start = Instant::now();
    loop {
        while let Ok(m) = fb.rx.try_recv() {
            sizes::apply(&mut map, &m);
        }
        if let Some(f) = map.get(folder).filter(|f: &&fractale::model::sizes::FolderSizes| f.complete) {
            return f.clone();
        }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn sizes_from_the_daemon_match_our_own() {
    let s = Scratch::new("rep-sizes");
    tree(&s);
    let sock = socket("sizes");
    let _d = Daemon::start(&s.0, &sock, false);
    let mut h = window_with(&s.0, sources(&sock));
    listed(&mut h);
    until(&mut h, Duration::from_secs(10), "répertoire sizes", |w| w.explorer.uses_daemon() && w.explorer.folder_sizes().is_some_and(|f| f.complete) && matches!(w.explorer.source, SourceState::Repertoire(_)));
    let ours = fallback_sizes(&s.0);
    let theirs = h.state().explorer.folder_sizes().unwrap().clone();
    assert_eq!(theirs.total, ours.total, "folder total");
    assert_eq!(theirs.files_here, ours.files_here, "files here");
    for (name, c) in &ours.children {
        assert_eq!(theirs.children[name].total, c.total, "{name:?}");
        assert_eq!(theirs.children[name].files, c.files, "{name:?} files");
    }
    // Expanded rows come from the daemon too.
    h.state_mut().explorer.toggle_expanded(s.0.join("big"));
    until(&mut h, Duration::from_secs(5), "expanded row", |w| w.explorer.sizes.contains_key(&s.0.join("big")));
    let inner = &h.state().explorer.sizes[&s.0.join("big")].children[OsStr::new("inner")];
    assert_eq!(inner.total, fallback_sizes(&s.0.join("big")).children[OsStr::new("inner")].total);

    // Biggest folders here.
    h.state_mut().explorer.request_top();
    until(&mut h, Duration::from_secs(5), "top", |w| w.explorer.top.is_some());
    let top = h.state().explorer.top.clone().unwrap().1;
    // Largest first, never one inside another, wrappers unwrapped.
    assert!(top.windows(2).all(|w| w[0].1 >= w[1].1), "{top:?}");
    assert!(top.iter().all(|(a, _)| top.iter().all(|(b, _)| a == b || !b.starts_with(a))), "nested: {top:?}");
    assert!(top.iter().any(|(p, _)| *p == s.0.join("steamapps/common/Hades")), "wrappers are unwrapped: {top:?}");
}

#[test]
fn switching_sources_has_no_gap() {
    let s = Scratch::new("rep-switch");
    tree(&s);
    let sock = socket("switch");
    let mut d = Daemon::start(&s.0, &sock, false);
    let mut h = window_with(&s.0, sources(&sock));
    listed(&mut h);
    until(&mut h, Duration::from_secs(10), "daemon", |w| w.explorer.uses_daemon() && w.explorer.folder_sizes().is_some_and(|f| f.complete));
    let total = h.state().explorer.folder_sizes().unwrap().total;

    // Stop the daemon: our own measuring takes over, numbers stay on screen.
    d.stop();
    let start = Instant::now();
    loop {
        h.step();
        let w = h.state();
        let f = w.explorer.folder_sizes().expect("sizes never disappear");
        assert!(f.total > 0 && f.children.contains_key(OsStr::new("big")), "a gap: {f:?}");
        if !w.explorer.daemon_up && matches!(w.explorer.source, SourceState::OwnScan { .. }) && f.complete {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "didn't fall back");
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!("fell back to our own sizes after {:?}", start.elapsed());
    assert_eq!(h.state().explorer.folder_sizes().unwrap().total, total);

    // Start it again: back on répertoire within the retry period.
    let _d2 = Daemon::start(&s.0, &sock, false);
    let took = until(&mut h, Duration::from_secs(10), "daemon again", |w| w.explorer.uses_daemon() && matches!(w.explorer.source, SourceState::Repertoire(_)));
    eprintln!("back on répertoire after {took:?} (retry every 300 ms here, 60 s by default)");
}

#[test]
fn search_matches_the_daemon_with_and_without_it() {
    let s = Scratch::new("rep-search");
    tree(&s);
    let sock = socket("search");
    let d = Daemon::start(&s.0, &sock, false);
    let mut h = window_with(&s.0, sources(&sock));
    listed(&mut h);
    until(&mut h, Duration::from_secs(10), "daemon", |w| w.explorer.daemon_up);
    let mut c = repertoire_proto::Client::connect_to(&sock).unwrap();
    for q in ["steam", "bin", "c", "inner", "hades"] {
        let expected: Vec<PathBuf> = c.search(q, Some(&s.0), 500, &[]).unwrap().into_iter().map(|h| h.path.to_path()).collect();
        h.state_mut().explorer.set_search(q, false);
        until(&mut h, Duration::from_secs(5), "results", |w| w.explorer.search.as_ref().is_some_and(|v| v.done));
        let got: Vec<PathBuf> = h.state().explorer.search.as_ref().unwrap().hits.iter().map(|h| h.path.clone()).collect();
        assert_eq!(got, expected, "query {q:?} through répertoire");
        assert!(h.state().explorer.search.as_ref().unwrap().indexed);
    }
    // Without the daemon: our own walk finds the same things, ranked the
    // same (folder sizes aren't known, so compare as sets per rank).
    drop(d);
    until(&mut h, Duration::from_secs(10), "fallback", |w| !w.explorer.daemon_up);
    for q in ["steam", "bin", "c", "inner", "hades"] {
        h.state_mut().explorer.set_search(q, false);
        until(&mut h, Duration::from_secs(5), "walk results", |w| w.explorer.search.as_ref().is_some_and(|v| v.done));
        let v = h.state().explorer.search.clone().unwrap();
        assert!(!v.indexed && !v.partial);
        let mut got: Vec<PathBuf> = v.hits.iter().map(|h| h.path.clone()).collect();
        got.sort();
        let mut want = walk_expected(&s.0, q);
        want.sort();
        assert_eq!(got, want, "query {q:?} without répertoire");
    }
}

/// Every name under `root` that matches `q` the way répertoire ranks it.
fn walk_expected(root: &Path, q: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
            use std::os::unix::ffi::OsStrExt;
            if repertoire_core::query::rank(q, e.file_name().as_bytes()).is_some() {
                out.push(e.path());
            }
        }
    }
    out
}

/// Linux as root: the live daemon (fanotify). Writing 1 GB into a subfolder
/// updates its row within 2 seconds.
#[test]
fn live_updates_from_the_daemon() {
    // SAFETY: geteuid can't fail.
    if !cfg!(target_os = "linux") || unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: needs Linux and root (fanotify)");
        return;
    }
    let base = PathBuf::from(format!("/var/tmp/fractale-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("sub")).unwrap();
    std::fs::write(base.join("sub/small"), b"x").unwrap();
    let sock = socket("live");
    let _d = Daemon::start(&base, &sock, true);
    let mut h = window_with(&base, sources(&sock));
    listed(&mut h);
    until(&mut h, Duration::from_secs(10), "live sizes", |w| w.explorer.uses_daemon() && w.explorer.folder_sizes().is_some_and(|f| f.children.contains_key(OsStr::new("sub"))));
    assert!(matches!(h.state().explorer.source, SourceState::Repertoire(fractale::model::sizes::RepertoireState::Live)), "{:?}", h.state().explorer.source);
    let writer = std::thread::spawn({
        let p = base.join("sub/big.bin");
        move || {
            use std::io::Write;
            let mut f = std::fs::File::create(p).unwrap();
            let chunk = vec![7u8; 1 << 20];
            for _ in 0..1024 {
                f.write_all(&chunk).unwrap();
            }
            f.sync_all().unwrap();
            Instant::now()
        }
    });
    let written = writer.join().unwrap();
    until(&mut h, Duration::from_secs(10), "the new size", |w| {
        w.explorer.folder_sizes().and_then(|f| f.children.get(OsStr::new("sub"))).is_some_and(|c| c.total >= 1 << 30)
    });
    let took = written.elapsed();
    eprintln!("1 GB write visible in its row {took:?} after the write finished");
    assert!(took < Duration::from_secs(2), "{took:?}");
    let _ = std::fs::remove_dir_all(&base);
}
